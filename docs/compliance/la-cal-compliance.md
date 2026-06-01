# LA-CAL Compliance Assessment

## 1. Scope

**Unit Name**: Sleet

**Component Type**: Language-Agnostic Critical Abstraction Layer (LA-CAL) Provider

**Standard**: Open Mission Systems (OMS) v2.5 - LA-CAL Specification (OWP v1.0)

This document maps the specific Language-Agnostic CAL (LA-CAL) requirements to Sleet's implementation and provides the verification evidence proving compliance.

## 2. LA-CAL Profile: OMS WebSocket Protocol (OWP)

Sleet implements the LA-CAL utilizing the OMS WebSocket Protocol (OWP) v1.0 standard, providing transport and messaging infrastructure for non-native or distinct-language OMS components.

### 2.1. Connection and Transport

- **Requirement**: The LA-CAL shall support client connections via WebSocket over TCP, utilizing the appropriate subprotocol.
- **Compliance**: **Compliant**. Sleet accepts standard HTTP upgrades to WebSocket, strictly requiring the `Sec-WebSocket-Protocol: owp` header.

### 2.2. Initialization Phase

- **Requirement**: Clients must authenticate and negotiate parameters via an `INIT` handshake before publishing or subscribing.
- **Compliance**: **Compliant**. Sleet enforces the `INIT` sequence. It validates the protocol version, schema version, and service configuration. It responds with an `INFO` frame containing standard RFC4122 UUIDs.

### 2.3. Publish Operations

- **Requirement**: The LA-CAL shall accept messages for routing via `PUB` operations and ensure message integrity.
- **Compliance**: **Compliant**. Sleet accepts `PUB` frames, enforcing deep XSD structural validation and topic authorization before placing messages onto the Abstract Service Bus.

### 2.4. Subscribe Operations

- **Requirement**: The LA-CAL shall support dynamic message subscription (`SUB`) and unsubscription (`UNSUB`). It must support delivery groups for load-balancing identically-configured services.
- **Compliance**: **Compliant**. Sleet manages up to 2048 active subscriptions per connection. It fully implements delivery grouping, ensuring a single delivery per group for a given message.

### 2.5. Error Handling and State Management

- **Requirement**: The LA-CAL must provide deterministic error codes for malformed operations, invalid states, and schema violations.
- **Compliance**: **Compliant**. Sleet emits standard `-ERR` responses (e.g., `Unsupported-Version`, `Illegal-Operation`, `Invalid-Message`) and cleanly terminates connections upon critical state violations. Sleet natively respects the `verbose` flag to suppress `+OK` frames while guaranteeing delivery of asynchronous errors.

## 3. Verification Evidence

Compliance with the LA-CAL OWP v1.0 specification is verified continuously via the Distro A `la-cal-harness` automated test suite.

**Evidence Document:** [LA-CAL Harness Assessment](./la-cal-harness-assessment.md)

The harness acts as a rigorous client, exhaustively testing state machines, edge cases, and high-throughput scenarios. Sleet maintains a **100% pass rate (72/72 tests)** against the `exhaustive` validation tier of the harness, proving full compliance with the LA-CAL specification boundaries.
