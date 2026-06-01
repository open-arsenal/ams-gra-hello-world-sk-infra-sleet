# OMS Platform Compliance Checklist: Sleet

## 1. Scope

**Unit Name**: Sleet

**Component Type**: OMS Platform (Abstract Service Bus & LA-CAL)

**Standard**: Open Mission Systems (OMS) v2.5

This document assesses Sleet's compliance against the mandatory requirements for an OMS Platform, utilizing criteria derived from `OMSC-CHK-003_RevM_PlatformChecklist`.

## 2. Applicability

Sleet acts as the OMS Platform's message routing infrastructure. It provides the Language-Agnostic Critical Abstraction Layer (LA-CAL) over the OMS WebSocket Protocol (OWP) and acts as the Abstract Service Bus (ASB) for connected OMS Services and Subsystems.

## 3. Compliance Assessment

### 3.1. General Platform Requirements

- **ASB Provisioning**: **Compliant**. Sleet provides a centralized pub/sub routing fabric (the ASB) utilizing topic and message-name based routing.

- **CAL Provisioning**: **Compliant**. Sleet implements the Language-Agnostic CAL (LA-CAL) interface using OWP v1.0. It allows independent clients to connect, initialize, publish, and subscribe.

- **OS Facade Provisioning**: **N/A**. Sleet operates as a user-space application within an OS Facade provided by the underlying container orchestrator (e.g., Kubernetes/Podman). Sleet relies on standard container abstractions for compute and storage.

### 3.2. Data Exchanges and Messaging

- **Message Routing**: **Compliant**. Sleet routes messages asynchronously and guarantees delivery order on a per-connection basis (TCP/WebSocket).

- **Subscription Management**: **Compliant**. Sleet handles dynamic `SUB` and `UNSUB` operations, supporting up to 2048 active subscriptions per connection and applying group-based single-delivery rules when specified.

- **Validation**: **Compliant**. Sleet strictly enforces OMS messaging formats. It requires a valid `INIT` handshake defining the schema version, and applies deep validation on all `PUB` messages before they are allowed onto the ASB.

### 3.3. Lifecycle and State

- **Platform Initialization**: **Compliant**. Sleet loads its configuration (`server.toml`) and the UCI XSD schema prior to accepting LA-CAL connections, ensuring the ASB is fully initialized before use.

- **Service Lifecycle Tracking**: **Not Implemented**. Sleet currently acts as an opaque router and does not inspect `ServiceStatus` payloads to monitor service health.

- **Fault Reporting**: **Compliant**. Sleet reports connection, validation, and delivery faults via OWP `-ERR` responses and exposes internal telemetry metrics via an admin HTTP endpoint.

### 3.4. Security and Isolation

- **Connection Isolation**: **Compliant**. Subscriptions and publish queues are strictly isolated per connection state machine.

- **Transport Security**: **N/A (Delegated)**. Sleet provides the plaintext OWP layer. Transport Layer Security (TLS) and network isolation must be provided by the surrounding infrastructure/service mesh.
