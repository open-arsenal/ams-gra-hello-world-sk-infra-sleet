//! WebSocket server: TCP listener, OWP subprotocol upgrade, and per-connection
//! state machine handling INIT, PUB, SUB, UNSUB operations.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_hdr_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, info_span, warn};

use crate::config::{Config, ServiceConfig};
use crate::router::{Router, RouterError, Subscription};
use crate::schema::model::Schema;
use crate::validator::validate_oms_json;
use sleet_types::owp::{ClientOp, InfoPayload, InitPayload, OwpError, ServerOp};

/// Fatal errors from [`run`].
#[derive(Debug)]
pub enum ServerError {
    /// TCP bind failed on the configured address.
    Bind(std::io::Error),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(source) => write!(f, "bind failed: {source}"),
        }
    }
}

impl std::error::Error for ServerError {}

/// Bind the TCP listener, accept WebSocket connections, and drain on shutdown.
///
/// The accept loop exits when the shutdown token is cancelled. Active
/// connections are then given a bounded drain window to close cleanly.
#[allow(clippy::significant_drop_tightening)]
pub async fn run(
    config: Arc<Config>,
    schema: Arc<Schema>,
    router: Arc<Router>,
    shutdown: CancellationToken,
    active_connections: Arc<AtomicU64>,
    ready: Arc<AtomicBool>,
) -> Result<(), ServerError> {
    let listener = TcpListener::bind(config.server.bind_addr)
        .await
        .map_err(ServerError::Bind)?;
    info!(addr = %config.server.bind_addr, "listening");
    ready.store(true, Ordering::Relaxed);
    let next_conn_id = Arc::new(AtomicU64::new(1));
    let mut connections: JoinSet<()> = JoinSet::new();
    let conn_limit = Arc::new(tokio::sync::Semaphore::new(config.server.max_connections));
    let mut permit: Option<tokio::sync::OwnedSemaphorePermit> = None;

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                break;
            }
            Some(res) = connections.join_next() => {
                if let Err(e) = res {
                    error!("connection task panicked: {e}");
                }
            }
            Ok(p) = async { conn_limit.clone().acquire_owned().await }, if permit.is_none() => {
                permit = Some(p);
            }
            accept_result = listener.accept(), if permit.is_some() => {
                let Ok((stream, _addr)) = accept_result else {
                    continue;
                };

                let acquired_permit = permit.take().expect("permit must exist");
                let config = Arc::clone(&config);
                let schema = Arc::clone(&schema);
                let router = Arc::clone(&router);
                let next_conn_id = Arc::clone(&next_conn_id);
                let conn_shutdown = shutdown.clone();
                let active_connections = Arc::clone(&active_connections);

                connections.spawn(async move {
                    let _permit = acquired_permit;
                    let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
                    let span = info_span!(
                        "connection",
                        id = conn_id,
                        service_id = tracing::field::Empty
                    );

                    async move {
                        let Ok(ws_stream) = upgrade_with_subprotocol(stream, &config).await else {
                            return;
                        };

                        info!("accepted");

                        if let Err(e) = handle_connection(
                            ws_stream,
                            config,
                            schema,
                            router,
                            conn_id,
                            conn_shutdown,
                            active_connections,
                        )
                        .await
                        {
                            error!("connection error: {e}");
                        }
                    }
                    .instrument(span)
                    .await;
                });
            }
        }
    }

    ready.store(false, Ordering::Relaxed);

    let active_count = connections.len();
    info!(
        active_connections = active_count,
        "shutting down: draining connections"
    );

    let drain = Duration::from_secs(config.server.shutdown_drain_seconds);
    let drain_result = time::timeout(drain, async {
        while connections.join_next().await.is_some() {}
    })
    .await;

    if drain_result.is_err() {
        warn!(
            active_connections = connections.len(),
            drain_seconds = config.server.shutdown_drain_seconds,
            "shutdown drain expired; aborting remaining connection tasks"
        );
        connections.shutdown().await;
    }

    Ok(())
}

/// Perform the WebSocket handshake, enforcing the `owp` subprotocol.
///
/// Returns HTTP 400 if the client does not offer `owp` in `Sec-WebSocket-Protocol`.
async fn upgrade_with_subprotocol(
    stream: TcpStream,
    config: &Config,
) -> Result<WebSocketStream<TcpStream>, ()> {
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(config.server.max_message_size))
        .max_frame_size(Some(config.server.max_frame_size))
        .max_write_buffer_size(config.server.max_write_buffer_size);

    #[allow(clippy::result_large_err)]
    let handshake_result = time::timeout(
        Duration::from_secs(10),
        accept_hdr_async_with_config(
            stream,
            |request: &Request, mut response: Response| {
                let Some(value) = request.headers().get("sec-websocket-protocol") else {
                    warn!("rejected: missing Sec-WebSocket-Protocol");
                    return Err(http_400("missing Sec-WebSocket-Protocol"));
                };

                let Ok(raw) = value.to_str() else {
                    warn!("rejected: invalid Sec-WebSocket-Protocol");
                    return Err(http_400("invalid Sec-WebSocket-Protocol"));
                };

                let has_owp = raw
                    .split(',')
                    .map(str::trim)
                    .any(|protocol| protocol.eq_ignore_ascii_case("owp"));

                if !has_owp {
                    warn!("rejected: missing owp subprotocol");
                    return Err(http_400("missing owp subprotocol"));
                }

                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    tokio_tungstenite::tungstenite::http::HeaderValue::from_static("owp"),
                );

                Ok(response)
            },
            Some(ws_config),
        ),
    )
    .await;

    match handshake_result {
        Ok(Ok(ws)) => Ok(ws),
        Ok(Err(e)) => {
            warn!(error = %e, "websocket handshake failed");
            Err(())
        }
        Err(_) => {
            warn!("websocket handshake timed out");
            Err(())
        }
    }
}

/// Build an HTTP 400 response for rejecting invalid WebSocket upgrade requests.
fn http_400(message: &str) -> tokio_tungstenite::tungstenite::http::Response<Option<String>> {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(400)
        .body(Some(message.to_owned()))
        .expect("valid 400 response")
}

/// Per-connection state machine: awaiting INIT or active.
enum ConnectionState {
    AwaitingInit,
    Active {
        service: Arc<ServiceConfig>,
        verbose: bool,
    },
}

/// Run the main loop for a single WebSocket connection.
///
/// Multiplexes between reading client frames and delivering outbound MSG
/// frames via `tokio::select!`. Cleans up subscriptions on disconnect.
async fn handle_connection(
    ws: WebSocketStream<TcpStream>,
    config: Arc<Config>,
    schema: Arc<Schema>,
    router: Arc<Router>,
    connection_id: u64,
    shutdown: CancellationToken,
    active_connections: Arc<AtomicU64>,
) -> Result<(), String> {
    track_connection_opened(&active_connections);

    let result =
        connection_loop(ws, config, schema, router.as_ref(), connection_id, shutdown).await;

    router.remove_connection(connection_id).await;
    track_connection_closed(&active_connections);
    info!("disconnected");
    result
}

