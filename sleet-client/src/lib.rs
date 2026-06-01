//! Implement a server-agnostic asynchronous OWP client.

use std::fmt;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use sleet_types::owp::{OwpError, ServerOp, parse_server, validate_identifier};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Configure INIT negotiation values for [`CalClient::connect_with_options`].
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Declare the list of protocol versions accepted by the client.
    pub versions: Vec<String>,
    /// Declare the target schema version expected by the client.
    pub schema: String,
    /// Control whether server `+OK` frames are expected.
    pub verbose: bool,
    /// Maximum payload size in bytes.
    pub max_message_size: usize,
    /// Maximum frame size in bytes.
    pub max_frame_size: usize,
    /// Maximum outbound write buffer size in bytes.
    pub max_write_buffer_size: usize,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            versions: vec!["1.0".to_owned()],
            schema: "002.5.0".to_owned(),
            verbose: true,
            max_message_size: 512 * 1024 * 1024,
            max_frame_size: 128 * 1024 * 1024,
            max_write_buffer_size: 128 * 1024 * 1024,
        }
    }
}

/// Represent a message delivered via `MSG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// Identify the subscription that received the message.
    pub sid: String,
    /// Carry the raw OMS JSON payload text.
    pub payload: std::sync::Arc<str>,
}

/// Represent errors that can occur while using [`CalClient`].
#[derive(Debug)]
pub enum ClientError {
    /// Report malformed client inputs.
    InvalidInput(String),
    /// Report websocket transport-level failures.
    Transport(tokio_tungstenite::tungstenite::Error),
    /// Report protocol parse failures.
    ProtocolParse(sleet_types::owp::ParseError),
    /// Report unexpected operation sequencing.
    UnexpectedServerOp(String),
    /// Report a `-ERR` response from the server.
    Server {
        /// Carry the standardized OWP error code.
        error: OwpError,
        /// Carry optional server detail text.
        details: Option<String>,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::Transport(err) => write!(f, "transport error: {err}"),
            Self::ProtocolParse(err) => write!(f, "protocol parse error: {err}"),
            Self::UnexpectedServerOp(op) => write!(f, "unexpected server operation: {op}"),
            Self::Server { error, details } => match details {
                Some(details) => write!(f, "server error {error}: {details}"),
                None => write!(f, "server error {error}"),
            },
        }
    }
}

impl std::error::Error for ClientError {}

impl From<tokio_tungstenite::tungstenite::Error> for ClientError {
    fn from(value: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Transport(value)
    }
}

impl From<sleet_types::owp::ParseError> for ClientError {
    fn from(value: sleet_types::owp::ParseError) -> Self {
        Self::ProtocolParse(value)
    }
}

/// Manage an OWP websocket connection and operation flow.
#[derive(Debug)]
pub struct CalClient {
    ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    verbose: bool,
}

impl CalClient {
    /// Connect to an LA-CAL websocket endpoint with default INIT options.
    pub async fn connect(url: &str, service_id: &str) -> Result<Self, ClientError> {
        Self::connect_with_options(url, service_id, InitOptions::default()).await
    }

