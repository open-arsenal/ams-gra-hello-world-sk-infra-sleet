//! Configuration loading, parsing, and validation.
//!
//! This module defines the server and per-service configuration structures
//! and provides the logic to load them from TOML files on disk.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sleet_types::owp::validate_identifier;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Validated server-level configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Socket address to bind the TCP listener on.
    pub bind_addr: SocketAddr,
    /// Unique server identifier (OWP identifier format).
    pub server_id: String,
    /// Human-readable platform label sent in INFO responses.
    pub system_label: String,
    /// Path to the OMS XSD schema file.
    pub schema_path: PathBuf,
    /// Expected schema version string (e.g., `"002.5.0"`).
    pub schema_version: String,
    /// System UUID for the platform.
    pub system_uuid: Uuid,
    /// Subsystem UUID for the platform.
    pub subsystem_uuid: Uuid,
    /// Directory containing per-service TOML files.
    pub services_dir: PathBuf,
    /// Log output format.
    pub log_format: String,
    /// Optional tracing filter directive from configuration.
    pub log_level: Option<String>,
    /// Optional OTLP collector endpoint.
    pub otlp_endpoint: Option<String>,
    /// Grace period in seconds for draining connections during shutdown.
    pub shutdown_drain_seconds: u64,
    /// Optional admin HTTP listener for `/metrics` and `/health`.
    pub metrics_bind_addr: Option<SocketAddr>,
    /// Idle timeout in seconds for connection liveness probing.
    ///
    /// A value of `0` disables idle timeout.
    pub idle_timeout_seconds: u64,
    /// WebSocket maximum payload size in bytes.
    pub max_message_size: usize,
    /// WebSocket maximum frame size in bytes.
    pub max_frame_size: usize,
    /// WebSocket maximum outbound write buffer size in bytes.
    pub max_write_buffer_size: usize,
    /// Maximum concurrent TCP connections allowed.
    pub max_connections: usize,
}

/// Validated per-service configuration.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Unique service identifier (OWP identifier format).
    pub service_id: String,
    /// Service UUID.
    pub service_uuid: Uuid,
    /// Topics this service is authorized to PUB/SUB on.
    pub allowed_topics: Vec<String>,
    /// Optional per-topic message type restrictions.
    ///
    /// Maps topic names to the set of OMS message names (global element names)
    /// allowed on that topic. Topics absent from this map accept any schema-valid
    /// message (permissive default).
    pub topic_bindings: HashMap<String, HashSet<String>>,
}