#[allow(clippy::too_many_lines)]
async fn connection_loop(
    mut ws: WebSocketStream<TcpStream>,
    config: Arc<Config>,
    schema: Arc<Schema>,
    router: &Router,
    connection_id: u64,
    shutdown: CancellationToken,
) -> Result<(), String> {
    let mut state = ConnectionState::AwaitingInit;
    let (msg_tx, mut msg_rx) = mpsc::channel::<ServerOp>(1024);
    let idle_enabled = config.server.idle_timeout_seconds > 0;
    let idle_period = Duration::from_secs(config.server.idle_timeout_seconds.max(1));
    let mut idle_interval = time::interval_at(time::Instant::now() + idle_period, idle_period);
    let mut awaiting_pong = false;
    let mut last_activity = time::Instant::now();
    let mut init_timeout = Box::pin(time::sleep(Duration::from_secs(10)));

    loop {
        tokio::select! {
            () = &mut init_timeout, if matches!(state, ConnectionState::AwaitingInit) => {
                warn!("timeout waiting for INIT frame");
                let close = Message::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "INIT timeout".into(),
                }));
                let _ = time::timeout(Duration::from_secs(5), ws.send(close)).await;
                break;
            }
            () = shutdown.cancelled() => {
                info!("shutdown requested; closing websocket");
                let close = Message::Close(Some(CloseFrame {
                    code: CloseCode::Away,
                    reason: "server shutting down".into(),
                }));
                let _ = time::timeout(Duration::from_secs(5), ws.send(close)).await;
                break;
            }
            _ = idle_interval.tick(), if idle_enabled => {
                if time::Instant::now().duration_since(last_activity) < idle_period {
                    awaiting_pong = false;
                    continue;
                }

                if awaiting_pong {
                    info!("idle timeout reached; closing websocket");
                    let close = Message::Close(Some(CloseFrame {
                        code: CloseCode::Away,
                        reason: "idle timeout".into(),
                    }));
                    let _ = time::timeout(Duration::from_secs(5), ws.send(close)).await;
                    break;
                }

                awaiting_pong = true;
                let _ = time::timeout(Duration::from_secs(5), ws.send(Message::Ping(Vec::new().into()))).await;
            }
            outbound = msg_rx.recv() => {
                match outbound {
                    Some(op) => {
                        let send_fut = ws.send(Message::Text(op.to_string().into()));
                        match time::timeout(Duration::from_secs(5), send_fut).await {
                            Ok(Err(e)) => {
                                warn!(error = %e, "ws send failed");
                                break;
                            }
                            Err(_) => {
                                warn!("ws send timed out; dropping slow client");
                                break;
                            }
                            Ok(Ok(())) => {}
                        }
                    }
                    None => break,
                }
            }
            inbound = ws.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        awaiting_pong = false;
                        last_activity = time::Instant::now();

                        let bytes_len = text.len();
                        metrics::counter!("sleet_websocket_bytes_received_total").increment(bytes_len as u64);
                        #[allow(clippy::cast_precision_loss)]
                        metrics::histogram!("sleet_message_size_bytes").record(bytes_len as f64);

                        let keyword = text.split_ascii_whitespace().next().unwrap_or("<empty>");
                        debug!(operation = keyword, "recv");

                        let should_fail = handle_text_frame(
                            &mut ws,
                            &mut state,
                            &text,
                            &config,
                            Arc::clone(&schema),
                            router,
                            connection_id,
                            &msg_tx,
                        ).await?;

                        if should_fail {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        awaiting_pong = false;
                        last_activity = time::Instant::now();
                        warn!("binary frame rejected");
                        send_err(
                            &mut ws,
                            OwpError::IllegalOperation,
                            "binary frames are not supported".to_owned(),
                        )
                        .await?;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        awaiting_pong = false;
                        last_activity = time::Instant::now();
                        let pong_fut = ws.send(Message::Pong(payload));
                        match time::timeout(Duration::from_secs(5), pong_fut).await {
                            Ok(Err(e)) => {
                                warn!(error = %e, "ws pong send failed");
                                break;
                            }
                            Err(_) => {
                                warn!("ws pong send timed out; dropping slow client");
                                break;
                            }
                            Ok(Ok(())) => {}
                        }
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {
                        awaiting_pong = false;
                        last_activity = time::Instant::now();
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                }
            }
        }
    }

    Ok(())
}

fn track_connection_opened(active_connections: &AtomicU64) {
    let current_connections = active_connections.fetch_add(1, Ordering::Relaxed) + 1;
    set_active_connections_metric(current_connections);
}

fn track_connection_closed(active_connections: &AtomicU64) {
    let current_connections = active_connections
        .fetch_sub(1, Ordering::Relaxed)
        .saturating_sub(1);
    set_active_connections_metric(current_connections);
}

fn set_active_connections_metric(count: u64) {
    let bounded_count = u32::try_from(count).unwrap_or(u32::MAX);
    metrics::gauge!("sleet_connections_active").set(f64::from(bounded_count));
}

/// Dispatch a text frame to the appropriate handler based on connection state.
///
/// Returns `Ok(true)` when the connection should be failed (closed).
async fn handle_text_frame(
    ws: &mut WebSocketStream<TcpStream>,
    state: &mut ConnectionState,
    text: &str,
    config: &Config,
    schema: Arc<Schema>,
    router: &Router,
    connection_id: u64,
    msg_tx: &mpsc::Sender<ServerOp>,
) -> Result<bool, String> {
    match state {
        ConnectionState::AwaitingInit => handle_awaiting_init(ws, state, text, config).await,
        ConnectionState::Active { service, verbose } => {
            handle_active(
                ws,
                text,
                service,
                *verbose,
                schema,
                router,
                connection_id,
                msg_tx,
            )
            .await
        }
    }
}

