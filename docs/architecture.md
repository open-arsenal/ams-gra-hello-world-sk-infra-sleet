# Architecture

## Overview

Sleet is a single-process OMS LA-CAL server. It accepts OWP client connections over WebSocket, validates incoming OMS JSON messages against a loaded UCI XSD schema model, and routes valid messages to subscribers.

The process also exposes an admin HTTP listener for health and metrics endpoints when configured.

## Components

### `sleet` (server binary)

- Loads and validates server/service TOML configuration.
- Parses UCI schema files into an in-memory model at startup.
- Runs WebSocket listener and per-connection OWP state machines.
- Performs OMS JSON validation before routing `PUB` payloads.
- Exposes admin endpoints (`/health`, `/ready`, `/status`, `/metrics`) when `metrics_bind_addr` is configured.
- Initializes tracing and optional OTLP export.

### `sleet-types` (shared protocol/types crate)

- OWP wire operation parsing and serialization.
- Shared OWP enums and payload structs.
- Generated UCI v2.5 helper types exposed under `sleet_types::uci::v2_5` for the supported UCI message subset.

### `scripts/generate_uci_types.py` (UCI helper generator)

- Loads the local UCI v2.5 MessageDefinitions XSD and local includes.
- Discovers the transitive type closure for the supported UCI message subset.
- Emits deterministic Rust source with serde renames for exact UCI serialized names.
- Generates strong Rust enums for UCI enumeration simple types and strict one-of enums for `xs:choice` complex types.

### `sleet-client` (client library)

- Async OWP client implementation.
- INIT handshake handling and helper methods for `SUB`, `UNSUB`, `PUB`, and message receive.

## Data Flow

1. **Startup**
   - Load `server.toml` and service files.
   - Parse and index XSD schema.
   - Initialize telemetry and optional admin listener.
   - Bind OWP WebSocket listener.

2. **Connection initialization**
   - Client upgrades to WebSocket with `Sec-WebSocket-Protocol: owp`.
   - Client sends `INIT`.
   - Server validates version, schema, and configured service.
   - Server responds with `+OK` (verbose mode) and `INFO`.

3. **Message processing**
   - `PUB`: validate topic authorization, parse JSON, validate against schema, route by `(topic, message_name)`.
   - `SUB`: validate and register subscription.
   - `UNSUB`: remove subscription.

4. **Operations and shutdown**
   - `/health` always reports liveness.
   - `/ready` reflects readiness state.
   - `/status` returns runtime state snapshot.
   - On shutdown signal, readiness is disabled, accepts stop, and active connections are drained.
