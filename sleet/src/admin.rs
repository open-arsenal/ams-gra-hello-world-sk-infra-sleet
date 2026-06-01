//! Admin HTTP server exposing `/health`, `/ready`, `/status`, and `/metrics`.

use std::convert::Infallible;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::http::StatusCode;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Shared health metadata read by the admin endpoints.
#[derive(Debug)]
pub struct AdminState {
    /// Process start time used for uptime calculations.
    pub start_time: Instant,
    /// Schema version loaded at startup.
    pub schema_version: String,
    /// Current number of active WebSocket connections.
    pub active_connections: Arc<AtomicU64>,
    /// Readiness state toggled by the server lifecycle.
    pub ready: Arc<AtomicBool>,
}

/// Errors returned by the admin listener.
#[derive(Debug)]
pub enum AdminError {
    /// TCP bind failed for the configured admin listener.
    Bind(std::io::Error),
}

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(source) => write!(f, "admin bind failed: {source}"),
        }
    }
}

impl std::error::Error for AdminError {}

/// Run the admin HTTP listener until shutdown is requested.
pub async fn run(
    bind_addr: SocketAddr,
    state: Arc<AdminState>,
    metrics_handle: PrometheusHandle,
    shutdown: CancellationToken,
) -> Result<(), AdminError> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(AdminError::Bind)?;
    info!(addr = %bind_addr, "admin listener started");

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                info!(addr = %bind_addr, "admin listener shutdown requested");
                break;
            }
            accept_result = listener.accept() => {
                let Ok((stream, peer)) = accept_result else {
                    warn!("admin listener accept failed");
                    continue;
                };

                let state = Arc::clone(&state);
                let metrics_handle = metrics_handle.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |request| {
                        let state = Arc::clone(&state);
                        let metrics_handle = metrics_handle.clone();
                        async move {
                            Ok::<_, Infallible>(route(&request, state.as_ref(), &metrics_handle))
                        }
                    });

                    if let Err(error) = http1::Builder::new().serve_connection(io, service).await {
                        error!(peer = %peer, "admin request error: {error}");
                    }
                });
            }
        }
    }

    Ok(())
}

fn route(
    request: &Request<Incoming>,
    state: &AdminState,
    metrics_handle: &PrometheusHandle,
) -> Response<Full<Bytes>> {
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/health") => health_response(state),
        (&Method::GET, "/ready") => ready_response(state),
        (&Method::GET, "/status") => status_response(state),
        (&Method::GET, "/metrics") => metrics_response(metrics_handle),
        _ => not_found_response(),
    }
}

fn health_response(_state: &AdminState) -> Response<Full<Bytes>> {
    let payload = serde_json::json!({"status": "ok"});

    let mut response = Response::new(Full::new(Bytes::from(payload.to_string())));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response
}

fn ready_response(state: &AdminState) -> Response<Full<Bytes>> {
    let is_ready = state.ready.load(Ordering::Relaxed);
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = if is_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    response
}

fn status_response(state: &AdminState) -> Response<Full<Bytes>> {
    let uptime_seconds = state.start_time.elapsed().as_secs();
    let connections = state.active_connections.load(Ordering::Relaxed);
    let ready = state.ready.load(Ordering::Relaxed);
    let payload = serde_json::json!({
        "status": if ready { "ready" } else { "starting_or_draining" },
        "ready": ready,
        "connections": connections,
        "uptime_seconds": uptime_seconds,
        "schema_version": state.schema_version,
        "version": env!("CARGO_PKG_VERSION"),
    });

    let mut response = Response::new(Full::new(Bytes::from(payload.to_string())));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response
}

fn metrics_response(metrics_handle: &PrometheusHandle) -> Response<Full<Bytes>> {
    let rendered = metrics_handle.render();
    let mut response = Response::new(Full::new(Bytes::from(rendered)));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    response
}

