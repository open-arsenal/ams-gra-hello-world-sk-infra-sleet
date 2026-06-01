# Contributing to Sleet

## Prerequisites

- [Podman](https://podman.io/getting-started/installation) (or Docker)
- [podman-compose](https://github.com/containers/podman-compose) (`pip install podman-compose`)
- Git

For host development without containers, you also need:
- Rust toolchain (see `rust-toolchain.toml` for the pinned version)
- `cargo-audit`, `cargo-deny`, and `cargo-about` installed manually for `host-audit` / `host-licenses`

The builder image includes the pinned Rust tooling used by containerized local targets and CI.

## Building and testing locally

All local development uses containerized Make targets. The first run builds the builder image; subsequent runs reuse cached layers.

```bash
# Fast source-mounted validation while iterating
make gate-fast           # alias for dev-gate-fast

# Full image-backed validation before opening an MR
make gate                # alias for image-gate

# Individual source-mounted targets
make dev-build
make dev-test
make dev-check

# Security and license audit with mounted source/cache
make dev-audit
```

### Running the full stack locally

```bash
# Run the configured image
make compose-up

# Build the runtime image locally and start it
make compose-build-up

# Or use the dev overlay for live source rebuilds
podman-compose -f compose.yaml -f compose.dev.yaml up --build
```

## Merge request requirements

Before opening an MR, ensure:

1. **Full gate passes**: `make gate`
2. **Fast gate passes during iteration**: `make gate-fast`
3. **No new warnings**: clippy must be clean (`-D warnings` is enforced)
4. **Tests cover the change**: new logic needs tests that verify behavior, not implementation details. See the test guidelines below.
5. **Commit messages are clear**: describe *what* changed and *why*, not *how*.

The CI pipeline builds the builder image, runs `make host-gate` in that image, runs audit/license checks where configured, and builds the runtime image. All required jobs must pass before merge.

## CI pipeline summary

Sleet CI is split by pipeline type:

- **Feature/fix branches (`feat/*`, `fix/*`)**
  - Run `build-builder`, `validate`, and `build-runtime` for fast feedback.
  - `audit` and `sbom` are skipped on these branch pipelines.
- **Merge requests / `main`**
  - Run full validation, audit/license checks, runtime image build, and SBOM generation.
- **Release tags (`v*`)**
  - Do not rebuild images from source.
  - CI promotes already-built immutable SHA images (`:<sha>`) to release tags (`:<tag>`), then bundles and publishes release artifacts.

To reduce duplicate pipelines, branch pipelines are skipped when an MR is open (`CI_OPEN_MERGE_REQUESTS`).

### Test guidelines

- Test **behavior**, not implementation details
- Use **inline fixtures** (e.g., inline TOML strings), not external files
- Do not grep source code, logs, or output as assertions
- Do not test file existence or project structure
- One logical concept per test with a descriptive name
- Mocks are acceptable for unit tests of application logic; integration tests should use real interfaces

## Project structure

| Path | Purpose |
|---|---|
| `sleet/` | Main server binary crate |
| `sleet-types/` | Shared OWP/UCI protocol types |
| `sleet-client/` | Server-agnostic OWP client library |
| `config/` | Example server and service configuration files |
| `spec/` | UCI schema files (fetched from upstream, not committed) |
| `scripts/` | `bundle.sh` (release packaging) |
| `.cargo/` | `deny.toml`, `about.toml`, `about.hbs` (audit and license report configs) |

## Release process

Releases are triggered by pushing a calver tag:

```bash
git tag v2026.03.29
git push origin v2026.03.29
```

Tags must point to commits that already have pushed SHA-tagged runtime and builder images from a branch or default-branch pipeline. The release pipeline promotes those existing images and does not rebuild from source.

The CI release pipeline then:

1. **Validates** — runs check, test, audit (including license checks)
2. **Promotes** container images — retags `:sha` images as `:<tag>` (no rebuild)
3. **Bundles** two tar artifacts:
   - `sleet-<tag>.tar.gz` (runtime) — container image, compose file, config, licenses
   - `sleet-<tag>-devkit.tar.gz` — adds builder image, source archive, dev compose overlay
4. **Publishes** both tars to the GitLab Generic Package Registry
5. **Creates** a GitLab Release with download links

### Tag format

- `v2026.03.29` — stable release
- `v2026.03.29-alpha.1` — pre-release
- `v2026.03.29-rc.1` — release candidate
- `v2026.03.29-hotfix.1` — same-day fix after a stable release

### What goes in the bundle

Each release tar is self-contained and includes:

| File | Purpose |
|---|---|
| `images/*.tar` | Container images (`podman load` ready) |
| `compose.yaml` | Service definitions for `podman-compose up` |
| `env.example` | Overridable environment variables |
| `config/` | Example server and service configuration |
| `spec/` | UCI schema files included in the runtime image |
| `LICENSE` | Project license (Apache License 2.0) |
| `INTENT.md` | Licensing intent for U.S. Federal Government contributions |
| `LICENSES/` | Third-party dependency licenses |
| `LICENSES/THIRD_PARTY_LICENSES.html` | Auto-generated license report for all Rust crate dependencies |
| `manifest.json` | Provenance metadata (tag, commit SHA, pipeline URL) |
| `SHA256SUMS` | Integrity verification for all files in the bundle |

Bundled compose files reference the release tag rather than `latest`.

The devkit variant additionally includes `source/` (source archive), `compose.dev.yaml` (builder overlay), and the builder container image.

## Licensing

Sleet is licensed under the Apache License 2.0. See `LICENSE` and `INTENT.md`.

Third-party dependencies are all Rust crates, audited by `cargo deny` in CI, with a license report generated by `cargo-about`.

When adding a new non-crate dependency:

1. Create `LICENSES/<name>/LICENSE` with the dependency's license text
2. Create `LICENSES/<name>/README.md` with YAML front matter including `LICENSE:` (SPDX) and `SOURCE:` (upstream URL)
3. Add a `dependencies` entry in `scripts/bundle.sh`'s manifest.json generation
4. If the dependency is LGPL, ensure the build recipe is included in both runtime and devkit tars
