//! Define OWP protocol types and frame parsing/formatting utilities.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Validate that a value matches the OWP identifier grammar.
///
/// The allowed characters are ASCII letters, digits, underscore, hyphen,
/// and dot (`^[A-Za-z0-9_\-.]+$`).
#[must_use]
pub fn validate_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// Represent OWP error types defined by OMSC-SPC-013.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwpError {
    /// Report an unsupported protocol version request.
    UnsupportedVersion,
    /// Report an unsupported schema version request.
    UnsupportedSchema,
    /// Report an unknown or unauthorized service identifier.
    UnsupportedService,
    /// Report an unknown or disallowed protocol operation.
    IllegalOperation,
    /// Report a malformed operation argument.
    IllegalArgument,
    /// Report an operation that is invalid for current state.
    IllegalState,
    /// Report an internal server processing failure.
    InternalError,
    /// Report a message payload that fails message-format validation.
    InvalidMessage,
}

impl fmt::Display for OwpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::UnsupportedVersion => "Unsupported-Version",
            Self::UnsupportedSchema => "Unsupported-Schema",
            Self::UnsupportedService => "Unsupported-Service",
            Self::IllegalOperation => "Illegal-Operation",
            Self::IllegalArgument => "Illegal-Argument",
            Self::IllegalState => "Illegal-State",
            Self::InternalError => "Internal-Error",
            Self::InvalidMessage => "Invalid-Message",
        };
        f.write_str(s)
    }
}

impl FromStr for OwpError {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Unsupported-Version" => Ok(Self::UnsupportedVersion),
            "Unsupported-Schema" => Ok(Self::UnsupportedSchema),
            "Unsupported-Service" => Ok(Self::UnsupportedService),
            "Illegal-Operation" => Ok(Self::IllegalOperation),
            "Illegal-Argument" => Ok(Self::IllegalArgument),
            "Illegal-State" => Ok(Self::IllegalState),
            "Internal-Error" => Ok(Self::InternalError),
            "Invalid-Message" => Ok(Self::InvalidMessage),
            other => Err(ParseError::UnknownOp(other.to_owned())),
        }
    }
}

/// Represent the INIT payload sent by clients.
#[derive(Debug, Clone, Deserialize)]
pub struct InitPayload {
    /// List supported OWP versions.
    pub versions: Vec<String>,
    /// Requested OMS schema version.
    pub schema: String,
    /// Control whether `+OK` responses are emitted.
    pub verbose: Option<bool>,
    /// Declare the logical service identifier for this connection.
    pub service_id: String,
}

/// Represent client-to-server OWP operations.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ClientOp {
    /// Negotiate the connection with an INIT payload.
    Init(InitPayload),
    /// Publish a raw OMS JSON payload to a topic.
    Pub {
        /// Select the target topic.
        topic: String,
        /// Carry the JSON payload text.
        payload: String,
    },
    /// Register a subscription for a topic and message name.
    Sub {
        /// Identify the subscription.
        sid: String,
        /// Filter by message name.
        message_name: String,
        /// Select the subscribed topic.
        topic: String,
        /// Optionally assign a subscription group.
        group: Option<String>,
    },
    /// Remove a previously registered subscription.
    Unsub {
        /// Identify the subscription to remove.
        sid: String,
    },
}

