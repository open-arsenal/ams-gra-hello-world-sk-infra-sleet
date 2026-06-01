# Design Decisions

## Decisions

- **Workspace split into server + shared crates** — The repository is organized as a Cargo workspace with `sleet`, `sleet-types`, and `sleet-client` to keep protocol types reusable and server logic focused.
- **Schema-driven validation at runtime** — Sleet parses UCI XSD at startup and validates incoming OMS JSON against this model instead of generating static code from schema files.
- **Per-service authorization model** — Service identity from `INIT` drives topic authorization and optional topic/message restrictions.
- **Admin interface on separate listener** — Health/readiness/status/metrics are exposed on `metrics_bind_addr`, not on the OWP socket.
- **UCI helper generation uses the local XSD** — The repository includes a Python generator that indexes the local UCI v2.5 XSD files, reports only the transitive type closure for the supported UCI message subset, and emits deterministic Rust source for that subset, keeping Rust type generation work reproducible and offline.

## Constraints

- UCI schema files are sourced from a pinned upstream revision and expected at runtime paths configured in TOML.
- Generated UCI helper files (like `sleet-types/src/uci/v2_5.rs`) must not be manually edited; all changes must flow through the generator script.
- Sleet enforces WebSocket memory bounds using configurable `max_connections`, `max_message_size`, `max_frame_size`, and `max_write_buffer_size` limits. These values must be generous enough to accommodate large LA CAL OMS payloads (e.g., 300 MB IQ snippets) while preventing unbounded memory consumption. Sleet processes these massive payloads safely without starving the async executor by routing XSD validation to a blocking thread pool.