/// Top-level validated configuration containing server settings and services.
#[derive(Debug, Clone)]
pub struct Config {
    /// Server-level settings.
    pub server: ServerConfig,
    /// Configured services loaded from the services directory.
    pub services: Vec<Arc<ServiceConfig>>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors encountered while loading or validating configuration files.
#[derive(Debug)]
pub enum ConfigError {
    /// File I/O error (file not found, permission denied, etc.).
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// TOML syntax error.
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// A required field is missing or empty.
    MissingField { path: PathBuf, field: &'static str },
    /// A field value is not a valid OWP identifier.
    InvalidIdentifier {
        path: PathBuf,
        field: &'static str,
        value: String,
    },
    /// A field value is not a valid UUID.
    InvalidUuid {
        path: PathBuf,
        field: &'static str,
        value: String,
    },
    /// The `bind_addr` field is not a valid socket address.
    InvalidBindAddr { path: PathBuf, value: String },
    /// Two service files declare the same `service_id`.
    DuplicateServiceId { id: String, path: PathBuf },
    /// Two service files declare the same `service_uuid`.
    DuplicateServiceUuid { uuid: String, path: PathBuf },
    /// The `schema_path` does not exist on disk.
    SchemaPathNotFound { path: PathBuf },
    /// The `services_dir` does not exist or is not a directory.
    ServicesDirNotFound { path: PathBuf },
    /// The `services_dir` contains no service TOML files.
    NoServicesConfigured { path: PathBuf },
    /// The `log_format` field is present but not one of the supported values.
    InvalidLogFormat { path: PathBuf, value: String },
    /// The shutdown drain period is outside the accepted range.
    InvalidDrainPeriod { path: PathBuf, value: u64 },
    /// The `metrics_bind_addr` field is not a valid socket address.
    InvalidMetricsBindAddr { path: PathBuf, value: String },
    /// The idle timeout is outside the accepted range.
    InvalidIdleTimeout { path: PathBuf, value: u64 },
    /// A `topic_bindings` entry references a topic not in `allowed_topics`.
    TopicBindingInvalidTopic { path: PathBuf, topic: String },
    /// Two `topic_bindings` entries declare the same topic.
    TopicBindingDuplicateTopic { path: PathBuf, topic: String },
    /// A `topic_bindings` entry has an empty `allowed_messages` list.
    TopicBindingEmptyMessages { path: PathBuf, topic: String },
}

impl fmt::Display for ConfigError {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            Self::MissingField { path, field } => {
                write!(f, "{}: missing required field '{field}'", path.display())
            }
            Self::InvalidIdentifier { path, field, value } => {
                write!(
                    f,
                    "{}: field '{field}' value '{value}' is not a valid OWP identifier",
                    path.display()
                )
            }
            Self::InvalidUuid { path, field, value } => {
                write!(
                    f,
                    "{}: field '{field}' value '{value}' is not a valid UUID",
                    path.display()
                )
            }
            Self::InvalidBindAddr { path, value } => {
                write!(
                    f,
                    "{}: bind_addr '{value}' is not a valid socket address",
                    path.display()
                )
            }
            Self::DuplicateServiceId { id, path } => {
                write!(f, "{}: duplicate service_id '{id}'", path.display())
            }
            Self::DuplicateServiceUuid { uuid, path } => {
                write!(f, "{}: duplicate service_uuid '{uuid}'", path.display())
            }
            Self::SchemaPathNotFound { path } => {
                write!(f, "schema_path '{}' does not exist", path.display())
            }
            Self::ServicesDirNotFound { path } => {
                write!(f, "services_dir '{}' does not exist", path.display())
            }
            Self::NoServicesConfigured { path } => {
                write!(
                    f,
                    "services_dir '{}' contains no service TOML files",
                    path.display()
                )
            }
            Self::InvalidLogFormat { path, value } => {
                write!(
                    f,
                    "{}: log_format '{}' must be 'text' or 'json'",
                    path.display(),
                    value
                )
            }
            Self::InvalidDrainPeriod { path, value } => {
                write!(
                    f,
                    "{}: shutdown_drain_seconds '{}' must be in 1..=30",
                    path.display(),
                    value
                )
            }
            Self::InvalidMetricsBindAddr { path, value } => {
                write!(
                    f,
                    "{}: metrics_bind_addr '{}' is not a valid socket address",
                    path.display(),
                    value
                )
            }
            Self::InvalidIdleTimeout { path, value } => {
                write!(
                    f,
                    "{}: idle_timeout_seconds '{}' must be 0 or in 10..=3600",
                    path.display(),
                    value
                )
            }
            Self::TopicBindingInvalidTopic { path, topic } => {
                write!(
                    f,
                    "{}: topic_bindings topic '{}' is not in allowed_topics",
                    path.display(),
                    topic
                )
            }
            Self::TopicBindingDuplicateTopic { path, topic } => {
                write!(
                    f,
                    "{}: duplicate topic_bindings topic '{}'",
                    path.display(),
                    topic
                )
            }
            Self::TopicBindingEmptyMessages { path, topic } => {
                write!(
                    f,
                    "{}: topic_bindings for '{}' has empty allowed_messages",
                    path.display(),
                    topic
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Raw (serde) structs — fields read via deserialization
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawServerConfig {
    bind_addr: Option<String>,
    server_id: Option<String>,
    system_label: Option<String>,
    schema_path: Option<String>,
    schema_version: Option<String>,
    system_name: Option<String>,
    subsystem_name: Option<String>,
    namespace_uuid: Option<String>,
    system_uuid: Option<String>,
    subsystem_uuid: Option<String>,
    services_dir: Option<String>,
    log_format: Option<String>,
    log_level: Option<String>,
    otlp_endpoint: Option<String>,
    shutdown_drain_seconds: Option<u64>,
    metrics_bind_addr: Option<String>,
    idle_timeout_seconds: Option<u64>,
    max_message_size: Option<usize>,
    max_frame_size: Option<usize>,
    max_write_buffer_size: Option<usize>,
    max_connections: Option<usize>,
}

/// Raw per-topic message type binding read from TOML `[[topic_bindings]]`.
#[derive(Deserialize)]
struct RawTopicBinding {
    topic: Option<String>,
    allowed_messages: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawServiceConfig {
    service_id: Option<String>,
    service_uuid: Option<String>,
    allowed_topics: Option<Vec<String>>,
    topic_bindings: Option<Vec<RawTopicBinding>>,
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn require_field<'a>(
    value: Option<&'a String>,
    field: &'static str,
    path: &Path,
) -> Result<&'a str, ConfigError> {
    match value {
        Some(v) if !v.is_empty() => Ok(v.as_str()),
        _ => Err(ConfigError::MissingField {
            path: path.to_path_buf(),
            field,
        }),
    }
}

fn parse_identifier(
    value: Option<&String>,
    field: &'static str,
    path: &Path,
) -> Result<String, ConfigError> {
    let s = require_field(value, field, path)?;
    if validate_identifier(s) {
        Ok(s.to_owned())
    } else {
        Err(ConfigError::InvalidIdentifier {
            path: path.to_path_buf(),
            field,
            value: s.to_owned(),
        })
    }
}

fn parse_uuid(
    value: Option<&String>,
    field: &'static str,
    path: &Path,
) -> Result<Uuid, ConfigError> {
    let s = require_field(value, field, path)?;
    Uuid::parse_str(s).map_err(|_| ConfigError::InvalidUuid {
        path: path.to_path_buf(),
        field,
        value: s.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load and validate configuration from a server TOML file.
///
/// The server file's `services_dir` field points to a directory containing
/// zero or more `*.toml` service definition files.
pub fn load(server_path: &Path) -> Result<Config, ConfigError> {
    let server = load_server(server_path)?;
    let services = load_services(&server.services_dir)?;
    Ok(Config {
        server,
        services: services.into_iter().map(Arc::new).collect(),
    })
}

fn load_server(server_path: &Path) -> Result<ServerConfig, ConfigError> {
    load_server_with_env(server_path, std::env::var("SLEET_NAMESPACE_UUID").ok())
}

#[allow(clippy::too_many_lines)]
fn load_server_with_env(
    server_path: &Path,
    env_namespace: Option<String>,
) -> Result<ServerConfig, ConfigError> {
    let server_text = std::fs::read_to_string(server_path).map_err(|e| ConfigError::Io {
        path: server_path.to_path_buf(),
        source: e,
    })?;

    let raw: RawServerConfig = toml::from_str(&server_text).map_err(|e| ConfigError::Parse {
        path: server_path.to_path_buf(),
        source: e,
    })?;

    let server_id = parse_identifier(raw.server_id.as_ref(), "server_id", server_path)?;
    let system_label =
        require_field(raw.system_label.as_ref(), "system_label", server_path)?.to_owned();

    let bind_addr_str = require_field(raw.bind_addr.as_ref(), "bind_addr", server_path)?;
    let bind_addr: SocketAddr =
        bind_addr_str
            .parse()
            .map_err(|_| ConfigError::InvalidBindAddr {
                path: server_path.to_path_buf(),
                value: bind_addr_str.to_owned(),
            })?;

    let schema_path_str = require_field(raw.schema_path.as_ref(), "schema_path", server_path)?;
    let schema_path = PathBuf::from(schema_path_str);
    if !schema_path.exists() {
        return Err(ConfigError::SchemaPathNotFound { path: schema_path });
    }

    let schema_version =
        require_field(raw.schema_version.as_ref(), "schema_version", server_path)?.to_owned();

    let namespace_uuid_str = env_namespace
        .or_else(|| raw.namespace_uuid.clone())
        .unwrap_or_else(|| "507e1ce1-1111-4444-8888-507e1ce11111".to_owned());
    let namespace_uuid =
        Uuid::parse_str(&namespace_uuid_str).map_err(|_| ConfigError::InvalidUuid {
            path: server_path.to_path_buf(),
            field: "namespace_uuid",
            value: namespace_uuid_str.clone(),
        })?;

    let system_name = raw.system_name.unwrap_or_else(|| "supercell".to_owned());
    let subsystem_name = raw
        .subsystem_name
        .unwrap_or_else(|| "supercell.subsystem.sleet".to_owned());

    let system_uuid = if let Some(uuid_str) = raw.system_uuid {
        Uuid::parse_str(&uuid_str).map_err(|_| ConfigError::InvalidUuid {
            path: server_path.to_path_buf(),
            field: "system_uuid",
            value: uuid_str,
        })?
    } else {
        Uuid::new_v5(&namespace_uuid, system_name.as_bytes())
    };

    let subsystem_uuid = if let Some(uuid_str) = raw.subsystem_uuid {
        Uuid::parse_str(&uuid_str).map_err(|_| ConfigError::InvalidUuid {
            path: server_path.to_path_buf(),
            field: "subsystem_uuid",
            value: uuid_str,
        })?
    } else {
        Uuid::new_v5(&namespace_uuid, subsystem_name.as_bytes())
    };

    let services_dir_str = require_field(raw.services_dir.as_ref(), "services_dir", server_path)?;
    let services_dir = PathBuf::from(services_dir_str);
    if !services_dir.is_dir() {
        return Err(ConfigError::ServicesDirNotFound { path: services_dir });
    }

    let log_format = raw.log_format.unwrap_or_else(|| "text".to_owned());
    if log_format != "text" && log_format != "json" {
        return Err(ConfigError::InvalidLogFormat {
            path: server_path.to_path_buf(),
            value: log_format,
        });
    }

    let shutdown_drain_seconds = raw.shutdown_drain_seconds.unwrap_or(5);
    if !(1..=30).contains(&shutdown_drain_seconds) {
        return Err(ConfigError::InvalidDrainPeriod {
            path: server_path.to_path_buf(),
            value: shutdown_drain_seconds,
        });
    }

    let metrics_bind_addr = if let Ok(port_str) = std::env::var("SLEET_METRICS_PORT") {
        let port = port_str
            .parse::<u16>()
            .map_err(|_| ConfigError::InvalidMetricsBindAddr {
                path: server_path.to_path_buf(),
                value: format!("invalid SLEET_METRICS_PORT: {port_str}"),
            })?;
        let ip = if let Some(bind_addr_str) = raw.metrics_bind_addr.as_deref() {
            let addr: std::net::SocketAddr =
                bind_addr_str
                    .parse()
                    .map_err(|_| ConfigError::InvalidMetricsBindAddr {
                        path: server_path.to_path_buf(),
                        value: bind_addr_str.to_owned(),
                    })?;
            addr.ip()
        } else {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        };
        Some(std::net::SocketAddr::new(ip, port))
    } else if let Some(bind_addr_str) = raw.metrics_bind_addr.as_deref() {
        Some(
            bind_addr_str
                .parse()
                .map_err(|_| ConfigError::InvalidMetricsBindAddr {
                    path: server_path.to_path_buf(),
                    value: bind_addr_str.to_owned(),
                })?,
        )
    } else {
        None
    };

    let idle_timeout_seconds = raw.idle_timeout_seconds.unwrap_or(300);
    if idle_timeout_seconds != 0 && !(10..=3600).contains(&idle_timeout_seconds) {
        return Err(ConfigError::InvalidIdleTimeout {
            path: server_path.to_path_buf(),
            value: idle_timeout_seconds,
        });
    }

    Ok(ServerConfig {
        bind_addr,
        server_id,
        system_label,
        schema_path,
        schema_version,
        system_uuid,
        subsystem_uuid,
        services_dir,
        log_format,
        log_level: raw.log_level,
        otlp_endpoint: raw.otlp_endpoint,
        shutdown_drain_seconds,
        metrics_bind_addr,
        idle_timeout_seconds,
        max_message_size: raw.max_message_size.unwrap_or(512 * 1024 * 1024),
        max_frame_size: raw.max_frame_size.unwrap_or(128 * 1024 * 1024),
        max_write_buffer_size: raw.max_write_buffer_size.unwrap_or(128 * 1024 * 1024),
        max_connections: raw.max_connections.unwrap_or(10_000),
    })
}

fn load_services(services_dir: &Path) -> Result<Vec<ServiceConfig>, ConfigError> {
    let mut services = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut seen_uuids: HashSet<Uuid> = HashSet::new();

    let mut entries: Vec<_> = std::fs::read_dir(services_dir)
        .map_err(|e| ConfigError::Io {
            path: services_dir.to_path_buf(),
            source: e,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ConfigError::Io {
            path: services_dir.to_path_buf(),
            source: e,
        })?;

    // Sort for deterministic error reporting
    entries.sort_by_key(std::fs::DirEntry::path);

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let svc = load_single_service(&path)?;

        if !seen_ids.insert(svc.service_id.clone()) {
            return Err(ConfigError::DuplicateServiceId {
                id: svc.service_id,
                path,
            });
        }
        if !seen_uuids.insert(svc.service_uuid) {
            return Err(ConfigError::DuplicateServiceUuid {
                uuid: svc.service_uuid.to_string(),
                path,
            });
        }

        services.push(svc);
    }

    if services.is_empty() {
        return Err(ConfigError::NoServicesConfigured {
            path: services_dir.to_path_buf(),
        });
    }

    Ok(services)
}

fn load_single_service(path: &Path) -> Result<ServiceConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let raw: RawServiceConfig = toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;

    let service_id = parse_identifier(raw.service_id.as_ref(), "service_id", path)?;
    let service_uuid = parse_uuid(raw.service_uuid.as_ref(), "service_uuid", path)?;

    let allowed_topics = match &raw.allowed_topics {
        Some(topics) => {
            for topic in topics {
                if !validate_identifier(topic) {
                    return Err(ConfigError::InvalidIdentifier {
                        path: path.to_path_buf(),
                        field: "allowed_topics",
                        value: topic.clone(),
                    });
                }
            }
            topics.clone()
        }
        None => {
            return Err(ConfigError::MissingField {
                path: path.to_path_buf(),
                field: "allowed_topics",
            });
        }
    };

    let topic_bindings = parse_topic_bindings(raw.topic_bindings, &allowed_topics, path)?;

    Ok(ServiceConfig {
        service_id,
        service_uuid,
        allowed_topics,
        topic_bindings,
    })
}

/// Parse and validate optional `[[topic_bindings]]` entries.
///
/// Each binding's topic must be a valid identifier present in `allowed_topics`.
/// Each `allowed_messages` list must be non-empty with valid identifiers.
/// Duplicate topics across bindings are rejected.
fn parse_topic_bindings(
    raw: Option<Vec<RawTopicBinding>>,
    allowed_topics: &[String],
    path: &Path,
) -> Result<HashMap<String, HashSet<String>>, ConfigError> {
    let Some(bindings) = raw else {
        return Ok(HashMap::new());
    };

    let mut map = HashMap::new();
    for binding in bindings {
        let topic = match &binding.topic {
            Some(t) if !t.is_empty() => {
                if !validate_identifier(t) {
                    return Err(ConfigError::InvalidIdentifier {
                        path: path.to_path_buf(),
                        field: "topic_bindings.topic",
                        value: t.clone(),
                    });
                }
                t.clone()
            }
            _ => {
                return Err(ConfigError::MissingField {
                    path: path.to_path_buf(),
                    field: "topic_bindings.topic",
                });
            }
        };

        if !allowed_topics.contains(&topic) {
            return Err(ConfigError::TopicBindingInvalidTopic {
                path: path.to_path_buf(),
                topic,
            });
        }

        let messages = match &binding.allowed_messages {
            Some(msgs) => {
                if msgs.is_empty() {
                    return Err(ConfigError::TopicBindingEmptyMessages {
                        path: path.to_path_buf(),
                        topic,
                    });
                }
                for msg in msgs {
                    if !validate_identifier(msg) {
                        return Err(ConfigError::InvalidIdentifier {
                            path: path.to_path_buf(),
                            field: "topic_bindings.allowed_messages",
                            value: msg.clone(),
                        });
                    }
                }
                msgs.iter().cloned().collect::<HashSet<String>>()
            }
            None => {
                return Err(ConfigError::MissingField {
                    path: path.to_path_buf(),
                    field: "topic_bindings.allowed_messages",
                });
            }
        };

        if map.contains_key(&topic) {
            return Err(ConfigError::TopicBindingDuplicateTopic {
                path: path.to_path_buf(),
                topic,
            });
        }

        map.insert(topic, messages);
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper for creating temporary config directory layouts.
    struct TestFixture {
        dir: tempfile::TempDir,
    }

    impl TestFixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn server_path(&self) -> PathBuf {
            self.dir.path().join("server.toml")
        }

        fn services_dir(&self) -> PathBuf {
            self.dir.path().join("services.d")
        }

        fn schema_path(&self) -> PathBuf {
            self.dir.path().join("schema.xsd")
        }

        /// Write a valid server config with sensible defaults.
        fn write_server(&self, overrides: &str) -> PathBuf {
            let services_dir = self.services_dir();
            fs::create_dir_all(&services_dir).unwrap();

            let schema_path = self.schema_path();
            fs::write(&schema_path, "<xs:schema/>").unwrap();

            let content = if overrides.is_empty() {
                format!(
                    "bind_addr = \"127.0.0.1:9000\"\n\
                     server_id = \"sleet-1\"\n\
                     system_label = \"Test Platform\"\n\
                     schema_path = \"{}\"\n\
                     schema_version = \"002.5.0\"\n\
                     system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                     services_dir = \"{}\"",
                    schema_path.display(),
                    services_dir.display(),
                )
            } else {
                overrides.to_owned()
            };

            let path = self.server_path();
            fs::write(&path, content).unwrap();
            path
        }

        /// Write a server config using a closure that receives `schema_path`
        /// and `services_dir` (both already created on disk).
        fn write_server_with<F: FnOnce(&Path, &Path) -> String>(&self, f: F) -> PathBuf {
            let services_dir = self.services_dir();
            fs::create_dir_all(&services_dir).unwrap();

            let schema_path = self.schema_path();
            fs::write(&schema_path, "<xs:schema/>").unwrap();

            let content = f(&schema_path, &services_dir);
            let path = self.server_path();
            fs::write(&path, content).unwrap();
            path
        }

        fn write_service(&self, filename: &str, content: &str) {
            let services_dir = self.services_dir();
            fs::create_dir_all(&services_dir).unwrap();
            fs::write(services_dir.join(filename), content).unwrap();
        }

        fn write_default_service(&self) {
            self.write_service(
                "svc1.toml",
                "service_id = \"svc1\"\n\
                 service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
                 allowed_topics = [\"mission\"]\n",
            );
        }
    }

    // -- Identifier validation --

    #[test]
    fn valid_identifiers() {
        assert!(validate_identifier("abc"));
        assert!(validate_identifier("ABC-123"));
        assert!(validate_identifier("my_service.v2"));
        assert!(validate_identifier("a"));
        assert!(validate_identifier("123"));
        assert!(validate_identifier("a-b.c_d"));
    }

    #[test]
    fn invalid_identifiers() {
        assert!(!validate_identifier(""));
        assert!(!validate_identifier("has space"));
        assert!(!validate_identifier("tab\there"));
        assert!(!validate_identifier("new\nline"));
        assert!(!validate_identifier("special!char"));
        assert!(!validate_identifier("slash/bad"));
        assert!(!validate_identifier("colon:bad"));
    }

    // -- Valid loading --

    #[test]
    fn load_valid_server_one_service() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"tracker\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\", \"tracks\"]\n",
        );

        let cfg = load(&server_path).unwrap();
        assert_eq!(cfg.server.server_id, "sleet-1");
        assert_eq!(cfg.server.bind_addr.to_string(), "127.0.0.1:9000");
        assert_eq!(cfg.server.schema_version, "002.5.0");
        assert_eq!(cfg.services.len(), 1);
        assert_eq!(cfg.services[0].service_id, "tracker");
        assert_eq!(cfg.services[0].allowed_topics, vec!["mission", "tracks"]);
    }

    #[test]
    fn load_valid_multiple_services() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc-a\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"topic1\"]\n",
        );
        fix.write_service(
            "svc2.toml",
            "service_id = \"svc-b\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440002\"\n\
             allowed_topics = [\"topic2\", \"topic3\"]\n",
        );

        let cfg = load(&server_path).unwrap();
        assert_eq!(cfg.services.len(), 2);

        let ids: HashSet<&str> = cfg.services.iter().map(|s| s.service_id.as_str()).collect();
        assert!(ids.contains("svc-a"));
        assert!(ids.contains("svc-b"));
    }

    #[test]
    fn error_no_services_configured() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::NoServicesConfigured { .. })
        ));
    }

