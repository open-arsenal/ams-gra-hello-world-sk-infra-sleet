//! Tracing subscriber initialization and log filter resolution.

use tracing::dispatcher::DefaultGuard;
use tracing_subscriber::EnvFilter;

/// Log format variants supported by Sleet.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LogFormat {
    /// Human-readable log lines for local development and terminal usage.
    Text,
    /// Newline-delimited JSON logs for aggregators.
    Json,
}

/// Resolve the effective [`EnvFilter`] from environment and config values.
///
/// Precedence is: `RUST_LOG` environment value, then the configured
/// `log_level`, then the default `sleet=info`.
pub fn resolve_filter(env_val: Option<&str>, config: Option<&str>) -> EnvFilter {
    let candidate = env_val.or(config).unwrap_or("sleet=info");
    EnvFilter::try_new(candidate).unwrap_or_else(|_| EnvFilter::new("sleet=info"))
}

/// Install a bootstrap subscriber before configuration is loaded.
///
/// The returned guard must be held until [`init_tracing`] is called.
pub fn init_bootstrap() -> DefaultGuard {
    let env_val = std::env::var("RUST_LOG").ok();
    let filter = resolve_filter(env_val.as_deref(), None);
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(true)
        .finish();
    tracing::subscriber::set_default(subscriber)
}

/// Register Prometheus metric descriptions.
pub fn describe_metrics() {
    metrics::describe_gauge!(
        "sleet_connections_active",
        "Current number of active OWP WebSocket connections."
    );
    metrics::describe_counter!(
        "sleet_messages_published_total",
        "Total number of accepted OWP PUB messages."
    );
    metrics::describe_counter!(
        "sleet_messages_delivered_total",
        "Total number of OWP MSG deliveries sent to subscribers."
    );
    metrics::describe_counter!(
        "sleet_messages_dropped_total",
        "Total number of OWP MSG deliveries dropped before subscriber delivery."
    );
    metrics::describe_counter!(
        "sleet_validation_failures_total",
        "Total number of rejected message validation attempts."
    );
    metrics::describe_counter!(
        "sleet_connections_failed_total",
        "Total number of OWP connections failed during protocol handling."
    );
    metrics::describe_histogram!(
        "sleet_message_processing_duration_seconds",
        "Time taken from receiving a PUB message to placing it on subscriber queues."
    );
    metrics::describe_histogram!(
        "sleet_validation_duration_seconds",
        "Time spent validating XML/JSON payloads."
    );
    metrics::describe_counter!(
        "sleet_websocket_bytes_received_total",
        "Total incoming payload bytes."
    );
    metrics::describe_counter!(
        "sleet_websocket_bytes_sent_total",
        "Total outgoing payload bytes."
    );
    metrics::describe_histogram!(
        "sleet_message_size_bytes",
        "Distribution of incoming message sizes in bytes."
    );
    metrics::describe_gauge!(
        "sleet_subscriptions_active",
        "Current number of active subscriptions."
    );
    metrics::describe_histogram!(
        "sleet_client_queue_depth",
        "Current depth of the client bounded delivery queue."
    );
}

/// Install the final global tracing subscriber after configuration is loaded.
pub fn init_tracing(
    log_format: LogFormat,
    log_level: Option<&str>,
    otlp_endpoint: Option<&str>,
) -> Result<(), String> {
    let env_val = std::env::var("RUST_LOG").ok();
    let filter = resolve_filter(env_val.as_deref(), log_level);
    init_with_format(log_format, filter, otlp_endpoint)
}

fn init_with_format(
    log_format: LogFormat,
    filter: EnvFilter,
    otlp_endpoint: Option<&str>,
) -> Result<(), String> {
    #[cfg(feature = "otlp")]
    if let Some(endpoint) = otlp_endpoint {
        return init_with_otlp(log_format, filter, endpoint);
    }

    #[cfg(not(feature = "otlp"))]
    if otlp_endpoint.is_some() {
        tracing::warn!(
            "otlp_endpoint is configured but the 'otlp' feature is not enabled — traces will not be exported"
        );
    }

    init_without_otlp(log_format, filter)
}

fn init_without_otlp(log_format: LogFormat, filter: EnvFilter) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    match log_format {
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true),
            )
            .try_init()
            .map_err(|e| e.to_string()),
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .try_init()
            .map_err(|e| e.to_string()),
    }
}

#[cfg(feature = "otlp")]
fn init_with_otlp(log_format: LogFormat, filter: EnvFilter, endpoint: &str) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let tracer = build_otlp_tracer(endpoint)?;

    match log_format {
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true),
            )
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init()
            .map_err(|e| e.to_string()),
        LogFormat::Text => tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init()
            .map_err(|e| e.to_string()),
    }
}

#[cfg(feature = "otlp")]
fn build_otlp_tracer(endpoint: &str) -> Result<opentelemetry_sdk::trace::SdkTracer, String> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.to_owned())
        .build()
        .map_err(|e| e.to_string())?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder_empty()
                .with_attributes([opentelemetry::KeyValue::new("service.name", "sleet")])
                .build(),
        )
        .build();

    Ok(provider.tracer("sleet"))
}

#[cfg(test)]
mod tests {
    use super::resolve_filter;

    #[test]
    fn filter_uses_env_over_config() {
        let filter = resolve_filter(Some("sleet=debug"), Some("sleet=info"));
        assert!(filter.to_string().contains("sleet=debug"));
    }

    #[test]
    fn filter_uses_config_when_env_missing() {
        let filter = resolve_filter(None, Some("sleet=trace"));
        assert!(filter.to_string().contains("sleet=trace"));
    }

    #[test]
    fn filter_defaults_to_info() {
        let filter = resolve_filter(None, None);
        assert!(filter.to_string().contains("sleet=info"));
    }
}