    /// Connect to an LA-CAL websocket endpoint and perform INIT/INFO handshake.
    pub async fn connect_with_options(
        url: &str,
        service_id: &str,
        options: InitOptions,
    ) -> Result<Self, ClientError> {
        if !validate_identifier(service_id) {
            return Err(ClientError::InvalidInput(format!(
                "service_id '{service_id}' is not a valid OWP identifier"
            )));
        }

        let mut request = url
            .into_client_request()
            .map_err(|e| ClientError::InvalidInput(e.to_string()))?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "owp".parse().map_err(
                |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    ClientError::InvalidInput(e.to_string())
                },
            )?,
        );

        let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(options.max_message_size))
            .max_frame_size(Some(options.max_frame_size))
            .max_write_buffer_size(options.max_write_buffer_size);

        let (mut ws, _response) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false).await?;

        let init = serde_json::json!({
            "versions": options.versions,
            "schema": options.schema,
            "verbose": options.verbose,
            "service_id": service_id,
        });

        ws.send(Message::Text(format!("INIT {init}").into()))
            .await?;

        if options.verbose {
            match recv_server_op(&mut ws).await? {
                ServerOp::Ok => {}
                ServerOp::Err { error, details } => {
                    return Err(ClientError::Server { error, details });
                }
                other => {
                    return Err(ClientError::UnexpectedServerOp(format!("{other:?}")));
                }
            }
        }

        match recv_server_op(&mut ws).await? {
            ServerOp::Info(_) => Ok(Self {
                ws,
                verbose: options.verbose,
            }),
            ServerOp::Err { error, details } => Err(ClientError::Server { error, details }),
            other => Err(ClientError::UnexpectedServerOp(format!("{other:?}"))),
        }
    }

    /// Send a SUB operation and wait for status on verbose connections.
    pub async fn subscribe(
        &mut self,
        sid: &str,
        message_name: &str,
        topic: &str,
        group: Option<&str>,
    ) -> Result<(), ClientError> {
        if !validate_identifier(sid) {
            return Err(ClientError::InvalidInput(format!("invalid sid '{sid}'")));
        }
        if !validate_identifier(topic) {
            return Err(ClientError::InvalidInput(format!(
                "invalid topic '{topic}'"
            )));
        }
        if let Some(group) = group
            && !validate_identifier(group)
        {
            return Err(ClientError::InvalidInput(format!(
                "invalid group '{group}'"
            )));
        }

        let frame = group.map_or_else(
            || format!("SUB {sid} {message_name} {topic}"),
            |group| format!("SUB {sid} {message_name} {topic} {group}"),
        );

        self.ws.send(Message::Text(frame.into())).await?;
        self.complete_client_operation().await
    }

    /// Send an UNSUB operation and wait for status on verbose connections.
    pub async fn unsubscribe(&mut self, sid: &str) -> Result<(), ClientError> {
        if !validate_identifier(sid) {
            return Err(ClientError::InvalidInput(format!("invalid sid '{sid}'")));
        }
        self.ws
            .send(Message::Text(format!("UNSUB {sid}").into()))
            .await?;
        self.complete_client_operation().await
    }

    /// Send a PUB operation and wait for status on verbose connections.
    pub async fn publish<T: Serialize + Sync>(
        &mut self,
        topic: &str,
        payload: &T,
    ) -> Result<(), ClientError> {
        if !validate_identifier(topic) {
            return Err(ClientError::InvalidInput(format!(
                "invalid topic '{topic}'"
            )));
        }

        let payload =
            serde_json::to_string(payload).map_err(|e| ClientError::InvalidInput(e.to_string()))?;

        self.ws
            .send(Message::Text(format!("PUB {topic} {payload}").into()))
            .await?;
        self.complete_client_operation().await
    }

    /// Receive the next routed MSG from the server.
    pub async fn recv(&mut self) -> Result<ReceivedMessage, ClientError> {
        loop {
            match recv_server_op(&mut self.ws).await? {
                ServerOp::Msg { sid, payload } => return Ok(ReceivedMessage { sid, payload }),
                ServerOp::Err { error, details } => {
                    return Err(ClientError::Server { error, details });
                }
                ServerOp::Info(_) | ServerOp::Ok => {}
                _ => {
                    return Err(ClientError::UnexpectedServerOp(
                        "received unsupported server operation".to_owned(),
                    ));
                }
            }
        }
    }

    async fn complete_client_operation(&mut self) -> Result<(), ClientError> {
        if self.verbose {
            self.expect_status().await
        } else {
            Ok(())
        }
    }

    async fn expect_status(&mut self) -> Result<(), ClientError> {
        match recv_server_op(&mut self.ws).await? {
            ServerOp::Ok => Ok(()),
            ServerOp::Err { error, details } => Err(ClientError::Server { error, details }),
            ServerOp::Info(_) => Err(ClientError::UnexpectedServerOp(
                "received INFO while awaiting status".to_owned(),
            )),
            ServerOp::Msg { .. } => Err(ClientError::UnexpectedServerOp(
                "received MSG while awaiting status".to_owned(),
            )),
            _ => Err(ClientError::UnexpectedServerOp(
                "received unsupported server operation".to_owned(),
            )),
        }
    }
}