    #[test]
    fn load_valid_empty_allowed_topics() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"heartbeat-only\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = []\n",
        );

        let cfg = load(&server_path).unwrap();
        assert!(cfg.services[0].allowed_topics.is_empty());
    }

    // -- Error: missing server file --

    #[test]
    fn error_missing_server_file() {
        let result = load(Path::new("/nonexistent/server.toml"));
        assert!(matches!(result, Err(ConfigError::Io { .. })));
    }

    // -- Error: malformed TOML --

    #[test]
    fn error_malformed_toml() {
        let fix = TestFixture::new();
        let path = fix.server_path();
        fs::write(&path, "this is not [valid toml").unwrap();
        let result = load(&path);
        assert!(matches!(result, Err(ConfigError::Parse { .. })));
    }

    // -- Error: missing required fields (server) --

    #[test]
    fn error_missing_bind_addr() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "bind_addr"),
            other => panic!("expected MissingField(bind_addr), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_server_id() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "server_id"),
            other => panic!("expected MissingField(server_id), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_system_label() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "system_label"),
            other => panic!("expected MissingField(system_label), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_schema_path() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|_schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                svc_dir.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "schema_path"),
            other => panic!("expected MissingField(schema_path), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_schema_version() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "schema_version"),
            other => panic!("expected MissingField(schema_version), got {other:?}"),
        }
    }

    #[test]
    fn system_uuid_generation_defaults() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        fix.write_default_service();

        let result = load(&path).expect("config should load successfully");

        let expected_ns = Uuid::parse_str("507e1ce1-1111-4444-8888-507e1ce11111").unwrap();
        let expected_sys = Uuid::new_v5(&expected_ns, b"supercell");
        let expected_sub = Uuid::new_v5(&expected_ns, b"supercell.subsystem.sleet");

        assert_eq!(result.server.system_uuid, expected_sys);
        assert_eq!(result.server.subsystem_uuid, expected_sub);
    }

    #[test]
    fn system_uuid_generation_with_env_namespace() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        fix.write_default_service();

        let custom_ns_str = "11111111-2222-3333-4444-555555555555";

        let result = load_server_with_env(&path, Some(custom_ns_str.to_owned()));

        let config = result.expect("config should load successfully");

        let expected_ns = Uuid::parse_str(custom_ns_str).unwrap();
        let expected_sys = Uuid::new_v5(&expected_ns, b"supercell");
        let expected_sub = Uuid::new_v5(&expected_ns, b"supercell.subsystem.sleet");

        assert_eq!(config.system_uuid, expected_sys);
        assert_eq!(config.subsystem_uuid, expected_sub);
    }

    #[test]
    fn error_missing_services_dir_field() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, _svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"",
                schema.display()
            )
        });
        let result = load(&path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "services_dir"),
            other => panic!("expected MissingField(services_dir), got {other:?}"),
        }
    }

    // -- Error: missing service fields --

    #[test]
    fn error_missing_service_id() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "bad.toml",
            "service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "service_id"),
            other => panic!("expected MissingField(service_id), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_service_uuid() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "bad.toml",
            "service_id = \"svc1\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "service_uuid"),
            other => panic!("expected MissingField(service_uuid), got {other:?}"),
        }
    }

    #[test]
    fn error_missing_allowed_topics() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "bad.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n",
        );
        let result = load(&server_path);
        match &result {
            Err(ConfigError::MissingField { field, .. }) => assert_eq!(*field, "allowed_topics"),
            other => panic!("expected MissingField(allowed_topics), got {other:?}"),
        }
    }

    // -- Error: invalid values --

    #[test]
    fn error_invalid_server_id() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"bad server!\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdentifier {
                field: "server_id",
                ..
            })
        ));
    }

    #[test]
    fn error_empty_server_id() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        // Empty string hits MissingField because require_field treats empty as missing
        assert!(matches!(
            result,
            Err(ConfigError::MissingField {
                field: "server_id",
                ..
            })
        ));
    }

    #[test]
    fn error_invalid_uuid() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"not-a-uuid\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidUuid {
                field: "system_uuid",
                ..
            })
        ));
    }

    #[test]
    fn error_invalid_bind_addr() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"not-an-address\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                schema.display(),
                svc_dir.display()
            )
        });
        let result = load(&path);
        assert!(matches!(result, Err(ConfigError::InvalidBindAddr { .. })));
    }

    #[test]
    fn error_schema_path_not_found() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|_schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"/nonexistent/schema.xsd\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"",
                svc_dir.display()
            )
        });
        let result = load(&path);
        assert!(matches!(
            result,
            Err(ConfigError::SchemaPathNotFound { .. })
        ));
    }

    #[test]
    fn error_services_dir_not_found() {
        let fix = TestFixture::new();
        let path = fix.write_server_with(|schema, _svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"/nonexistent/services\"",
                schema.display()
            )
        });
        let result = load(&path);
        assert!(matches!(
            result,
            Err(ConfigError::ServicesDirNotFound { .. })
        ));
    }

    // -- Error: duplicates --

    #[test]
    fn error_duplicate_service_id() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"same-id\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"t\"]\n",
        );
        fix.write_service(
            "svc2.toml",
            "service_id = \"same-id\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440002\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::DuplicateServiceId { .. })
        ));
    }

    #[test]
    fn error_duplicate_service_uuid() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc-a\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"t\"]\n",
        );
        fix.write_service(
            "svc2.toml",
            "service_id = \"svc-b\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::DuplicateServiceUuid { .. })
        ));
    }

    // -- Error: invalid topic in allowed_topics --

    #[test]
    fn error_invalid_topic_identifier() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"good-topic\", \"bad topic!\"]\n",
        );
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdentifier {
                field: "allowed_topics",
                ..
            })
        ));
    }

    // -- Non-.toml files ignored --

    #[test]
    fn non_toml_files_are_ignored_before_empty_service_check() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");

        let services_dir = fix.services_dir();
        fs::write(services_dir.join("README.md"), "# Not a config file").unwrap();
        fs::write(services_dir.join("backup.bak"), "garbage").unwrap();

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::NoServicesConfigured { .. })
        ));
    }

    // -- Error: malformed service TOML --

    #[test]
    fn error_malformed_service_toml() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service("bad.toml", "this is [not valid");
        let result = load(&server_path);
        assert!(matches!(result, Err(ConfigError::Parse { .. })));
    }

    // -- Error: invalid service identifier --

    #[test]
    fn error_invalid_service_id_identifier() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"has space\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdentifier {
                field: "service_id",
                ..
            })
        ));
    }

    // -- Error: invalid service UUID --

    #[test]
    fn error_invalid_service_uuid() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"not-a-uuid\"\n\
             allowed_topics = [\"t\"]\n",
        );
        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidUuid {
                field: "service_uuid",
                ..
            })
        ));
    }

    #[test]
    fn log_config_defaults_to_text_without_optional_fields() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_default_service();

        let cfg = load(&server_path).unwrap();
        assert_eq!(cfg.server.log_format, "text");
        assert!(cfg.server.log_level.is_none());
        assert!(cfg.server.otlp_endpoint.is_none());
        assert_eq!(cfg.server.shutdown_drain_seconds, 5);
        assert!(cfg.server.metrics_bind_addr.is_none());
        assert_eq!(cfg.server.idle_timeout_seconds, 300);
    }

    #[test]
    fn log_config_accepts_json_format_level_and_otlp_endpoint() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 log_format = \"json\"\n\
                 log_level = \"sleet=debug\"\n\
                 otlp_endpoint = \"http://localhost:4317\"\n\
                 shutdown_drain_seconds = 30\n\
                 metrics_bind_addr = \"127.0.0.1:21312\"\n\
                 idle_timeout_seconds = 3600",
                schema.display(),
                svc_dir.display()
            )
        });
        fix.write_default_service();

        let cfg = load(&server_path).unwrap();
        assert_eq!(cfg.server.log_format, "json");
        assert_eq!(cfg.server.log_level.as_deref(), Some("sleet=debug"));
        assert_eq!(
            cfg.server.otlp_endpoint.as_deref(),
            Some("http://localhost:4317")
        );
        assert_eq!(cfg.server.shutdown_drain_seconds, 30);
        assert_eq!(
            cfg.server.metrics_bind_addr.map(|addr| addr.to_string()),
            Some("127.0.0.1:21312".to_owned())
        );
        assert_eq!(cfg.server.idle_timeout_seconds, 3600);
    }

    #[test]
    fn log_config_rejects_invalid_format() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 log_format = \"pretty\"",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(result, Err(ConfigError::InvalidLogFormat { .. })));
    }

    #[test]
    fn drain_period_rejects_zero() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 shutdown_drain_seconds = 0",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidDrainPeriod { value: 0, .. })
        ));
    }

    #[test]
    fn drain_period_rejects_above_upper_bound() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 shutdown_drain_seconds = 31",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidDrainPeriod { value: 31, .. })
        ));
    }

    #[test]
    fn metrics_bind_addr_rejects_invalid_address() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 metrics_bind_addr = \"not-an-addr\"",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidMetricsBindAddr { .. })
        ));
    }

    #[test]
    fn idle_timeout_accepts_disabled_zero() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 idle_timeout_seconds = 0",
                schema.display(),
                svc_dir.display()
            )
        });
        fix.write_default_service();

        let cfg = load(&server_path).expect("config with disabled idle timeout should load");
        assert_eq!(cfg.server.idle_timeout_seconds, 0);
    }

    #[test]
    fn idle_timeout_rejects_below_lower_bound() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 idle_timeout_seconds = 5",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdleTimeout { value: 5, .. })
        ));
    }

    #[test]
    fn idle_timeout_rejects_above_upper_bound() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 idle_timeout_seconds = 3601",
                schema.display(),
                svc_dir.display()
            )
        });

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdleTimeout { value: 3601, .. })
        ));
    }

    // -- Topic bindings --

    #[test]
    fn topic_bindings_absent_defaults_to_empty() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n",
        );

        let cfg = load(&server_path).unwrap();
        assert!(cfg.services[0].topic_bindings.is_empty());
    }

    #[test]
    fn topic_bindings_valid_single_binding() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = [\"Msg\", \"Track\"]\n",
        );

        let cfg = load(&server_path).unwrap();
        let bindings = &cfg.services[0].topic_bindings;
        assert_eq!(bindings.len(), 1);
        let msgs = bindings.get("mission").unwrap();
        assert!(msgs.contains("Msg"));
        assert!(msgs.contains("Track"));
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn topic_bindings_valid_multiple_bindings() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\", \"tracks\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = [\"Msg\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"tracks\"\n\
             allowed_messages = [\"Track\"]\n",
        );

        let cfg = load(&server_path).unwrap();
        let bindings = &cfg.services[0].topic_bindings;
        assert_eq!(bindings.len(), 2);
        assert!(bindings.contains_key("mission"));
        assert!(bindings.contains_key("tracks"));
    }

    #[test]
    fn topic_bindings_topic_not_in_allowed_topics() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"other\"\n\
             allowed_messages = [\"Msg\"]\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::TopicBindingInvalidTopic { ref topic, .. }) if topic == "other"
        ));
    }

    #[test]
    fn topic_bindings_duplicate_topic() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = [\"Msg\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = [\"Track\"]\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::TopicBindingDuplicateTopic { ref topic, .. }) if topic == "mission"
        ));
    }

    #[test]
    fn topic_bindings_invalid_topic_identifier() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"bad topic!\"\n\
             allowed_messages = [\"Msg\"]\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdentifier {
                field: "topic_bindings.topic",
                ..
            })
        ));
    }

    #[test]
    fn topic_bindings_invalid_message_identifier() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = [\"bad message!\"]\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::InvalidIdentifier {
                field: "topic_bindings.allowed_messages",
                ..
            })
        ));
    }

    #[test]
    fn topic_bindings_empty_allowed_messages() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n\
             allowed_messages = []\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::TopicBindingEmptyMessages { ref topic, .. }) if topic == "mission"
        ));
    }

    #[test]
    fn topic_bindings_missing_topic_field() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             allowed_messages = [\"Msg\"]\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::MissingField {
                field: "topic_bindings.topic",
                ..
            })
        ));
    }

    #[test]
    fn topic_bindings_missing_allowed_messages_field() {
        let fix = TestFixture::new();
        let server_path = fix.write_server("");
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = [\"mission\"]\n\
             \n\
             [[topic_bindings]]\n\
             topic = \"mission\"\n",
        );

        let result = load(&server_path);
        assert!(matches!(
            result,
            Err(ConfigError::MissingField {
                field: "topic_bindings.allowed_messages",
                ..
            })
        ));
    }

    #[test]
    fn load_server_custom_websocket_limits() {
        let fix = TestFixture::new();
        let server_path = fix.write_server_with(|schema, svc_dir| {
            format!(
                "bind_addr = \"127.0.0.1:9000\"\n\
                 server_id = \"s1\"\n\
                 system_label = \"L\"\n\
                 schema_path = \"{}\"\n\
                 schema_version = \"002.5.0\"\n\
                 system_uuid = \"550e8400-e29b-41d4-a716-446655440000\"\n\
                 services_dir = \"{}\"\n\
                 max_message_size = 1000\n\
                 max_frame_size = 500\n\
                 max_write_buffer_size = 2000\n\
                 max_connections = 50",
                schema.display(),
                svc_dir.display()
            )
        });
        fix.write_service(
            "svc1.toml",
            "service_id = \"svc1\"\n\
             service_uuid = \"550e8400-e29b-41d4-a716-446655440001\"\n\
             allowed_topics = []\n",
        );

        let config = load(&server_path).unwrap();
        assert_eq!(config.server.max_message_size, 1000);
        assert_eq!(config.server.max_frame_size, 500);
        assert_eq!(config.server.max_write_buffer_size, 2000);
        assert_eq!(config.server.max_connections, 50);
    }
}
