# Schema Sourcing

Sleet validates OMS JSON messages against the UCI XSD schema at runtime. Runtime UCI XSD files are **not committed** to the repository — they are fetched from the upstream UCI public repository and stored locally under `spec/<version>/`.

## Source Repository

The UCI schema is publicly available at:

```
https://gitlab.com/open-arsenal/uci/standard
```

Sleet pins the UCI repository revision used for builds and local schema fetches:

```
73a286fc4b881d9f328dbd64148525606a92c33a
```

Update the pinned revision intentionally when adopting a newer upstream schema snapshot.

For UCI v2.5, the relevant XSD files are in:

```
03_OAC-STD-002_RevE_UCI_Schema_v2_5/
├── UCI_MessageDefinitions_v2_5_0.xsd   ← main schema (includes SecurityMarkings)
├── UCI_SecurityMarkings_v2_5_0.xsd     ← included by MessageDefinitions
└── UCI_Versioning_v2_5_0.xsd           ← not needed for message validation
```

## Fetching the Schema Locally

The quickest way to populate `spec/` is the Makefile target:

```bash
make fetch-spec
```

This fetches the pinned UCI repository revision, copies the MessageDefinitions and SecurityMarkings XSD files into `spec/002.5.0/`, and cleans up.

To specify a different version (if supported in the Makefile):

```bash
make fetch-spec SPEC_VERSION=003.0.0
```

### Manual fetch

```bash
UCI_REPO=https://gitlab.com/open-arsenal/uci/standard.git
UCI_REF=73a286fc4b881d9f328dbd64148525606a92c33a
rm -rf /tmp/uci
mkdir -p /tmp/uci
git -C /tmp/uci init --initial-branch=main
git -C /tmp/uci remote add origin "$UCI_REPO"
git -C /tmp/uci fetch --depth 1 origin "$UCI_REF"
git -C /tmp/uci checkout --detach FETCH_HEAD
mkdir -p spec/002.5.0
cp /tmp/uci/03_OAC-STD-002_RevE_UCI_Schema_v2_5/UCI_MessageDefinitions_v2_5_0.xsd spec/002.5.0/
cp /tmp/uci/03_OAC-STD-002_RevE_UCI_Schema_v2_5/UCI_SecurityMarkings_v2_5_0.xsd spec/002.5.0/
rm -rf /tmp/uci
```

## Adding a New Schema Version

When a new UCI version is released (e.g., v3.0):

1. **Choose and fetch the upstream UCI revision:**

   ```bash
   UCI_REPO=https://gitlab.com/open-arsenal/uci/standard.git
   UCI_REF=<new-upstream-commit-sha>
   rm -rf /tmp/uci
   mkdir -p /tmp/uci
   git -C /tmp/uci init --initial-branch=main
   git -C /tmp/uci remote add origin "$UCI_REPO"
   git -C /tmp/uci fetch --depth 1 origin "$UCI_REF"
   git -C /tmp/uci checkout --detach FETCH_HEAD
   ```

2. **Identify the schema directory.** The naming pattern is `0N_OAC-STD-002_Rev*_UCI_Schema_v*`.

3. **Find the version string.** Open `UCI_MessageDefinitions_*.xsd` and look at the root element:

   ```xml
   <xs:schema ... version="003.0.0" ...>
   ```

   The `version` attribute value is the version string to use for the directory name.

4. **Create the spec directory and copy files:**

   ```bash
   mkdir -p spec/003.0.0
   cp /tmp/uci/<schema_dir>/UCI_MessageDefinitions_*.xsd spec/003.0.0/
   cp /tmp/uci/<schema_dir>/UCI_SecurityMarkings_*.xsd spec/003.0.0/
   ```

5. **Update `UCI_REF` in `Makefile` and `Containerfile`**, then update `config/server.toml` to reference the new version:

   ```toml
   schema_path = "./spec/003.0.0/UCI_MessageDefinitions_v3_0_0.xsd"
   schema_version = "003.0.0"
   ```

6. **Verify with the cached development test target:**
   ```bash
   make dev-test
   ```

## Generated UCI Helpers

The `sleet-types` crate includes strongly typed Rust structs and enums generated from the local UCI XSD for the supported UCI message subset. These generated types preserve exact serialized field names while offering idiomatic Rust structures.

**WARNING:** The generated files (such as `sleet-types/src/uci/v2_5.rs`) must **not** be manually edited. All updates must flow through the generator script.

To regenerate the UCI v2.5 helper types (for example, if the script is improved or the target message subset is expanded):

1. Ensure the `spec/002.5.0/` schema files are present locally (see _Fetching the Schema Locally_ above).
2. Run the generator script, writing the output to the generated module:
   ```bash
   python3 scripts/generate_uci_types.py --generate-rust --output sleet-types/src/uci/v2_5.rs
   ```
3. Format and check the generated code:
   ```bash
   make dev-check-fast
   ```
4. Run the test suite to verify generated changes:
   ```bash
   make dev-test
   ```

## Container Behavior

### Built-in schema (default)

The container image builds the UCI schema into the image at `/opt/sleet/spec/`. The Containerfile fetches the pinned UCI repository revision during the builder stage and copies the schema files into the runtime image.

The default schema path in a containerized deployment is:

```
/opt/sleet/spec/002.5.0/UCI_MessageDefinitions_v2_5_0.xsd
```

### Image provenance labels

Runtime images include labels describing the bundled UCI schema source:

| Label                           | Description                                           |
| ------------------------------- | ----------------------------------------------------- |
| `org.sleet.uci.schema_version`  | UCI schema version directory under `/opt/sleet/spec/` |
| `org.sleet.uci.schema_filename` | Main UCI message definitions XSD filename             |
| `org.sleet.uci.repo`            | Upstream UCI repository URL                           |
| `org.sleet.uci.ref`             | Upstream UCI revision fetched during the image build  |

Release bundle manifests use these image labels for schema provenance.

### Volume mount override

To use a different schema version or program-specific extensions without rebuilding the image, mount a volume over `/opt/sleet/spec/`:

```bash
# Use built-in schema (default)
podman run registry.gitlab.com/open-arsenal/ams-gra/hello-world-sk/infra/sleet:latest /opt/sleet/config/server.toml

# Override with custom schemas
podman run \
  -v /path/to/my/schemas:/opt/sleet/spec:Z \
  registry.gitlab.com/open-arsenal/ams-gra/hello-world-sk/infra/sleet:latest /opt/sleet/config/server.toml
```

Your `server.toml` should reference the schema path as it appears inside the container, and `schema_version` must match the root `version` attribute of the loaded XSD:

```toml
schema_path = "/opt/sleet/spec/002.5.0/UCI_MessageDefinitions_v2_5_0.xsd"
schema_version = "002.5.0"
```

Sleet fails startup when the configured schema version and loaded XSD version differ.

## CI Behavior

The GitLab CI pipeline builds a `builder` image from the Containerfile, which fetches the pinned UCI repository revision and bakes the schema files into the image at `/build/spec/`. Validate-stage jobs run inside this builder image, so the schema is always available without a separate fetch step.

The `validate` job sets `SLEET_SCHEMA_PATH` so that verification tests (which parse the real XSD and assert expected type/element counts) run against the baked-in schema automatically.

Schema clone failures during the builder image build will fail the pipeline — this is intentional, as the verification tests are part of the quality gate.
