# OMS Platform Design Document (PDD)

## 1. Introduction

This Platform Design Document (PDD) defines the architecture and interfaces of **Sleet**, an Open Mission Systems (OMS) Platform infrastructure component. Sleet provides the Abstract Service Bus (ASB) and the Language-Agnostic Critical Abstraction Layer (LA-CAL) for OMS Mission Packages.

## 2. Platform Architecture

### 2.1. Component Overview

Sleet is implemented as a highly concurrent, memory-safe Rust binary. Its primary responsibility is to broker communications between OMS components (Services, Subsystems, and Isolators) running within the Mission Package.

Sleet's execution environment is containerized (OCI-compliant), relying on the host system or orchestration layer to act as the OS Facade.

### 2.2. Abstract Service Bus (ASB)

Sleet implements the ASB through an in-memory pub/sub routing engine.

- **Routing Keys:** Messages are routed based on a composite of the `topic` and `message_name`.

- **Delivery:** Delivery is best-effort asynchronous. Each connected subscriber maintains an outbound delivery queue. If a subscriber fails to consume messages fast enough (queue saturation), Sleet drops the message for that specific subscriber to prevent head-of-line blocking and protect global memory bounds.

- **Concurrency:** Sleet utilizes an asynchronous event loop (`tokio`) to multiplex thousands of concurrent client connections over a single thread pool.

### 2.3. Language-Agnostic CAL (LA-CAL)

Sleet implements the LA-CAL via the OMS WebSocket Protocol (OWP) v1.0.

- **Endpoint:** Sleet binds a TCP port (default `9000`) and upgrades standard HTTP connections to WebSockets requiring the `Sec-WebSocket-Protocol: owp` header.

- **Handshake:** Clients must authenticate the session using the OWP `INIT` command, verifying schema version and service identity.

- **Operations:** The LA-CAL implementation strictly supports `INIT`, `PUB`, `SUB`, and `UNSUB` textual operations.

## 3. Resource Management

Sleet bounds memory usage to prevent Out-Of-Memory (OOM) conditions caused by massive payload exchanges (e.g., IQ snippets or large tracking updates):

- **Configurable Limits:** Sleet enforces `max_connections`, `max_message_size`, `max_frame_size`, and `max_write_buffer_size`.

- **Blocking Thread Pool:** Deep schema inspection and JSON parsing for large payloads are routed to a dedicated blocking thread pool. This guarantees the primary async executor is not starved during the validation of massive OMS messages.

## 4. Telemetry and Observability

Sleet acts as a transparent router but requires internal observability for Mission Package health.

- An optional administration listener (`metrics_bind_addr`) exposes `/health`, `/ready`, `/status`, and Prometheus metrics (`/metrics`).

- Operational telemetry tracks `connections_active`, `messages_published_total`, `messages_delivered_total`, and `validation_failures_total`.
