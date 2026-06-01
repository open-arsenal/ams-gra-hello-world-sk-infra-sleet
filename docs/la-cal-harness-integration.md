# LA-CAL Harness Integration

The Sleet test suite runs against the containerized [`la-cal-harness`](https://gitlab.com/open-arsenal/ams-gra/hello-world-sk/test/la-cal-harness) to validate OWP (OMS WebSocket Protocol) behavior.

## Harness Configuration

The harness strictly asserts against a provided configuration file containing the expected topics and services.
Developers must maintain `config/harness.yaml` to mirror Sleet's active `config/harness-server.toml` and `harness-services.d/` directory.
If you add a new topic or service to Sleet's testing harness, you **must** also update `config/harness.yaml` or the harness tests will fail with `-ERR Illegal-State`.

## Running the Harness Locally

Sleet's `Makefile` wraps the harness integration using the `compose.harness.yaml` overlay.

```bash
# Validates harness config and network connection to Sleet
make harness-preflight

# Fast validation (integrated into `make gate`)
make harness-smoke

# Standard validation
make harness-standard

# Extensive validation
make harness-exhaustive
```

If you modify Sleet's message schemas or add topics, remember to align `config/harness.yaml` with your `config/harness-server.toml` and `config/harness-services.d/*.toml` definitions.