/// Describe parse failures when decoding OWP frames.
#[non_exhaustive]
#[derive(Debug)]
pub enum ParseError {
    /// Indicate that the frame was empty or only delimiters.
    EmptyFrame,
    /// Indicate that the operation keyword is unknown.
    UnknownOp(String),
    /// Indicate that a required field is missing.
    MissingField {
        /// Name the operation being parsed.
        op: &'static str,
        /// Name the missing field.
        field: &'static str,
    },
    /// Indicate that extra trailing fields were found.
    ExtraFields {
        /// Name the operation being parsed.
        op: &'static str,
    },
    /// Indicate that an identifier does not follow OWP rules.
    InvalidIdentifier {
        /// Name the operation being parsed.
        op: &'static str,
        /// Name the invalid field.
        field: &'static str,
        /// Include the invalid value.
        value: String,
    },
    /// Indicate that JSON payload decoding failed.
    InvalidJson {
        /// Name the operation being parsed.
        op: &'static str,
        /// Provide the serde parse failure.
        source: serde_json::Error,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => write!(f, "empty frame"),
            Self::UnknownOp(op) => write!(f, "unknown operation '{op}'"),
            Self::MissingField { op, field } => {
                write!(f, "{op}: missing required field '{field}'")
            }
            Self::ExtraFields { op } => write!(f, "{op}: unexpected extra fields"),
            Self::InvalidIdentifier { op, field, value } => {
                write!(
                    f,
                    "{op}: field '{field}' value '{value}' is not a valid OWP identifier"
                )
            }
            Self::InvalidJson { op, source } => {
                write!(f, "{op}: invalid JSON: {source}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Represent UUID collections emitted in INFO responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identifiers {
    /// Carry the platform-level UUID.
    pub system: String,
    /// Carry the service-level UUID.
    pub service: String,
    /// Optionally carry a subsystem UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<String>,
    /// Optionally map capability names to UUIDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<HashMap<String, String>>,
    /// Optionally map component names to UUIDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<HashMap<String, String>>,
}

/// Represent INFO payload values sent by servers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoPayload {
    /// Declare the negotiated OWP version.
    pub version: String,
    /// Identify the server instance.
    pub server_id: String,
    /// Provide system/service UUID mappings.
    pub uuids: Identifiers,
    /// Provide a human-readable system label.
    pub system_label: String,
    /// Optionally advertise alternate connect URLs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_urls: Option<Vec<String>>,
}

/// Represent server-to-client OWP operations.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ServerOp {
    /// Confirm success for verbose-mode connections.
    Ok,
    /// Report operation failure details.
    Err {
        /// Carry the standardized OWP error code.
        error: OwpError,
        /// Optionally include a human-readable detail string.
        details: Option<String>,
    },
    /// Return negotiated connection and server metadata.
    Info(Box<InfoPayload>),
    /// Deliver a routed message to a subscription ID.
    Msg {
        /// Identify the target subscription.
        sid: String,
        /// Carry the raw message payload.
        payload: std::sync::Arc<str>,
    },
}

impl fmt::Display for ServerOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("+OK"),
            Self::Err { error, details } => match details {
                Some(d) => write!(f, "-ERR {error} {d}"),
                None => write!(f, "-ERR {error}"),
            },
            Self::Info(payload) => {
                let json = serde_json::to_string(payload).map_err(|_| fmt::Error)?;
                write!(f, "INFO {json}")
            }
            Self::Msg { sid, payload } => write!(f, "MSG {sid} {payload}"),
        }
    }
}