fn not_found_response() -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::time::Duration;

    use http_body_util::BodyExt;
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;

    use super::*;

    fn shared_metrics_handle() -> PrometheusHandle {
        static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
        HANDLE
            .get_or_init(|| PrometheusBuilder::new().build_recorder().handle())
            .clone()
    }

    async fn request(port: u16, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("admin listener should accept connections");

        let raw = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream
            .write_all(raw.as_bytes())
            .await
            .expect("HTTP request should be written");

        let mut buf = Vec::new();
        timeout(Duration::from_secs(2), stream.read_to_end(&mut buf))
            .await
            .expect("response should complete")
            .expect("response read should succeed");

        String::from_utf8(buf).expect("response should be UTF-8")
    }

    fn pick_free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
        listener.local_addr().expect("local addr").port()
    }

    #[tokio::test]
    async fn health_response_returns_minimal_liveness_status() {
        let state = AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(3)),
            ready: Arc::new(AtomicBool::new(true)),
        };

        let response = health_response(&state);
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("health body should collect")
            .to_bytes();
        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).expect("health body should be valid JSON");
        assert_eq!(parsed, serde_json::json!({"status":"ok"}));
    }

    #[test]
    fn not_found_response_returns_404() {
        let response = not_found_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_run_returns_error_when_bind_addr_is_unavailable() {
        let occupied = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupying listener should bind");
        let bind_addr = occupied.local_addr().expect("occupied addr should resolve");
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(0)),
            ready: Arc::new(AtomicBool::new(true)),
        });

        let result = run(
            bind_addr,
            state,
            shared_metrics_handle(),
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(result, Err(AdminError::Bind(_))));
    }

    #[tokio::test]
    async fn health_endpoint_returns_200() {
        let port = pick_free_port();
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(2)),
            ready: Arc::new(AtomicBool::new(true)),
        });

        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("bind addr should parse"),
            state,
            shared_metrics_handle(),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = request(port, "/health").await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("\"status\":\"ok\""));

        shutdown.cancel();
        let join_result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("admin task should exit promptly")
            .expect("admin task join should succeed");
        assert!(join_result.is_ok());
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_200() {
        let port = pick_free_port();
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(0)),
            ready: Arc::new(AtomicBool::new(true)),
        });

        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("bind addr should parse"),
            state,
            shared_metrics_handle(),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = request(port, "/metrics").await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(
            response
                .to_lowercase()
                .contains("content-type: text/plain; version=0.0.4")
        );

        shutdown.cancel();
        let join_result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("admin task should exit promptly")
            .expect("admin task join should succeed");
        assert!(join_result.is_ok());
    }

    #[tokio::test]
    async fn ready_endpoint_reflects_readiness() {
        let port = pick_free_port();
        let ready = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(0)),
            ready: Arc::clone(&ready),
        });

        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("bind addr should parse"),
            state,
            shared_metrics_handle(),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;

        let response = request(port, "/ready").await;
        assert!(
            response.starts_with("HTTP/1.1 503"),
            "not-ready should be 503"
        );

        ready.store(true, Ordering::Relaxed);
        let response = request(port, "/ready").await;
        assert!(response.starts_with("HTTP/1.1 200"), "ready should be 200");

        shutdown.cancel();
        let join_result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("admin task should exit promptly")
            .expect("admin task join should succeed");
        assert!(join_result.is_ok());
    }

    #[tokio::test]
    async fn status_endpoint_returns_json() {
        let port = pick_free_port();
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(7)),
            ready: Arc::new(AtomicBool::new(true)),
        });

        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("bind addr should parse"),
            state,
            shared_metrics_handle(),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = request(port, "/status").await;
        assert!(response.starts_with("HTTP/1.1 200"));

        let body = response.split("\r\n\r\n").nth(1).expect("response body");
        let parsed: serde_json::Value =
            serde_json::from_str(body).expect("status body should be valid JSON");
        assert_eq!(parsed["schema_version"], "002.5.0");
        assert_eq!(parsed["connections"], 7);
        assert_eq!(parsed["ready"], true);
        assert_eq!(parsed["status"], "ready");
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert!(parsed["uptime_seconds"].is_u64());

        shutdown.cancel();
        let join_result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("admin task should exit promptly")
            .expect("admin task join should succeed");
        assert!(join_result.is_ok());
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let port = pick_free_port();
        let state = Arc::new(AdminState {
            start_time: Instant::now(),
            schema_version: "002.5.0".to_owned(),
            active_connections: Arc::new(AtomicU64::new(0)),
            ready: Arc::new(AtomicBool::new(true)),
        });

        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run(
            format!("127.0.0.1:{port}")
                .parse()
                .expect("bind addr should parse"),
            state,
            shared_metrics_handle(),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = request(port, "/unknown").await;
        assert!(response.starts_with("HTTP/1.1 404"));

        shutdown.cancel();
        let join_result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("admin task should exit promptly")
            .expect("admin task join should succeed");
        assert!(join_result.is_ok());
    }
}