/// Process a text frame while waiting for INIT.
///
/// Returns `Ok(true)` to signal the connection should be failed (closed).
async fn handle_awaiting_init(
    ws: &mut WebSocketStream<TcpStream>,
    state: &mut ConnectionState,
    text: &str,
    config: &Config,
) -> Result<bool, String> {
    match sleet_types::owp::parse(text) {
        Ok(ClientOp::Init(payload)) => match process_init(&payload, config) {
            Ok((service, verbose, info)) => {
                tracing::Span::current().record("service_id", service.service_id.as_str());
                info!(schema = %payload.schema, service_id = %service.service_id, "initialized");
                if verbose {
                    send_server_op(ws, ServerOp::Ok).await?;
                }
                send_server_op(ws, ServerOp::Info(Box::new(info))).await?;
                *state = ConnectionState::Active { service, verbose };
                Ok(false)
            }
            Err((err, details)) => {
                send_err(ws, err.clone(), details).await?;
                metrics::counter!("sleet_connections_failed_total", "error" => err.to_string())
                    .increment(1);
                Ok(true)
            }
        },
        Ok(_) => {
            send_err(
                ws,
                OwpError::IllegalState,
                "INIT required before other operations".to_owned(),
            )
            .await?;
            metrics::counter!("sleet_connections_failed_total", "error" => OwpError::IllegalState.to_string())
                .increment(1);
            Ok(true)
        }
        Err(err) => {
            let (error, details) = map_parse_error(err);
            send_err(ws, error.clone(), details).await?;
            metrics::counter!("sleet_connections_failed_total", "error" => error.to_string())
                .increment(1);
            Ok(true)
        }
    }
}