const fn is_delim(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn split_first_token(s: &str) -> Option<(&str, &str)> {
    let s = s.as_bytes();
    let start = s.iter().position(|b| !is_delim(*b))?;
    let end = s[start..]
        .iter()
        .position(|b| is_delim(*b))
        .map_or(s.len(), |p| start + p);
    let token = std::str::from_utf8(&s[start..end]).expect("input was &str");
    let rest_start = s[end..]
        .iter()
        .position(|b| !is_delim(*b))
        .map_or(s.len(), |p| end + p);
    let rest = std::str::from_utf8(&s[rest_start..]).expect("input was &str");
    Some((token, rest))
}

fn tokenize(s: &str, max: usize) -> Vec<&str> {
    let mut tokens = Vec::with_capacity(max);
    let mut remaining = s;

    while tokens.len() < max {
        match split_first_token(remaining) {
            Some((token, rest)) => {
                tokens.push(token);
                if tokens.len() == max {
                    break;
                }
                remaining = rest;
            }
            None => break,
        }
    }
    tokens
}

/// Parse a client-to-server OWP frame.
pub fn parse(frame: &str) -> Result<ClientOp, ParseError> {
    let (keyword, rest) = split_first_token(frame).ok_or(ParseError::EmptyFrame)?;

    match keyword {
        "INIT" => parse_init(rest),
        "PUB" => parse_pub(rest),
        "SUB" => parse_sub(rest),
        "UNSUB" => parse_unsub(rest),
        other => Err(ParseError::UnknownOp(other.to_owned())),
    }
}

/// Parse a server-to-client OWP frame.
pub fn parse_server(frame: &str) -> Result<ServerOp, ParseError> {
    let (keyword, rest) = split_first_token(frame).ok_or(ParseError::EmptyFrame)?;

    match keyword {
        "+OK" => {
            if rest.is_empty() {
                Ok(ServerOp::Ok)
            } else {
                Err(ParseError::ExtraFields { op: "+OK" })
            }
        }
        "-ERR" => {
            let (error_name, details) =
                split_first_token(rest).ok_or(ParseError::MissingField {
                    op: "-ERR",
                    field: "error",
                })?;
            let error = OwpError::from_str(error_name)?;
            let details = if details.is_empty() {
                None
            } else {
                Some(details.to_owned())
            };
            Ok(ServerOp::Err { error, details })
        }
        "INFO" => {
            if rest.is_empty() {
                return Err(ParseError::MissingField {
                    op: "INFO",
                    field: "payload",
                });
            }

            let payload: InfoPayload = serde_json::from_str(rest)
                .map_err(|source| ParseError::InvalidJson { op: "INFO", source })?;
            Ok(ServerOp::Info(Box::new(payload)))
        }
        "MSG" => {
            let (sid, payload) = split_first_token(rest).ok_or(ParseError::MissingField {
                op: "MSG",
                field: "sid",
            })?;

            if !validate_identifier(sid) {
                return Err(ParseError::InvalidIdentifier {
                    op: "MSG",
                    field: "sid",
                    value: sid.to_owned(),
                });
            }

            if payload.is_empty() {
                return Err(ParseError::MissingField {
                    op: "MSG",
                    field: "payload",
                });
            }

            Ok(ServerOp::Msg {
                sid: sid.to_owned(),
                payload: std::sync::Arc::from(payload),
            })
        }
        other => Err(ParseError::UnknownOp(other.to_owned())),
    }
}

fn parse_init(rest: &str) -> Result<ClientOp, ParseError> {
    if rest.is_empty() {
        return Err(ParseError::MissingField {
            op: "INIT",
            field: "payload",
        });
    }

    let payload: InitPayload = serde_json::from_str(rest).map_err(|e| ParseError::InvalidJson {
        op: "INIT",
        source: e,
    })?;

    if payload.versions.is_empty() {
        return Err(ParseError::MissingField {
            op: "INIT",
            field: "versions",
        });
    }

    if !validate_identifier(&payload.service_id) {
        return Err(ParseError::InvalidIdentifier {
            op: "INIT",
            field: "service_id",
            value: payload.service_id,
        });
    }

    Ok(ClientOp::Init(payload))
}

fn parse_pub(rest: &str) -> Result<ClientOp, ParseError> {
    let (topic, payload) = split_first_token(rest).ok_or(ParseError::MissingField {
        op: "PUB",
        field: "topic",
    })?;

    if !validate_identifier(topic) {
        return Err(ParseError::InvalidIdentifier {
            op: "PUB",
            field: "topic",
            value: topic.to_owned(),
        });
    }

    if payload.is_empty() {
        return Err(ParseError::MissingField {
            op: "PUB",
            field: "payload",
        });
    }

    Ok(ClientOp::Pub {
        topic: topic.to_owned(),
        payload: payload.to_owned(),
    })
}

fn parse_sub(rest: &str) -> Result<ClientOp, ParseError> {
    let tokens = tokenize(rest, 5);

    if tokens.len() < 3 {
        let missing = match tokens.len() {
            0 => "sid",
            1 => "message_name",
            2 => "topic",
            _ => unreachable!(),
        };
        return Err(ParseError::MissingField {
            op: "SUB",
            field: missing,
        });
    }

    if tokens.len() > 4 {
        return Err(ParseError::ExtraFields { op: "SUB" });
    }

    let sid = tokens[0];
    let message_name = tokens[1];
    let topic = tokens[2];
    let group = tokens.get(3).copied();

    if !validate_identifier(sid) {
        return Err(ParseError::InvalidIdentifier {
            op: "SUB",
            field: "sid",
            value: sid.to_owned(),
        });
    }

    if !validate_identifier(topic) {
        return Err(ParseError::InvalidIdentifier {
            op: "SUB",
            field: "topic",
            value: topic.to_owned(),
        });
    }

    if let Some(g) = group
        && !validate_identifier(g)
    {
        return Err(ParseError::InvalidIdentifier {
            op: "SUB",
            field: "group",
            value: g.to_owned(),
        });
    }

    Ok(ClientOp::Sub {
        sid: sid.to_owned(),
        message_name: message_name.to_owned(),
        topic: topic.to_owned(),
        group: group.map(str::to_owned),
    })
}

fn parse_unsub(rest: &str) -> Result<ClientOp, ParseError> {
    let tokens = tokenize(rest, 2);

    if tokens.is_empty() {
        return Err(ParseError::MissingField {
            op: "UNSUB",
            field: "sid",
        });
    }

    if tokens.len() > 1 {
        return Err(ParseError::ExtraFields { op: "UNSUB" });
    }

    let sid = tokens[0];

    if !validate_identifier(sid) {
        return Err(ParseError::InvalidIdentifier {
            op: "UNSUB",
            field: "sid",
            value: sid.to_owned(),
        });
    }

    Ok(ClientOp::Unsub {
        sid: sid.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── INIT parsing ──────────────────────────────────────────────────────

    #[test]
    fn init_valid_all_fields() {
        let frame =
            r#"INIT {"versions":["1.0"],"schema":"002.5.0","verbose":false,"service_id":"svc-1"}"#;
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Init(p) => {
                assert_eq!(p.versions, vec!["1.0"]);
                assert_eq!(p.schema, "002.5.0");
                assert_eq!(p.verbose, Some(false));
                assert_eq!(p.service_id, "svc-1");
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn init_valid_without_verbose() {
        let frame = r#"INIT {"versions":["1.0"],"schema":"002.5.0","service_id":"tracker"}"#;
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Init(p) => assert!(p.verbose.is_none()),
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn init_empty_versions() {
        let frame = r#"INIT {"versions":[],"schema":"002.5.0","service_id":"svc-1"}"#;
        let err = parse(frame).unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "INIT",
                field: "versions"
            }
        ));
    }

    #[test]
    fn init_malformed_json() {
        let frame = r"INIT {not valid json}";
        let err = parse(frame).unwrap_err();
        assert!(matches!(err, ParseError::InvalidJson { op: "INIT", .. }));
    }

    #[test]
    fn init_missing_schema_field() {
        let frame = r#"INIT {"versions":["1.0"],"service_id":"svc-1"}"#;
        let err = parse(frame).unwrap_err();
        assert!(matches!(err, ParseError::InvalidJson { op: "INIT", .. }));
    }

    #[test]
    fn init_no_json() {
        let frame = "INIT";
        let err = parse(frame).unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "INIT",
                field: "payload"
            }
        ));
    }

    #[test]
    fn init_invalid_service_id() {
        let frame = r#"INIT {"versions":["1.0"],"schema":"002.5.0","service_id":"bad!id"}"#;
        let err = parse(frame).unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "INIT",
                field: "service_id",
                ..
            }
        ));
    }

    #[test]
    fn init_extra_whitespace_before_json() {
        let frame =
            "INIT  \t  {\"versions\":[\"1.0\"],\"schema\":\"002.5.0\",\"service_id\":\"svc-1\"}";
        let op = parse(frame).unwrap();
        assert!(matches!(op, ClientOp::Init(_)));
    }

    // ── PUB parsing ───────────────────────────────────────────────────────

    #[test]
    fn pub_valid() {
        let frame = r#"PUB mission {"AccessAssessment":{}}"#;
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Pub { topic, payload } => {
                assert_eq!(topic, "mission");
                assert_eq!(payload, r#"{"AccessAssessment":{}}"#);
            }
            other => panic!("expected Pub, got {other:?}"),
        }
    }

    #[test]
    fn pub_multiline_json() {
        let frame = "PUB topic1 {\n  \"Foo\": {\n    \"bar\": 1\n  }\n}";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Pub { payload, .. } => {
                assert!(payload.contains('\n'));
                assert!(payload.contains("\"bar\": 1"));
            }
            other => panic!("expected Pub, got {other:?}"),
        }
    }

    #[test]
    fn pub_spaces_inside_json() {
        let frame = r#"PUB topic1 {"key": "value with spaces"}"#;
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Pub { payload, .. } => {
                assert_eq!(payload, r#"{"key": "value with spaces"}"#);
            }
            other => panic!("expected Pub, got {other:?}"),
        }
    }

    #[test]
    fn pub_tab_delimiters() {
        let frame = "PUB\ttopic1\t{\"a\":1}";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Pub { topic, payload } => {
                assert_eq!(topic, "topic1");
                assert_eq!(payload, "{\"a\":1}");
            }
            other => panic!("expected Pub, got {other:?}"),
        }
    }

    #[test]
    fn pub_missing_topic() {
        let err = parse("PUB").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "PUB",
                field: "topic"
            }
        ));
    }

    #[test]
    fn pub_invalid_topic() {
        let frame = r#"PUB bad!topic {"a":1}"#;
        let err = parse(frame).unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "PUB",
                field: "topic",
                ..
            }
        ));
    }

    #[test]
    fn pub_missing_payload() {
        let err = parse("PUB topic1").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "PUB",
                field: "payload"
            }
        ));
    }

    // ── SUB parsing ───────────────────────────────────────────────────────

    #[test]
    fn sub_valid_no_group() {
        let frame = "SUB sub1 AccessAssessment mission";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Sub {
                sid,
                message_name,
                topic,
                group,
            } => {
                assert_eq!(sid, "sub1");
                assert_eq!(message_name, "AccessAssessment");
                assert_eq!(topic, "mission");
                assert!(group.is_none());
            }
            other => panic!("expected Sub, got {other:?}"),
        }
    }

    #[test]
    fn sub_valid_with_group() {
        let frame = "SUB sub1 AccessAssessment mission group-a";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Sub { group, .. } => {
                assert_eq!(group.as_deref(), Some("group-a"));
            }
            other => panic!("expected Sub, got {other:?}"),
        }
    }

    #[test]
    fn sub_missing_fields_zero() {
        let err = parse("SUB").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "SUB",
                field: "sid"
            }
        ));
    }

    #[test]
    fn sub_missing_fields_one() {
        let err = parse("SUB sub1").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "SUB",
                field: "message_name"
            }
        ));
    }

    #[test]
    fn sub_missing_fields_two() {
        let err = parse("SUB sub1 MsgA").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "SUB",
                field: "topic"
            }
        ));
    }

    #[test]
    fn sub_extra_fields() {
        let err = parse("SUB sub1 MsgA topic1 grp extra").unwrap_err();
        assert!(matches!(err, ParseError::ExtraFields { op: "SUB" }));
    }

    #[test]
    fn sub_invalid_sid() {
        let err = parse("SUB bad!sid MsgA topic1").unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "SUB",
                field: "sid",
                ..
            }
        ));
    }

    #[test]
    fn sub_invalid_topic() {
        let err = parse("SUB sub1 MsgA bad!topic").unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "SUB",
                field: "topic",
                ..
            }
        ));
    }

    #[test]
    fn sub_invalid_group() {
        let err = parse("SUB sub1 MsgA topic1 bad!group").unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "SUB",
                field: "group",
                ..
            }
        ));
    }

    #[test]
    fn sub_namespace_message_name() {
        let frame = "SUB sub1 {uci.program.extension.gov}MyExtension topic1";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Sub { message_name, .. } => {
                assert_eq!(message_name, "{uci.program.extension.gov}MyExtension");
            }
            other => panic!("expected Sub, got {other:?}"),
        }
    }

    // ── UNSUB parsing ─────────────────────────────────────────────────────

    #[test]
    fn unsub_valid() {
        let frame = "UNSUB sub1";
        let op = parse(frame).unwrap();
        match op {
            ClientOp::Unsub { sid } => assert_eq!(sid, "sub1"),
            other => panic!("expected Unsub, got {other:?}"),
        }
    }

    #[test]
    fn unsub_no_sid() {
        let err = parse("UNSUB").unwrap_err();
        assert!(matches!(
            err,
            ParseError::MissingField {
                op: "UNSUB",
                field: "sid"
            }
        ));
    }

    #[test]
    fn unsub_extra_fields() {
        let err = parse("UNSUB sub1 extra").unwrap_err();
        assert!(matches!(err, ParseError::ExtraFields { op: "UNSUB" }));
    }

    #[test]
    fn unsub_invalid_sid() {
        let err = parse("UNSUB bad!sid").unwrap_err();
        assert!(matches!(
            err,
            ParseError::InvalidIdentifier {
                op: "UNSUB",
                field: "sid",
                ..
            }
        ));
    }

    // ── Edge cases ────────────────────────────────────────────────────────

    #[test]
    fn empty_frame() {
        let err = parse("").unwrap_err();
        assert!(matches!(err, ParseError::EmptyFrame));
    }

    #[test]
    fn unknown_op() {
        let err = parse("CONNECT foo").unwrap_err();
        match err {
            ParseError::UnknownOp(op) => assert_eq!(op, "CONNECT"),
            other => panic!("expected UnknownOp, got {other:?}"),
        }
    }

    #[test]
    fn lowercase_op() {
        let err = parse(r#"init {"versions":["1.0"],"schema":"002.5.0","service_id":"svc-1"}"#)
            .unwrap_err();
        assert!(matches!(err, ParseError::UnknownOp(_)));
    }

    #[test]
    fn whitespace_only_frame() {
        let err = parse("   \t  ").unwrap_err();
        assert!(matches!(err, ParseError::EmptyFrame));
    }

    #[test]
    fn consecutive_delimiters() {
        let frame = "SUB  sub1  MsgA  topic1";
        let op = parse(frame).unwrap();
        assert!(matches!(op, ClientOp::Sub { .. }));
    }

    #[test]
    fn tab_delimiters() {
        let frame = "SUB\tsub1\tMsgA\ttopic1";
        let op = parse(frame).unwrap();
        assert!(matches!(op, ClientOp::Sub { .. }));
    }

    #[test]
    fn mixed_delimiters() {
        let frame = "SUB \t sub1 \t MsgA \t topic1";
        let op = parse(frame).unwrap();
        assert!(matches!(op, ClientOp::Sub { .. }));
    }

    // ── ServerOp serialization ────────────────────────────────────────────

    #[test]
    fn server_op_ok() {
        assert_eq!(ServerOp::Ok.to_string(), "+OK");
    }

    #[test]
    fn server_op_err_no_details() {
        let op = ServerOp::Err {
            error: OwpError::IllegalArgument,
            details: None,
        };
        assert_eq!(op.to_string(), "-ERR Illegal-Argument");
    }

    #[test]
    fn server_op_err_with_details() {
        let op = ServerOp::Err {
            error: OwpError::InvalidMessage,
            details: Some("missing required field 'Foo'".to_owned()),
        };
        assert_eq!(
            op.to_string(),
            "-ERR Invalid-Message missing required field 'Foo'"
        );
    }

    #[test]
    fn server_op_info() {
        let payload = InfoPayload {
            version: "1.0".to_owned(),
            server_id: "sleet-1".to_owned(),
            uuids: Identifiers {
                system: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                service: "550e8400-e29b-41d4-a716-446655440001".to_owned(),
                subsystem: None,
                capabilities: None,
                components: None,
            },
            system_label: "Test Platform".to_owned(),
            connect_urls: None,
        };
        let op = ServerOp::Info(Box::new(payload));
        let s = op.to_string();
        assert!(s.starts_with("INFO {"));
        let json_str = &s[5..];
        let v: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(v["version"], "1.0");
        assert_eq!(v["server_id"], "sleet-1");
        assert_eq!(v["uuids"]["system"], "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn server_op_msg() {
        let op = ServerOp::Msg {
            sid: "sub1".to_owned(),
            payload: std::sync::Arc::from(r#"{"Foo":{}}"#),
        };
        assert_eq!(op.to_string(), r#"MSG sub1 {"Foo":{}}"#);
    }

    // ── parse_server ──────────────────────────────────────────────────────

    #[test]
    fn parse_server_ok() {
        let op = parse_server("+OK").unwrap();
        assert!(matches!(op, ServerOp::Ok));
    }

    #[test]
    fn parse_server_err_no_details() {
        let op = parse_server("-ERR Illegal-State").unwrap();
        assert!(matches!(
            op,
            ServerOp::Err {
                error: OwpError::IllegalState,
                details: None
            }
        ));
    }

    #[test]
    fn parse_server_err_with_details() {
        let op = parse_server("-ERR Invalid-Message missing field 'Id'").unwrap();
        assert!(matches!(
            op,
            ServerOp::Err {
                error: OwpError::InvalidMessage,
                details: Some(ref details)
            } if details == "missing field 'Id'"
        ));
    }

    #[test]
    fn parse_server_info() {
        let frame = "INFO {\"version\":\"1.0\",\"server_id\":\"sleet-1\",\"uuids\":{\"system\":\"550e8400-e29b-41d4-a716-446655440000\",\"service\":\"550e8400-e29b-41d4-a716-446655440001\"},\"system_label\":\"Test\"}";
        let op = parse_server(frame).unwrap();

        match op {
            ServerOp::Info(payload) => {
                assert_eq!(payload.version, "1.0");
                assert_eq!(payload.server_id, "sleet-1");
                assert_eq!(payload.system_label, "Test");
                assert_eq!(payload.uuids.system, "550e8400-e29b-41d4-a716-446655440000");
            }
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn parse_server_msg() {
        let op = parse_server(r#"MSG sub1 {"Msg":{}}"#).unwrap();
        assert!(matches!(
            op,
            ServerOp::Msg { ref sid, ref payload }
            if sid == "sub1" && &**payload == r#"{"Msg":{}}"#
        ));
    }

    #[test]
    fn parse_server_msg_payload_with_spaces() {
        let op = parse_server(r#"MSG s1 {"key": "value with spaces"}"#).unwrap();
        assert!(matches!(
            op,
            ServerOp::Msg { ref sid, ref payload }
            if sid == "s1" && &**payload == r#"{"key": "value with spaces"}"#
        ));
    }

    #[test]
    fn parse_server_unknown_keyword() {
        let err = parse_server("CONNECT foo").unwrap_err();
        assert!(matches!(err, ParseError::UnknownOp(ref op) if op == "CONNECT"));
    }

    #[test]
    fn parse_server_unknown_error_name() {
        let err = parse_server("-ERR Made-Up-Error details").unwrap_err();
        assert!(matches!(err, ParseError::UnknownOp(ref op) if op == "Made-Up-Error"));
    }

    #[test]
    fn parse_server_roundtrip_all_variants() {
        let info = ServerOp::Info(Box::new(InfoPayload {
            version: "1.0".to_owned(),
            server_id: "sleet-1".to_owned(),
            uuids: Identifiers {
                system: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                service: "550e8400-e29b-41d4-a716-446655440001".to_owned(),
                subsystem: None,
                capabilities: None,
                components: None,
            },
            system_label: "Test".to_owned(),
            connect_urls: Some(vec!["ws://127.0.0.1:9000".to_owned()]),
        }));

        let cases = vec![
            ServerOp::Ok,
            ServerOp::Err {
                error: OwpError::IllegalArgument,
                details: None,
            },
            ServerOp::Err {
                error: OwpError::InvalidMessage,
                details: Some("bad payload".to_owned()),
            },
            ServerOp::Msg {
                sid: "sub1".to_owned(),
                payload: std::sync::Arc::from(r#"{"Msg":{}}"#),
            },
            info,
        ];

        for op in &cases {
            let frame = op.to_string();
            let parsed = parse_server(&frame).unwrap();
            match (op, parsed) {
                (ServerOp::Ok, ServerOp::Ok) => {}
                (
                    ServerOp::Err {
                        error: expected_error,
                        details: expected_details,
                    },
                    ServerOp::Err { error, details },
                ) => {
                    assert_eq!(error.to_string(), expected_error.to_string());
                    assert_eq!(&details, expected_details);
                }
                (
                    ServerOp::Msg {
                        sid: expected_sid,
                        payload: expected_payload,
                    },
                    ServerOp::Msg { sid, payload },
                ) => {
                    assert_eq!(&sid, expected_sid);
                    assert_eq!(&payload, expected_payload);
                }
                (ServerOp::Info(expected), ServerOp::Info(actual)) => {
                    assert_eq!(actual.version, expected.version);
                    assert_eq!(actual.server_id, expected.server_id);
                    assert_eq!(actual.system_label, expected.system_label);
                    assert_eq!(actual.connect_urls, expected.connect_urls);
                    assert_eq!(actual.uuids.system, expected.uuids.system);
                    assert_eq!(actual.uuids.service, expected.uuids.service);
                }
                (expected, actual) => {
                    panic!("roundtrip mismatch: expected {expected:?}, got {actual:?}")
                }
            }
        }
    }

    // ── OwpError Display ──────────────────────────────────────────────────

    #[test]
    fn owp_error_display_all_variants() {
        let cases = [
            (OwpError::UnsupportedVersion, "Unsupported-Version"),
            (OwpError::UnsupportedSchema, "Unsupported-Schema"),
            (OwpError::UnsupportedService, "Unsupported-Service"),
            (OwpError::IllegalOperation, "Illegal-Operation"),
            (OwpError::IllegalArgument, "Illegal-Argument"),
            (OwpError::IllegalState, "Illegal-State"),
            (OwpError::InternalError, "Internal-Error"),
            (OwpError::InvalidMessage, "Invalid-Message"),
        ];
        for (variant, expected) in &cases {
            assert_eq!(variant.to_string(), *expected);
        }
    }
}
