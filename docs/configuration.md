# Configuration

## Overview

Sleet uses:

1. A server TOML file (path passed as CLI positional argument, default `config/server.toml`)
2. A services directory containing one or more `*.toml` service files
3. Environment variables for log filtering and compose deployment settings

## Server Config File

| Key                      | Type             | Required | Default                                | Description                                                                                                                         |
| ------------------------ | ---------------- | -------- | -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `bind_addr`              | socket address   | yes      | —                                      | OWP WebSocket bind address                                                                                                          |
| `server_id`              | string           | yes      | —                                      | Server identifier used in `INFO`                                                                                                    |
| `system_label`           | string           | yes      | —                                      | Human-readable platform label                                                                                                       |
| `schema_path`            | path             | yes      | —                                      | Path to UCI message schema XSD                                                                                                      |
| `schema_version`         | string           | yes      | —                                      | Expected schema version for INIT validation; must match the loaded XSD root `version` attribute                                     |
| `namespace_uuid`         | UUID string      | no       | `507e1ce1-1111-4444-8888-507e1ce11111` | Namespace UUID used for UUIDv5 generation. Overridden by `SLEET_NAMESPACE_UUID` environment variable                                |
| `system_name`            | string           | no       | `supercell`                            | Name used with namespace UUID to generate the platform `system_uuid`                                                                |
| `subsystem_name`         | string           | no       | `supercell.subsystem.sleet`            | Name used with namespace UUID to generate the platform `subsystem_uuid`                                                             |
| `system_uuid`            | UUID string      | no       | UUIDv5(namespace, system_name)         | Explicit override for platform UUID included in INFO                                                                                |
| `subsystem_uuid`         | UUID string      | no       | UUIDv5(namespace, subsystem_name)      | Explicit override for platform subsystem UUID included in INFO                                                                      |
| `services_dir`           | path             | yes      | —                                      | Directory of per-service TOML files                                                                                                 |
| `log_format`             | `text` or `json` | no       | `text`                                 | Tracing output format                                                                                                               |
| `log_level`              | EnvFilter string | no       | `sleet=info`                           | Config-level tracing filter                                                                                                         |
| `otlp_endpoint`          | URL string       | no       | —                                      | OTLP collector endpoint (requires `otlp` feature)                                                                                   |
| `shutdown_drain_seconds` | integer          | no       | `5`                                    | Graceful shutdown drain time; valid `1..=30`                                                                                        |
| `metrics_bind_addr`      | socket address   | no       | —                                      | Enables admin listener (`/health`, `/ready`, `/status`, `/metrics`); startup fails if the listener or metrics recorder cannot start |
| `idle_timeout_seconds`   | integer          | no       | `300`                                  | Idle connection timeout; `0` disables, valid `0` or `10..=3600`                                                                     |
| `max_message_size`       | integer          | no       | `536870912`                            | WebSocket maximum payload size in bytes                                                                                             |
| `max_frame_size`         | integer          | no       | `134217728`                            | WebSocket maximum frame size in bytes                                                                                               |
| `max_write_buffer_size`  | integer          | no       | `134217728`                            | WebSocket maximum outbound write buffer size in bytes                                                                               |
| `max_connections`        | integer          | no       | `10000`                                | Maximum concurrent TCP connections allowed                                                                                          |

## Service Config Files (`services_dir/*.toml`)

| Key                                   | Type             | Required | Default | Description                              |
| ------------------------------------- | ---------------- | -------- | ------- | ---------------------------------------- |
| `service_id`                          | string           | yes      | —       | Client-visible service identifier        |
| `service_uuid`                        | UUID string      | yes      | —       | Service UUID for INFO response           |
| `allowed_topics`                      | array of strings | yes      | —       | Topics this service can `PUB`/`SUB`      |
| `[[topic_bindings]].topic`            | string           | no       | —       | Topic with message-type restrictions     |
| `[[topic_bindings]].allowed_messages` | array of strings | no       | —       | Allowed OMS message names for that topic |

Notes:

- If a topic has no `topic_bindings` entry, any schema-valid message is allowed.
- `topic_bindings.topic` must exist in `allowed_topics`.
- `allowed_messages` must be non-empty when present.

## Environment Variables

| Variable               | Description                                          | Overrides                                    |
| ---------------------- | ---------------------------------------------------- | -------------------------------------------- |
| `SLEET_NAMESPACE_UUID` | Environment-provided namespace for UUIDv5 generation | Overrides `namespace_uuid` config file value |
| `RUST_LOG`             | Tracing filter expression                            | Overrides `log_level`                        |
| `SLEET_METRICS_PORT`   | Admin listener port                                  | Overrides `metrics_bind_addr` port           |

## Compose Variables

These variables are consumed by `compose.yaml` for containerized deployments.

| Variable             | Default      | Description                                                  |
| -------------------- | ------------ | ------------------------------------------------------------ |
| `SLEET_LOG_LEVEL`    | `sleet=info` | Sets the container `RUST_LOG` value                          |
| `SLEET_BIND_PORT`    | `21402`      | Host port mapped to the container OWP WebSocket port `9000`  |
| `SLEET_METRICS_PORT` | `21312`      | Host port mapped to the container admin/metrics port `21312` |

## CLI

| Argument/Flag     | Description                                                           |
| ----------------- | --------------------------------------------------------------------- |
| `[CONFIG_PATH]`   | Optional path to server TOML config (`config/server.toml` by default) |
| `-h`, `--help`    | Print usage                                                           |
| `-V`, `--version` | Print version                                                         |

## Precedence

`RUST_LOG` > `server.toml log_level` > default (`sleet=info`)
