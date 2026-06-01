# UCI Interaction ICD

## 1. Overview

This Interface Control Document (ICD) describes Sleet's interactions with the Universal Command and Control Interface (UCI) schema. While Sleet is primarily a transport-agnostic router (acting as the OMS Platform ASB), it strictly enforces UCI message structures.

## 2. Deep XSD Inspection

Sleet guarantees that the ASB remains unpolluted by malformed or non-compliant messages through **Deep XSD Inspection**.

### 2.1. Schema Loading

At startup, Sleet parses the full UCI XML Schema Definition (`UCI_MessageDefinitions_v2_5_0.xsd` and its includes) into an in-memory validation model. Sleet verifies the `schema_version` negotiated during the LA-CAL `INIT` handshake against this loaded schema.

### 2.2. Message Validation

When an OMS Service submits a `PUB` operation, Sleet executes a blocking validation pass:

1. The payload is parsed as JSON.

2. Sleet extracts the root element name (the UCI Message Name).

3. The payload structure is validated deeply against the XSD memory model (validating required elements, types, and sequence order).

4. If validation fails, the message is discarded before reaching the ASB, and the publisher receives a `-ERR Invalid-Message` response.

_Note: Sleet routes deep validation to a dedicated blocking thread pool. This design ensures that massive payloads (e.g., OMS large data transfers or 300MB IQ snippets) are thoroughly inspected without starving the primary async asynchronous network executor._

## 3. Native Type Generation (`sleet-types`)

While messages are treated as opaque, validated JSON during routing, the `sleet-types` crate provides generated Rust structures for a subset of the UCI schema. These types are available for future platform health tracking capabilities.

### 3.1. Auto-Generation Process

`sleet-types` is an automatically generated Rust crate that provides strongly typed struct and enum definitions for a subset of the UCI schema.

- **Tooling:** A Python script (`scripts/generate_uci_types.py`) loads the local UCI XSD.

- **Closure Discovery:** The script discovers the transitive type closure required for the targeted message subset.

- **Code Emission:** It emits deterministic Rust source code, enforcing strict memory safety and applying precise `#[serde(rename = "...")]` attributes to guarantee exact matching with UCI's serialized JSON field names.

- **Complex Types:** It automatically resolves UCI simple type enumerations into strongly-typed Rust enums, and handles `xs:choice` complex types by generating strict `one-of` constructs.

### 3.2. Usage

Future usage is intended to leverage the `sleet-types` crate to deserialize payloads like `ServiceStatus` directly into Rust structs. This will allow the platform to safely inspect the operational states of attached services without manual JSON traversal.