async fn recv_server_op(
    ws: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Result<ServerOp, ClientError> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => return Ok(parse_server(&text)?),
            Some(Ok(Message::Ping(payload))) => {
                ws.send(Message::Pong(payload)).await?;
            }
            Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                return Err(ClientError::UnexpectedServerOp(
                    "received binary frame".to_owned(),
                ));
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(ClientError::UnexpectedServerOp(
                    "connection closed".to_owned(),
                ));
            }
            Some(Err(err)) => return Err(ClientError::Transport(err)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio::time::timeout;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    use super::*;

    const INFO_FRAME: &str = "INFO {\"version\":\"1.0\",\"server_id\":\"mock\",\"uuids\":{\"system\":\"s\",\"service\":\"v\"},\"system_label\":\"mock\"}";

    async fn accept_owp(listener: TcpListener) -> WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.expect("accept should succeed");
        accept_hdr_async(stream, |_: &Request, mut response: Response| {
            response.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                "owp".parse().expect("valid owp header"),
            );
            Ok(response)
        })
        .await
        .expect("websocket handshake should succeed")
    }

    async fn spawn_mock_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind should succeed");
        let addr = listener.local_addr().expect("local addr should resolve");

        tokio::spawn(async move {
            let mut ws = accept_owp(listener).await;

            if let Some(Ok(Message::Text(text))) = ws.next().await {
                assert!(text.starts_with("INIT "));
                ws.send(Message::Text("+OK".into()))
                    .await
                    .expect("send +OK should succeed");
                ws.send(Message::Text(INFO_FRAME.into()))
                    .await
                    .expect("send INFO should succeed");
            }

            if let Some(Ok(Message::Text(text))) = ws.next().await
                && text.starts_with("SUB ")
            {
                ws.send(Message::Text("+OK".into()))
                    .await
                    .expect("send SUB +OK should succeed");
                ws.send(Message::Text("MSG s1 {\"Msg\":{}}".into()))
                    .await
                    .expect("send MSG should succeed");
            }
        });

        addr
    }

    async fn spawn_non_verbose_server(
        expected_prefix: &'static str,
        response_after_operation: Option<&'static str>,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind should succeed");
        let addr = listener.local_addr().expect("local addr should resolve");

        tokio::spawn(async move {
            let mut ws = accept_owp(listener).await;

            if let Some(Ok(Message::Text(text))) = ws.next().await {
                assert!(text.starts_with("INIT "));
                assert!(text.contains("\"verbose\":false"));
                ws.send(Message::Text(INFO_FRAME.into()))
                    .await
                    .expect("send INFO should succeed");
            }

            if let Some(Ok(Message::Text(text))) = ws.next().await {
                assert!(
                    text.starts_with(expected_prefix),
                    "unexpected frame: {text}"
                );
                if let Some(response) = response_after_operation {
                    ws.send(Message::Text(response.into()))
                        .await
                        .expect("send follow-up response should succeed");
                } else {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });

        addr
    }

    #[tokio::test]
    async fn interoperates_with_spec_minimal_server() {
        let addr = spawn_mock_server().await;
        let mut client = CalClient::connect(&format!("ws://{addr}"), "svc")
            .await
            .expect("connect should succeed");

        client
            .subscribe("s1", "Msg", "topic1", None)
            .await
            .expect("subscribe should succeed");

        let msg = client.recv().await.expect("recv should succeed");
        assert_eq!(msg.sid, "s1");
        assert_eq!(&*msg.payload, "{\"Msg\":{}}");
    }

    #[tokio::test]
    async fn non_verbose_subscribe_returns_without_ok_and_preserves_msg() {
        let addr = spawn_non_verbose_server("SUB ", Some("MSG s1 {\"Msg\":{}}")).await;
        let mut client = CalClient::connect_with_options(
            &format!("ws://{addr}"),
            "svc",
            InitOptions {
                verbose: false,
                ..InitOptions::default()
            },
        )
        .await
        .expect("connect should succeed");

        client
            .subscribe("s1", "Msg", "topic1", None)
            .await
            .expect("non-verbose subscribe should not wait for +OK");

        let msg = client.recv().await.expect("recv should get queued MSG");
        assert_eq!(msg.sid, "s1");
        assert_eq!(&*msg.payload, "{\"Msg\":{}}");
    }

    #[tokio::test]
    async fn non_verbose_publish_returns_without_ok_and_preserves_msg() {
        let addr = spawn_non_verbose_server("PUB ", Some("MSG s1 {\"Msg\":{}}")).await;
        let mut client = CalClient::connect_with_options(
            &format!("ws://{addr}"),
            "svc",
            InitOptions {
                verbose: false,
                ..InitOptions::default()
            },
        )
        .await
        .expect("connect should succeed");

        client
            .publish("topic1", &serde_json::json!({ "Msg": {} }))
            .await
            .expect("non-verbose publish should not wait for +OK");

        let msg = client.recv().await.expect("recv should get queued MSG");
        assert_eq!(msg.sid, "s1");
        assert_eq!(&*msg.payload, "{\"Msg\":{}}");
    }

    #[tokio::test]
    async fn non_verbose_unsubscribe_returns_without_ok() {
        let addr = spawn_non_verbose_server("UNSUB ", None).await;
        let mut client = CalClient::connect_with_options(
            &format!("ws://{addr}"),
            "svc",
            InitOptions {
                verbose: false,
                ..InitOptions::default()
            },
        )
        .await
        .expect("connect should succeed");

        timeout(Duration::from_secs(1), client.unsubscribe("s1"))
            .await
            .expect("non-verbose unsubscribe should not wait for server status")
            .expect("unsubscribe should succeed after send");
    }

    #[tokio::test]
    async fn non_verbose_server_errors_are_reported_by_recv() {
        let addr = spawn_non_verbose_server("SUB ", Some("-ERR Illegal-State duplicate sid")).await;
        let mut client = CalClient::connect_with_options(
            &format!("ws://{addr}"),
            "svc",
            InitOptions {
                verbose: false,
                ..InitOptions::default()
            },
        )
        .await
        .expect("connect should succeed");

        client
            .subscribe("s1", "Msg", "topic1", None)
            .await
            .expect("non-verbose subscribe returns after send");

        let err = client.recv().await.expect_err("recv should surface -ERR");
        assert!(matches!(
            err,
            ClientError::Server {
                error: OwpError::IllegalState,
                details: Some(_),
            }
        ));
    }

    #[test]
    fn rejects_invalid_identifier_inputs() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime should start");
        runtime.block_on(async {
            let result = CalClient::connect_with_options(
                "ws://127.0.0.1:9",
                "bad service",
                InitOptions::default(),
            )
            .await;

            assert!(matches!(result, Err(ClientError::InvalidInput(_))));
        });
    }
}
