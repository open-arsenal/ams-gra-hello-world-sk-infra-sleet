# External Interface Contracts

## Inputs

### OWP WebSocket Protocol

- **Direction:** Input
- **External party:** OWP clients
- **Transport:** WebSocket over TCP
- **Default endpoint:** `bind_addr` (example `0.0.0.0:9000`)
- **Compose host port:** `21402` maps to container/service port `9000` by default.
- **Handshake requirement:** Client must offer `Sec-WebSocket-Protocol: owp`; otherwise upgrade is rejected with HTTP 400.

#### Client operations

1. `INIT <json>`
   - Validates protocol version, schema version, and configured `service_id`.
   - Must be first and only INIT on a connection.
   - Client must send a valid `INIT` frame within 10 seconds of WebSocket connection establishment, or the connection is terminated.
2. `PUB <topic> <json>`
   - Topic authorization and schema validation required.
   - Optional topic/message binding enforcement per service config.
3. `SUB <sid> <message_name> <topic> [group]`
   - Registers message delivery subscription.
   - Each connection can register up to 2048 active subscriptions. Further subscriptions are rejected with `-ERR Illegal-State`.
4. `UNSUB <sid>`
   - Removes subscription.

#### Delivery and acknowledgments

- `PUB` accepts a schema-valid message for routing, but per-subscriber delivery is best effort.
- Each subscriber connection has an outbound delivery queue of 1024 server operations.
- If a subscriber delivery queue is saturated, Sleet drops that message for that subscriber and continues routing to other matching subscribers.
- Closed subscribers are cleaned up by the owning connection lifecycle.
- `INIT` accepts optional `verbose`; default is `true`.
- When `verbose=true`, successful client operations produce `+OK` before any operation-specific response. The first `+OK` precedes `INFO` after successful `INIT`.
- When `verbose=false`, successful client operations do not produce `+OK`; clients detect success by the absence of a later error or connection failure.
- Protocol/application errors produce `-ERR <code> [details]` on both verbose and non-verbose connections.
- Initialization failures and protocol state violations can terminate the connection.

## Outputs

### OWP Server Operations

- **Direction:** Output
- **External party:** Connected OWP clients
- **Transport:** WebSocket text frames

Operations:

- `+OK`
- `-ERR <error> [details]`
- `INFO <json>`
  - `INFO.uuids.subsystem` is populated with a UUIDv5 generated from the namespace and subsystem name, or from the config override.
- `MSG <sid> <json>`

### Admin HTTP Endpoints

- **Direction:** Output
- **External party:** Operators, orchestrators, monitoring systems
- **Transport:** HTTP/1.1
- **Enabled when:** `metrics_bind_addr` is configured
- **Failure handling:** When configured, admin listener or metrics recorder startup failure terminates the process instead of continuing without operational endpoints.
- **Compose host port:** `21312` maps to container/service port `21312` by default.

| Method | Path       | Contract                                                                                             |
| ------ | ---------- | ---------------------------------------------------------------------------------------------------- |
| `GET`  | `/health`  | Liveness endpoint. Returns `200` with JSON body `{"status":"ok"}`.                                   |
| `GET`  | `/ready`   | Readiness endpoint. Returns `200` when ready, `503` otherwise.                                       |
| `GET`  | `/status`  | Returns detailed JSON runtime status (uptime, connection count, schema version, readiness, version). |
| `GET`  | `/metrics` | Returns Prometheus exposition format (`text/plain; version=0.0.4`).                                  |

### Prometheus Metrics

Current emitted metrics include:

- `sleet_connections_active` (gauge)
- `sleet_messages_published_total` (counter, label: `topic`)
- `sleet_messages_delivered_total` (counter)
- `sleet_messages_dropped_total` (counter, label: `reason`)
- `sleet_validation_failures_total` (counter, label: `category`)
- `sleet_connections_failed_total` (counter, label: `error`)
- `sleet_message_processing_duration_seconds` (histogram)
- `sleet_validation_duration_seconds` (histogram)
- `sleet_websocket_bytes_received_total` (counter)
- `sleet_websocket_bytes_sent_total` (counter)
- `sleet_message_size_bytes` (histogram)
- `sleet_subscriptions_active` (gauge, label: `topic`)
- `sleet_client_queue_depth` (histogram)

## Security

- The protocol itself does not embed authentication.
- Deployments should enforce transport security and access control via surrounding infrastructure (for example, TLS termination and network policy).
- Secrets must not be stored in committed `.env` files.