/// Process a text frame on an active (post-INIT) connection.
///
/// Returns `Ok(true)` to signal the connection should be failed (closed).
async fn handle_active(
    ws: &mut WebSocketStream<TcpStream>,
    text: &str,
    service: &Arc<ServiceConfig>,
    verbose: bool,
    schema: Arc<Schema>,
    router: &Router,
    connection_id: u64,
    msg_tx: &mpsc::Sender<ServerOp>,
) -> Result<bool, String> {
    let parsed = match sleet_types::owp::parse(text) {
        Ok(op) => op,
        Err(err) => {
            let (error, details) = map_parse_error(err);
            send_err(ws, error, details).await?;
            return Ok(false);
        }
    };

    let mut failure = false;
    let result = match parsed {
        ClientOp::Init(_) => {
            debug!("recv INIT after handshake");
            failure = true;
            Err((OwpError::IllegalState, "INIT already completed".to_owned()))
        }
        ClientOp::Pub { topic, payload } => {
            debug!(topic = %topic, "dispatch PUB");
            process_pub(topic, payload, Arc::clone(service), schema, router).await
        }
        ClientOp::Sub {
            sid,
            message_name,
            topic,
            group,
        } => {
            debug!(sid = %sid, message_name = %message_name, topic = %topic, "dispatch SUB");
            process_sub(
                SubRequest {
                    sid,
                    message_name,
                    topic,
                    group,
                    connection_id,
                    sender: msg_tx.clone(),
                },
                service,
                &schema,
                router,
            )
            .await
        }
        ClientOp::Unsub { sid } => {
            debug!(sid = %sid, "dispatch UNSUB");
            process_unsub(connection_id, &sid, router).await
        }
        _ => Err((
            OwpError::IllegalOperation,
            "unsupported client operation".to_owned(),
        )),
    };

    match result {
        Ok(()) => {
            if verbose {
                send_server_op(ws, ServerOp::Ok).await?;
            }
        }
        Err((error, details)) => {
            send_err(ws, error.clone(), details).await?;
            if failure {
                metrics::counter!("sleet_connections_failed_total", "error" => error.to_string())
                    .increment(1);
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Convert an OWP parse error into an `(OwpError, detail)` pair for `-ERR` responses.
fn map_parse_error(err: sleet_types::owp::ParseError) -> (OwpError, String) {
    match err {
        sleet_types::owp::ParseError::UnknownOp(op) => (
            OwpError::IllegalOperation,
            format!("unknown operation: {op}"),
        ),
        other => (OwpError::IllegalArgument, other.to_string()),
    }
}

/// Validate an INIT payload: check version, schema, and `service_id`.
///
/// On success returns the matched service config, verbose flag, and INFO payload.
fn process_init(
    payload: &InitPayload,
    config: &Config,
) -> Result<(Arc<ServiceConfig>, bool, InfoPayload), (OwpError, String)> {
    if !payload.versions.iter().any(|v| v == "1.0") {
        return Err((
            OwpError::UnsupportedVersion,
            "server supports only OWP version 1.0".to_owned(),
        ));
    }

    if payload.schema != config.server.schema_version {
        return Err((
            OwpError::UnsupportedSchema,
            format!(
                "requested schema {} does not match {}",
                payload.schema, config.server.schema_version
            ),
        ));
    }

    let Some(service) = config
        .services
        .iter()
        .find(|s| s.service_id == payload.service_id)
        .cloned()
    else {
        return Err((
            OwpError::UnsupportedService,
            format!("unknown service_id {}", payload.service_id),
        ));
    };

    let info = InfoPayload {
        version: "1.0".to_owned(),
        server_id: config.server.server_id.clone(),
        uuids: sleet_types::owp::Identifiers {
            system: config.server.system_uuid.to_string(),
            service: service.service_uuid.to_string(),
            subsystem: Some(config.server.subsystem_uuid.to_string()),
            capabilities: None,
            components: None,
        },
        system_label: config.server.system_label.clone(),
        connect_urls: None,
    };

    Ok((service, payload.verbose.unwrap_or(true), info))
}

/// Process a PUB operation: authorize topic, parse JSON, validate against schema, route.
async fn process_pub(
    topic: String,
    raw_json: String,
    service: Arc<ServiceConfig>,
    schema: Arc<Schema>,
    router: &Router,
) -> Result<(), (OwpError, String)> {
    let process_start = std::time::Instant::now();

    if !service.allowed_topics.iter().any(|t| t == &topic) {
        return Err((
            OwpError::IllegalState,
            format!(
                "service '{}' is not authorized to publish to topic {}",
                service.service_id, topic
            ),
        ));
    }

    let validation_result = tokio::task::spawn_blocking(move || {
        let val_start = std::time::Instant::now();
        let value: serde_json::Value = match serde_json::from_str(&raw_json) {
            Ok(value) => value,
            Err(e) => {
                return Err((
                    "invalid_json",
                    OwpError::InvalidMessage,
                    format!("invalid JSON payload: {e}"),
                ));
            }
        };

        if let serde_json::Value::Object(root) = &value
            && root.len() == 1
            && let Some((message_name, _)) = root.iter().next()
            && let Some(allowed) = service.topic_bindings.get(&topic)
            && !allowed.contains(message_name)
            && schema.global_elements.contains_key(message_name)
        {
            return Err((
                "topic_binding",
                OwpError::IllegalState,
                format!("message type {message_name} not allowed on topic {topic}"),
            ));
        }

        let res = match validate_oms_json(&schema, &value) {
            Ok(validated) => Ok((validated, topic, raw_json)),
            Err(e) => Err((
                "schema_validation",
                OwpError::InvalidMessage,
                format!("schema validation failed: {e:?}"),
            )),
        };
        metrics::histogram!("sleet_validation_duration_seconds")
            .record(val_start.elapsed().as_secs_f64());
        res
    })
    .await;

    let (validated, topic, raw_json) = match validation_result {
        Ok(Ok(success)) => success,
        Ok(Err((category, error_code, details))) => {
            metrics::counter!("sleet_validation_failures_total", "category" => category)
                .increment(1);
            return Err((error_code, details));
        }
        Err(e) => {
            return Err((
                OwpError::InternalError,
                format!("validation task failed: {e}"),
            ));
        }
    };

    debug!(topic = %topic, message_name = %validated.message_name, "route PUB message");
    router
        .publish(&topic, &validated.message_name, &raw_json)
        .await;
    metrics::counter!("sleet_messages_published_total", "topic" => topic.clone()).increment(1);
    metrics::histogram!("sleet_message_processing_duration_seconds")
        .record(process_start.elapsed().as_secs_f64());
    Ok(())
}

/// Parameters for a SUB request, bundled to reduce argument count.
struct SubRequest {
    sid: String,
    message_name: String,
    topic: String,
    group: Option<String>,
    connection_id: u64,
    sender: mpsc::Sender<ServerOp>,
}

/// Process a SUB operation: authorize topic, validate message name, register subscription.
async fn process_sub(
    req: SubRequest,
    service: &ServiceConfig,
    schema: &Schema,
    router: &Router,
) -> Result<(), (OwpError, String)> {
    if !service.allowed_topics.iter().any(|t| t == &req.topic) {
        return Err((
            OwpError::IllegalState,
            format!(
                "service '{}' is not authorized to subscribe to topic {}",
                service.service_id, req.topic
            ),
        ));
    }

    if !schema.is_global_element(&req.message_name) {
        return Err((
            OwpError::IllegalArgument,
            format!("unknown message name {}", req.message_name),
        ));
    }

    if let Some(allowed) = service.topic_bindings.get(&req.topic)
        && !allowed.contains(&req.message_name)
    {
        return Err((
            OwpError::IllegalState,
            format!(
                "message type {} not allowed on topic {}",
                req.message_name, req.topic
            ),
        ));
    }

    let sid_for_log = req.sid.clone();
    let topic_for_log = req.topic.clone();
    let name_for_log = req.message_name.clone();

    router
        .subscribe(Subscription {
            sid: req.sid,
            connection_id: req.connection_id,
            topic: req.topic,
            message_name: req.message_name,
            group: req.group,
            sender: req.sender,
        })
        .await
        .map_err(|e| match e {
            RouterError::DuplicateSid(sid) => (
                OwpError::IllegalState,
                format!("duplicate subscription id {sid}"),
            ),
            RouterError::UnknownSid(sid) => (
                OwpError::InternalError,
                format!("router inconsistency for sid {sid}"),
            ),
            RouterError::TooManySubscriptions => (
                OwpError::IllegalState,
                "maximum subscriptions reached for this connection".to_owned(),
            ),
        })?;

    debug!(sid = %sid_for_log, topic = %topic_for_log, message_name = %name_for_log, "subscription registered");
    Ok(())
}

/// Process an UNSUB operation: deregister the subscription from the router.
async fn process_unsub(
    connection_id: u64,
    sid: &str,
    router: &Router,
) -> Result<(), (OwpError, String)> {
    router
        .unsubscribe(connection_id, sid)
        .await
        .map_err(|e| match e {
            RouterError::UnknownSid(unknown) => (
                OwpError::IllegalState,
                format!("unknown subscription id {unknown}"),
            ),
            RouterError::DuplicateSid(dupe) => (
                OwpError::InternalError,
                format!("router inconsistency for sid {dupe}"),
            ),
            RouterError::TooManySubscriptions => (
                OwpError::InternalError,
                "router inconsistency during unsubscribe".to_owned(),
            ),
        })?;

    debug!(sid = %sid, "subscription removed");
    Ok(())
}

/// Serialize and send a server operation as a WebSocket text frame.
async fn send_server_op(ws: &mut WebSocketStream<TcpStream>, op: ServerOp) -> Result<(), String> {
    let op_name = match &op {
        ServerOp::Info(_) => "INFO",
        ServerOp::Msg { .. } => "MSG",
        ServerOp::Ok => "+OK",
        ServerOp::Err { .. } => "-ERR",
        _ => "UNKNOWN",
    };
    debug!(operation = op_name, "send");

    let payload = op.to_string();
    let bytes_len = payload.len();
    let send_fut = ws.send(Message::Text(payload.into()));
    match time::timeout(Duration::from_secs(5), send_fut).await {
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("send timed out".to_owned()),
        Ok(Ok(())) => {
            metrics::counter!("sleet_websocket_bytes_sent_total").increment(bytes_len as u64);
            Ok(())
        }
    }
}

/// Send a `-ERR` operation and emit a matching warning log line.
async fn send_err(
    ws: &mut WebSocketStream<TcpStream>,
    error: OwpError,
    details: String,
) -> Result<(), String> {
    warn!(error = %error, details, "sending protocol error");
    send_server_op(
        ws,
        ServerOp::Err {
            error,
            details: Some(details),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::task::JoinHandle;
    use tokio::time::{sleep, timeout};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_util::sync::CancellationToken;

    use crate::config;
    use crate::router::Router;
    use crate::schema::parser;
    use sleet_client::CalClient;
    use sleet_types::owp::{OwpError, ServerOp, parse_server};

    fn pick_free_port() -> io::Result<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(listener.local_addr()?.port())
    }

    fn init_frame(service_id: &str) -> tokio_tungstenite::tungstenite::Utf8Bytes {
        init_frame_custom(service_id, &["1.0"], "002.5.0", None)
    }

    fn metrics_handle() -> PrometheusHandle {
        static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
        HANDLE
            .get_or_init(|| {
                PrometheusBuilder::new()
                    .install_recorder()
                    .expect("prometheus recorder should install")
            })
            .clone()
    }

    fn metric_value(rendered: &str, metric_name: &str, label_fragment: Option<&str>) -> f64 {
        rendered
            .lines()
            .find_map(|line| {
                if line.starts_with('#') {
                    return None;
                }

                if !line.starts_with(metric_name) {
                    return None;
                }

                if let Some(fragment) = label_fragment
                    && !line.contains(fragment)
                {
                    return None;
                }

                line.split_whitespace().nth(1)?.parse::<f64>().ok()
            })
            .unwrap_or(0.0)
    }

    fn init_frame_custom(
        service_id: &str,
        versions: &[&str],
        schema: &str,
        verbose: Option<bool>,
    ) -> tokio_tungstenite::tungstenite::Utf8Bytes {
        let versions_json = versions
            .iter()
            .map(|version| format!("\"{version}\""))
            .collect::<Vec<_>>()
            .join(",");

        let verbose_json = verbose.map_or_else(String::new, |flag| format!(",\"verbose\":{flag}"));

        format!(
            "INIT {{\"versions\":[{versions_json}],\"schema\":\"{schema}\",\"service_id\":\"{service_id}\"{verbose_json}}}"
        )
        .into()
    }

    async fn spawn_server() -> (u16, CancellationToken, JoinHandle<()>) {
        spawn_server_custom(None, &[]).await
    }

    async fn spawn_server_with_idle_timeout(
        idle_timeout_seconds: Option<u64>,
    ) -> (u16, CancellationToken, JoinHandle<()>) {
        spawn_server_custom(idle_timeout_seconds, &[]).await
    }

    /// Spawn a test server with optional idle timeout and per-service topic
    /// binding overrides.
    ///
    /// Each entry in `svc_topic_bindings` is `(service_filename, toml_fragment)`
    /// where `toml_fragment` contains `[[topic_bindings]]` sections to append to
    /// the service's TOML file.
    async fn spawn_server_custom(
        idle_timeout_seconds: Option<u64>,
        svc_topic_bindings: &[(&str, &str)],
    ) -> (u16, CancellationToken, JoinHandle<()>) {
        let port = pick_free_port().expect("pick free port");

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        std::mem::forget(temp);
        let schema_path = root.join("schema.xsd");
        let services_dir = root.join("services.d");
        std::fs::create_dir_all(&services_dir).expect("create services dir");

        std::fs::write(
            &schema_path,
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" version="002.5.0">
  <xs:complexType name="MsgType">
    <xs:sequence/>
  </xs:complexType>
  <xs:element name="Msg" type="MsgType"/>

  <xs:complexType name="DetailedType">
    <xs:sequence>
      <xs:element name="Id" type="xs:string" minOccurs="1" maxOccurs="1"/>
    </xs:sequence>
  </xs:complexType>
  <xs:element name="DetailedMsg" type="DetailedType"/>
</xs:schema>"#,
        )
        .expect("write schema");

        let svc_defs: &[(&str, &str)] = &[
            (
                "svc1.toml",
                "service_id = \"svc1\"\nservice_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\nallowed_topics = [\"mission\"]\n",
            ),
            (
                "svc2.toml",
                "service_id = \"svc2\"\nservice_uuid = \"550e8400-e29b-41d4-a716-446655440002\"\nallowed_topics = [\"mission\"]\n",
            ),
            (
                "svc3.toml",
                "service_id = \"svc3\"\nservice_uuid = \"550e8400-e29b-41d4-a716-446655440003\"\nallowed_topics = [\"other\"]\n",
            ),
        ];

        for (filename, base_content) in svc_defs {
            let extra = svc_topic_bindings
                .iter()
                .find(|(f, _)| f == filename)
                .map_or("", |(_, fragment)| fragment);
            let content = format!("{base_content}{extra}");
            std::fs::write(services_dir.join(filename), content)
                .unwrap_or_else(|_| panic!("write {filename}"));
        }

        let server_toml = root.join("server.toml");
        std::fs::write(
            &server_toml,
            format!(
                "bind_addr = \"127.0.0.1:{port}\"\nserver_id = \"sleet-test\"\nsystem_label = \"Test\"\nschema_path = \"{}\"\nschema_version = \"002.5.0\"\nsystem_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\nservices_dir = \"{}\"\n",
                schema_path.display(),
                services_dir.display(),
            ),
        )
        .expect("write server config");

        let mut cfg = config::load(&server_toml).expect("load config");
        if let Some(seconds) = idle_timeout_seconds {
            cfg.server.idle_timeout_seconds = seconds;
        }
        let cfg = Arc::new(cfg);
        let schema = Arc::new(parser::parse(&schema_path).expect("parse schema"));
        let router = Arc::new(Router::new());

        let shutdown = CancellationToken::new();
        let shutdown_for_server = shutdown.clone();
        let handle = tokio::spawn(async move {
            let active_connections = Arc::new(AtomicU64::new(0));
            let ready = Arc::new(AtomicBool::new(false));
            let _ = super::run(
                cfg,
                schema,
                router,
                shutdown_for_server,
                active_connections,
                ready,
            )
            .await;
        });

        for _ in 0..50 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        (port, shutdown, handle)
    }

    async fn shutdown_server(shutdown: CancellationToken, handle: JoinHandle<()>) {
        shutdown.cancel();
        let result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("server task did not exit after shutdown");
        assert!(result.is_ok(), "server task join failed: {result:?}");
    }

    async fn connect_with_owp(
        port: u16,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut req = format!("ws://127.0.0.1:{port}/")
            .into_client_request()
            .expect("request");
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "owp".parse().expect("header value"),
        );

        let (ws, _resp) = connect_async(req).await.expect("connect websocket");
        ws
    }

    async fn connect_client_with_retry(url: &str, service_id: &str) -> CalClient {
        let mut last_error = None;
        for _ in 0..10 {
            match CalClient::connect(url, service_id).await {
                Ok(client) => return client,
                Err(error) => {
                    last_error = Some(error);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }

        panic!(
            "client should connect after retries: {:?}",
            last_error.expect("last connection error should be captured")
        );
    }

    async fn recv_text(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => text.to_string(),
            other => panic!("expected text message, got {other:?}"),
        }
    }

    async fn recv_parsed(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> ServerOp {
        let text = recv_text(ws).await;
        parse_server(&text).unwrap_or_else(|e| panic!("parse_server failed for '{text}': {e}"))
    }

    async fn assert_disconnected(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) {
        match ws.next().await {
            None | Some(Err(_) | Ok(Message::Close(_))) => {}
            other => panic!("expected disconnect, got {other:?}"),
        }
    }

    async fn init_verbose(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        service_id: &str,
    ) {
        ws.send(Message::Text(init_frame(service_id)))
            .await
            .expect("send init");
        assert!(matches!(recv_parsed(ws).await, ServerOp::Ok));
        assert!(matches!(recv_parsed(ws).await, ServerOp::Info(_)));
    }

    async fn expect_err(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        expected: OwpError,
    ) {
        let resp = recv_parsed(ws).await;
        assert!(matches!(
            resp,
            ServerOp::Err { error, .. } if error.to_string() == expected.to_string()
        ));
    }

    #[tokio::test]
    async fn handshake_timeout_closes_connection() {
        let (port, shutdown, handle) = spawn_server().await;

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("tcp connect");

        // Send a partial request and never finish it.
        let request = "GET / HTTP/1.1\r\nHost: localhost\r\n";

        stream
            .write_all(request.as_bytes())
            .await
            .expect("write partial request");

        let mut buf = vec![0_u8; 1024];

        // Timeout is 10s. Wait up to 12s for the connection to be closed by the server.
        let read_result = timeout(Duration::from_secs(12), stream.read(&mut buf))
            .await
            .expect("read should not exceed 12s timeout")
            .expect("read from socket failed unexpectedly");

        assert_eq!(
            read_result, 0,
            "expected immediate EOF after 10s handshake timeout"
        );

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_timeout_closes_connection() {
        let (port, shutdown, handle) = spawn_server().await;

        let mut ws = connect_with_owp(port).await;

        // Timeout is 10s. Wait up to 12s for the connection to be closed by the server.
        let msg_result = timeout(Duration::from_secs(12), ws.next())
            .await
            .expect("read should not exceed 12s timeout");

        match msg_result {
            Some(Ok(Message::Close(_))) | None => {}
            other => panic!("expected close or EOF, got: {other:?}"),
        }

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn missing_subprotocol_returns_http_400() {
        let (port, shutdown, handle) = spawn_server().await;

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("tcp connect");

        let request = "GET / HTTP/1.1\r\n\
Host: localhost\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
\r\n";

        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");

        let mut buf = vec![0_u8; 1024];
        let n = stream.read(&mut buf).await.expect("read response");
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(response.starts_with("HTTP/1.1 400") || response.starts_with("HTTP/1.0 400"));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_success_sends_ok_then_info() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame("svc1")))
            .await
            .expect("send init");

        let first = recv_parsed(&mut ws).await;
        let second = recv_parsed(&mut ws).await;

        assert!(matches!(first, ServerOp::Ok));
        assert!(matches!(second, ServerOp::Info(_)));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pre_init_non_init_fails_connection() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text("PUB mission {}".into()))
            .await
            .expect("send pre-init pub");

        let err = recv_text(&mut ws).await;
        assert!(err.starts_with("-ERR Illegal-State"));

        assert_disconnected(&mut ws).await;
        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn duplicate_init_fails_connection() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame("svc1")))
            .await
            .expect("send first init");
        let _ = recv_text(&mut ws).await;
        let _ = recv_text(&mut ws).await;

        ws.send(Message::Text(init_frame("svc1")))
            .await
            .expect("send second init");

        let err = recv_text(&mut ws).await;
        assert!(err.starts_with("-ERR Illegal-State"));

        assert_disconnected(&mut ws).await;
        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_bad_version_fails_connection() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame_custom(
            "svc1",
            &["2.0"],
            "002.5.0",
            None,
        )))
        .await
        .expect("send init");

        expect_err(&mut ws, OwpError::UnsupportedVersion).await;
        assert_disconnected(&mut ws).await;

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_bad_schema_fails_connection() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame_custom(
            "svc1",
            &["1.0"],
            "999.0.0",
            None,
        )))
        .await
        .expect("send init");

        expect_err(&mut ws, OwpError::UnsupportedSchema).await;
        assert_disconnected(&mut ws).await;

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_unknown_service_fails_connection() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame("unknown")))
            .await
            .expect("send init");

        expect_err(&mut ws, OwpError::UnsupportedService).await;
        assert_disconnected(&mut ws).await;

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn init_non_verbose_skips_ok() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;

        ws.send(Message::Text(init_frame_custom(
            "svc1",
            &["1.0"],
            "002.5.0",
            Some(false),
        )))
        .await
        .expect("send init");

        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Info(_)));
        assert!(
            timeout(Duration::from_millis(100), ws.next())
                .await
                .is_err()
        );

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_invalid_json() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("PUB mission {not json}".into()))
            .await
            .expect("send pub");
        expect_err(&mut ws, OwpError::InvalidMessage).await;

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_unknown_root_element() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("PUB mission {\"Unknown\":{}}".into()))
            .await
            .expect("send pub");
        expect_err(&mut ws, OwpError::InvalidMessage).await;

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_missing_required_field() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("PUB mission {\"DetailedMsg\":{}}".into()))
            .await
            .expect("send pub");
        expect_err(&mut ws, OwpError::InvalidMessage).await;

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_disallowed_topic() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("PUB other {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        let resp = recv_parsed(&mut ws).await;
        assert!(matches!(
            resp,
            ServerOp::Err { error, details } if error.to_string() == OwpError::IllegalState.to_string()
                && details == Some("service 'svc1' is not authorized to publish to topic other".to_string())
        ));

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_disallowed_message_type_for_topic_returns_illegal_state() {
        let (port, shutdown, handle) = spawn_server_custom(
            None,
            &[(
                "svc1.toml",
                "\n[[topic_bindings]]\ntopic = \"mission\"\nallowed_messages = [\"Msg\"]\n",
            )],
        )
        .await;

        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("SUB s1 DetailedMsg mission".into()))
            .await
            .expect("send sub");

        expect_err(&mut ws, OwpError::IllegalState).await;

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_unknown_message_name_with_topic_binding_returns_illegal_argument() {
        let (port, shutdown, handle) = spawn_server_custom(
            None,
            &[(
                "svc1.toml",
                "\n[[topic_bindings]]\ntopic = \"mission\"\nallowed_messages = [\"Msg\"]\n",
            )],
        )
        .await;

        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("SUB s1 Nope mission".into()))
            .await
            .expect("send sub");

        expect_err(&mut ws, OwpError::IllegalArgument).await;

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_disallowed_topic() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("SUB s1 Msg other".into()))
            .await
            .expect("send sub");
        let resp = recv_parsed(&mut ws).await;
        assert!(matches!(
            resp,
            ServerOp::Err { error, details } if error.to_string() == OwpError::IllegalState.to_string()
                && details == Some("service 'svc1' is not authorized to subscribe to topic other".to_string())
        ));

        ws.send(Message::Text("SUB s2 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_unknown_message_name() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("SUB s1 Nope mission".into()))
            .await
            .expect("send sub");
        expect_err(&mut ws, OwpError::IllegalArgument).await;

        ws.send(Message::Text("SUB s2 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_duplicate_sid() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("SUB dup Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        ws.send(Message::Text("SUB dup Msg mission".into()))
            .await
            .expect("send duplicate sub");
        expect_err(&mut ws, OwpError::IllegalState).await;

        ws.send(Message::Text("UNSUB dup".into()))
            .await
            .expect("send unsub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn unsub_unknown_sid() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("UNSUB nope".into()))
            .await
            .expect("send unsub");
        expect_err(&mut ws, OwpError::IllegalState).await;

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn unsub_stops_delivery() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut sub_ws = connect_with_owp(port).await;
        let mut pub_ws = connect_with_owp(port).await;

        init_verbose(&mut sub_ws, "svc1").await;
        init_verbose(&mut pub_ws, "svc2").await;

        sub_ws
            .send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut sub_ws).await, ServerOp::Ok));

        sub_ws
            .send(Message::Text("UNSUB s1".into()))
            .await
            .expect("send unsub");
        assert!(matches!(recv_parsed(&mut sub_ws).await, ServerOp::Ok));

        pub_ws
            .send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        assert!(matches!(recv_parsed(&mut pub_ws).await, ServerOp::Ok));

        assert!(
            timeout(Duration::from_millis(150), sub_ws.next())
                .await
                .is_err()
        );

        sub_ws
            .send(Message::Text("SUB s2 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut sub_ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn unknown_operation() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("CONNECT foo".into()))
            .await
            .expect("send unknown op");
        expect_err(&mut ws, OwpError::IllegalOperation).await;

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn verbose_false_suppresses_ok_on_sub_pub() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut sub_ws = connect_with_owp(port).await;
        let mut pub_ws = connect_with_owp(port).await;

        sub_ws
            .send(Message::Text(init_frame_custom(
                "svc1",
                &["1.0"],
                "002.5.0",
                Some(false),
            )))
            .await
            .expect("sub init");
        assert!(matches!(recv_parsed(&mut sub_ws).await, ServerOp::Info(_)));

        init_verbose(&mut pub_ws, "svc2").await;

        sub_ws
            .send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send sub");
        assert!(
            timeout(Duration::from_millis(100), sub_ws.next())
                .await
                .is_err()
        );

        pub_ws
            .send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        assert!(matches!(recv_parsed(&mut pub_ws).await, ServerOp::Ok));

        let delivered = recv_parsed(&mut sub_ws).await;
        assert!(matches!(
            delivered,
            ServerOp::Msg { ref sid, ref payload }
            if sid == "s1" && &**payload == "{\"Msg\":{}}"
        ));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sub_then_pub_delivers_msg() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut sub_ws = connect_with_owp(port).await;
        let mut pub_ws = connect_with_owp(port).await;

        sub_ws
            .send(Message::Text(init_frame("svc1")))
            .await
            .expect("sub init");
        let _ = recv_text(&mut sub_ws).await;
        let _ = recv_text(&mut sub_ws).await;

        pub_ws
            .send(Message::Text(init_frame("svc2")))
            .await
            .expect("pub init");
        let _ = recv_text(&mut pub_ws).await;
        let _ = recv_text(&mut pub_ws).await;

        sub_ws
            .send(Message::Text("SUB sub1 Msg mission".into()))
            .await
            .expect("send sub");
        let sub_ok = recv_text(&mut sub_ws).await;
        assert_eq!(sub_ok, "+OK");

        pub_ws
            .send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        let pub_ok = recv_text(&mut pub_ws).await;
        assert_eq!(pub_ok, "+OK");

        let delivered = recv_text(&mut sub_ws).await;
        assert_eq!(delivered, "MSG sub1 {\"Msg\":{}}");

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn same_sid_on_different_connections_can_unsubscribe_independently() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut sub_ws1 = connect_with_owp(port).await;
        let mut sub_ws2 = connect_with_owp(port).await;
        let mut pub_ws = connect_with_owp(port).await;

        init_verbose(&mut sub_ws1, "svc1").await;
        init_verbose(&mut sub_ws2, "svc1").await;
        init_verbose(&mut pub_ws, "svc2").await;

        sub_ws1
            .send(Message::Text("SUB shared Msg mission".into()))
            .await
            .expect("send first sub");
        assert!(matches!(recv_parsed(&mut sub_ws1).await, ServerOp::Ok));

        sub_ws2
            .send(Message::Text("SUB shared Msg mission".into()))
            .await
            .expect("send second sub");
        assert!(matches!(recv_parsed(&mut sub_ws2).await, ServerOp::Ok));

        pub_ws
            .send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        assert!(matches!(recv_parsed(&mut pub_ws).await, ServerOp::Ok));
        assert!(
            matches!(recv_parsed(&mut sub_ws1).await, ServerOp::Msg { sid, .. } if sid == "shared")
        );
        assert!(
            matches!(recv_parsed(&mut sub_ws2).await, ServerOp::Msg { sid, .. } if sid == "shared")
        );

        sub_ws1
            .send(Message::Text("UNSUB shared".into()))
            .await
            .expect("send unsub");
        assert!(matches!(recv_parsed(&mut sub_ws1).await, ServerOp::Ok));

        pub_ws
            .send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send second pub");
        assert!(matches!(recv_parsed(&mut pub_ws).await, ServerOp::Ok));
        assert!(
            timeout(Duration::from_millis(150), sub_ws1.next())
                .await
                .is_err()
        );
        assert!(
            matches!(recv_parsed(&mut sub_ws2).await, ServerOp::Msg { sid, .. } if sid == "shared")
        );

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn idle_timeout_sends_ping_on_inactivity() {
        let (port, shutdown, handle) = spawn_server_with_idle_timeout(Some(1)).await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        let first = timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("expected ping frame")
            .expect("websocket should remain open")
            .expect("expected successful frame");
        assert!(matches!(first, Message::Ping(_)));

        ws.send(Message::Text("SUB s1 Msg mission".into()))
            .await
            .expect("send follow-up SUB after ping");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn metrics_publish_counter_increments() {
        let handle = metrics_handle();
        let before = metric_value(
            &handle.render(),
            "sleet_messages_published_total",
            Some("topic=\"mission\""),
        );

        let (port, shutdown, task) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;
        ws.send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        let after = metric_value(
            &handle.render(),
            "sleet_messages_published_total",
            Some("topic=\"mission\""),
        );
        assert!(after >= before + 1.0);

        shutdown_server(shutdown, task).await;
    }

    #[tokio::test]
    async fn metrics_validation_failure_counter_increments() {
        let handle = metrics_handle();
        let before = metric_value(
            &handle.render(),
            "sleet_validation_failures_total",
            Some("category=\"invalid_json\""),
        );

        let (port, shutdown, task) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;
        ws.send(Message::Text("PUB mission {not json}".into()))
            .await
            .expect("send invalid pub");
        expect_err(&mut ws, OwpError::InvalidMessage).await;

        let after = metric_value(
            &handle.render(),
            "sleet_validation_failures_total",
            Some("category=\"invalid_json\""),
        );
        assert!(after >= before + 1.0);

        shutdown_server(shutdown, task).await;
    }

    #[tokio::test]
    async fn metrics_connections_gauge_is_emitted() {
        let handle = metrics_handle();

        let (port, shutdown, task) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;
        ws.close(None).await.expect("close websocket");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let rendered = handle.render();
        let gauge = metric_value(&rendered, "sleet_connections_active", None);
        assert!(gauge >= 0.0);

        shutdown_server(shutdown, task).await;
    }

    #[tokio::test]
    async fn shutdown_closes_active_connections() {
        let (port, shutdown, handle) = spawn_server().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        shutdown.cancel();

        let frame = timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("expected websocket close frame during shutdown");

        assert!(matches!(frame, Some(Ok(Message::Close(_)))));

        let joined = timeout(Duration::from_secs(2), handle)
            .await
            .expect("server task did not exit after shutdown");
        assert!(joined.is_ok(), "server task join failed: {joined:?}");
    }

    #[tokio::test]
    async fn shutdown_stops_accepting_new_connections() {
        let (port, shutdown, handle) = spawn_server().await;
        shutdown_server(shutdown, handle).await;

        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Err(_) => {}
            Ok(mut stream) => {
                let mut byte = [0_u8; 1];
                let read_result = timeout(Duration::from_millis(200), stream.read(&mut byte))
                    .await
                    .expect("post-shutdown connection did not close promptly")
                    .expect("read from post-shutdown socket failed unexpectedly");
                assert_eq!(read_result, 0, "expected immediate EOF after shutdown");
            }
        }
    }

    // -- Topic binding enforcement --

    /// Helper to spawn a server where svc1 has topic bindings restricting
    /// "mission" to only accept "Msg" messages.
    async fn spawn_server_with_topic_bindings() -> (u16, CancellationToken, JoinHandle<()>) {
        spawn_server_custom(
            None,
            &[(
                "svc1.toml",
                "\n[[topic_bindings]]\ntopic = \"mission\"\nallowed_messages = [\"Msg\"]\n",
            )],
        )
        .await
    }

    #[tokio::test]
    async fn pub_allowed_message_type_for_topic_succeeds() {
        let (port, shutdown, handle) = spawn_server_with_topic_bindings().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        ws.send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send pub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_disallowed_message_type_for_topic_returns_illegal_state() {
        let (port, shutdown, handle) = spawn_server_with_topic_bindings().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        // DetailedMsg is a valid schema element but not in svc1's topic_bindings for "mission"
        ws.send(Message::Text(
            "PUB mission {\"DetailedMsg\":{\"Id\":\"x\"}}".into(),
        ))
        .await
        .expect("send pub");
        expect_err(&mut ws, OwpError::IllegalState).await;

        // Connection stays open after topic-binding rejection
        ws.send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send follow-up pub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_unbound_topic_allows_any_message() {
        let (port, shutdown, handle) = spawn_server_with_topic_bindings().await;
        let mut ws = connect_with_owp(port).await;
        // svc2 has no topic_bindings - permissive
        init_verbose(&mut ws, "svc2").await;

        ws.send(Message::Text(
            "PUB mission {\"DetailedMsg\":{\"Id\":\"x\"}}".into(),
        ))
        .await
        .expect("send pub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn pub_topic_binding_does_not_affect_schema_validation() {
        let (port, shutdown, handle) = spawn_server_with_topic_bindings().await;
        let mut ws = connect_with_owp(port).await;
        init_verbose(&mut ws, "svc1").await;

        // Invalid JSON - schema validation fires before topic-binding check
        ws.send(Message::Text("PUB mission {not json}".into()))
            .await
            .expect("send pub");
        expect_err(&mut ws, OwpError::InvalidMessage).await;

        // Connection stays open
        ws.send(Message::Text("PUB mission {\"Msg\":{}}".into()))
            .await
            .expect("send follow-up pub");
        assert!(matches!(recv_parsed(&mut ws).await, ServerOp::Ok));

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn sleet_client_interoperates_with_sleet_server() {
        let (port, shutdown, handle) = spawn_server().await;
        let url = format!("ws://127.0.0.1:{port}");

        let mut subscriber = connect_client_with_retry(&url, "svc1").await;
        let mut publisher = connect_client_with_retry(&url, "svc2").await;

        subscriber
            .subscribe("s-client", "Msg", "mission", None)
            .await
            .expect("subscribe should succeed");

        publisher
            .publish("mission", &serde_json::json!({"Msg": {}}))
            .await
            .expect("publish should succeed");

        let received = subscriber
            .recv()
            .await
            .expect("message should be delivered");
        assert_eq!(received.sid, "s-client");
        assert_eq!(&*received.payload, "{\"Msg\":{}}");

        shutdown_server(shutdown, handle).await;
    }

    #[tokio::test]
    async fn max_connections_enforced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        std::mem::forget(temp);
        let schema_path = root.join("schema.xsd");
        std::fs::write(
            &schema_path,
            r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" version="002.5.0">
  <xs:complexType name="MsgType"><xs:sequence/></xs:complexType>
  <xs:element name="Msg" type="MsgType"/>
</xs:schema>"#,
        )
        .unwrap();

        let services_dir = root.join("services.d");
        std::fs::create_dir_all(&services_dir).unwrap();
        std::fs::write(
            services_dir.join("svc1.toml"),
            "service_id = \"svc1\"\nservice_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\nallowed_topics = []\n",
        )
        .unwrap();

        let port = pick_free_port().unwrap();
        let server_toml = root.join("server.toml");
        std::fs::write(
            &server_toml,
            format!(
                "bind_addr = \"127.0.0.1:{}\"\nserver_id = \"s\"\nsystem_label = \"T\"\nschema_path = \"{}\"\nschema_version = \"002.5.0\"\nsystem_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\nservices_dir = \"{}\"\nmax_connections = 1\n",
                port,
                schema_path.display(),
                services_dir.display()
            ),
        )
        .unwrap();

        let cfg = std::sync::Arc::new(crate::config::load(&server_toml).unwrap());
        let schema = std::sync::Arc::new(crate::schema::parser::parse(&schema_path).unwrap());
        let router = std::sync::Arc::new(crate::router::Router::new());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let active_connections = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let shutdown_clone = shutdown.clone();
        let handle = tokio::spawn(async move {
            let _ = super::run(
                cfg,
                schema,
                router,
                shutdown_clone,
                active_connections,
                ready,
            )
            .await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let mut ws1 = connect_with_owp(port).await;
        init_verbose(&mut ws1, "svc1").await;

        let ws2_fut = connect_with_owp(port);
        let timeout_result =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), ws2_fut).await;
        assert!(timeout_result.is_err(), "second connection should block");

        let _ = ws1.close(None).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let mut ws3 = connect_with_owp(port).await;
        init_verbose(&mut ws3, "svc1").await;

        shutdown.cancel();
        let _ = handle.await;
    }
}
