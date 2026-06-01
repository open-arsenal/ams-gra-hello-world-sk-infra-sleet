#!/usr/bin/env bash
# scripts/bundle.sh — Produce a release artifact tar for sleet.
#
# Usage: scripts/bundle.sh <variant> <tag>
#   variant: "runtime" or "devkit"
#   tag:     release tag, e.g. "v2026.03.29"
#
# Outputs: ./<project>-<tag>.tar.gz or ./<project>-<tag>-devkit.tar.gz
# Exit 0 on success, non-zero on failure.

set -euo pipefail

PROJECT="sleet"
VARIANT="${1:?Usage: bundle.sh <runtime|devkit> <tag>}"
TAG="${2:?Usage: bundle.sh <runtime|devkit> <tag>}"
COMMIT="${CI_COMMIT_SHA:-$(git rev-parse HEAD)}"
PIPELINE_URL="${CI_PIPELINE_URL:-local}"
REGISTRY="${CI_REGISTRY_IMAGE:-registry.gitlab.com/open-arsenal/ams-gra/hello-world-sk/infra/sleet}"
RUNTIME_IMAGE_REF="${REGISTRY}:${TAG}"
BUILDER_IMAGE_REF="${REGISTRY}/builder:${TAG}"
CREATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [[ "$VARIANT" != "runtime" && "$VARIANT" != "devkit" ]]; then
  echo "ERROR: variant must be 'runtime' or 'devkit', got '$VARIANT'" >&2
  exit 1
fi

if [[ "$VARIANT" == "devkit" ]]; then
  BUNDLE_DIR="${PROJECT}-${TAG}-devkit"
  OUTPUT="${BUNDLE_DIR}.tar.gz"
else
  BUNDLE_DIR="${PROJECT}-${TAG}"
  OUTPUT="${BUNDLE_DIR}.tar.gz"
fi

echo "=== Bundling ${PROJECT} ${TAG} (${VARIANT}) ==="

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "ERROR: required file missing: $path" >&2
    exit 1
  fi
}

set_compose_image() {
  local path="$1"
  local image_ref="$2"
  sed -i -E 's#^([[:space:]]*image:[[:space:]]*).*$#\1'"${image_ref}"'#' "$path"
}

extract_runtime_spec() {
  local container_id=""

  if ! container_id=$(buildah from "${BUILDER_IMAGE_REF}"); then
    echo "ERROR: failed to create extraction container from ${BUILDER_IMAGE_REF}" >&2
    exit 1
  fi

  if ! buildah run "$container_id" -- tar cf - -C /build spec | tar xf - -C "${BUNDLE_DIR}"; then
    buildah rm "$container_id" >/dev/null 2>&1 || true
    echo "ERROR: failed to extract /build/spec from ${BUILDER_IMAGE_REF}" >&2
    exit 1
  fi

  buildah rm "$container_id" >/dev/null 2>&1 || true

  if ! find "${BUNDLE_DIR}/spec" -type f -name '*.xsd' | grep -q .; then
    echo "ERROR: runtime image spec extraction did not produce XSD files" >&2
    exit 1
  fi
}

extract_rustdoc() {
  local container_id=""

  if ! container_id=$(buildah from "${BUILDER_IMAGE_REF}"); then
    echo "ERROR: failed to create extraction container from ${BUILDER_IMAGE_REF}" >&2
    exit 1
  fi

  mkdir -p "${BUNDLE_DIR}/docs/rustdoc"
  if ! buildah run "$container_id" -- tar cf - -C /build/target doc | tar xf - -C "${BUNDLE_DIR}/docs/rustdoc" --strip-components=1; then
    buildah rm "$container_id" >/dev/null 2>&1 || true
    echo "ERROR: failed to extract /build/target/doc from ${BUILDER_IMAGE_REF}" >&2
    exit 1
  fi

  buildah rm "$container_id" >/dev/null 2>&1 || true
}

read_runtime_label() {
  local label_name="$1"
  buildah inspect --type image --format "{{ index .Docker.Config.Labels \"${label_name}\" }}" "${RUNTIME_IMAGE_REF}"
}

load_uci_provenance() {
  UCI_SCHEMA_VERSION=$(read_runtime_label "org.sleet.uci.schema_version")
  UCI_SCHEMA_FILENAME=$(read_runtime_label "org.sleet.uci.schema_filename")
  UCI_REPO=$(read_runtime_label "org.sleet.uci.repo")
  UCI_REF=$(read_runtime_label "org.sleet.uci.ref")

  if [[ -z "$UCI_SCHEMA_VERSION" || -z "$UCI_SCHEMA_FILENAME" || -z "$UCI_REPO" || -z "$UCI_REF" ]]; then
    echo "ERROR: runtime image ${RUNTIME_IMAGE_REF} is missing UCI provenance labels" >&2
    exit 1
  fi
}

# Clean
rm -rf "$BUNDLE_DIR" "$OUTPUT"
mkdir -p "${BUNDLE_DIR}/images"

# ── Save container images ─────────────────────────────────────────────────────
echo "Saving runtime image..."
buildah push "${RUNTIME_IMAGE_REF}" "docker-archive:${BUNDLE_DIR}/images/${PROJECT}-${TAG}.tar:${RUNTIME_IMAGE_REF}"

if [[ "$VARIANT" == "devkit" ]]; then
  echo "Saving builder image..."
  buildah push "${BUILDER_IMAGE_REF}" "docker-archive:${BUNDLE_DIR}/images/${PROJECT}-builder-${TAG}.tar:${BUILDER_IMAGE_REF}"
fi

# ── Copy compose, config ──────────────────────────────────────────────────────
cp compose.yaml "${BUNDLE_DIR}/compose.yaml"
set_compose_image "${BUNDLE_DIR}/compose.yaml" "${RUNTIME_IMAGE_REF}"
cp env.example "${BUNDLE_DIR}/env.example"
mkdir -p "${BUNDLE_DIR}/config"
cp config/server.toml "${BUNDLE_DIR}/config/server.toml"
cp -r config/container "${BUNDLE_DIR}/config/container"
cp -r config/services.d "${BUNDLE_DIR}/config/services.d"
cp -r config/grafana "${BUNDLE_DIR}/config/grafana"
cp config/prometheus.yml "${BUNDLE_DIR}/config/prometheus.yml"
extract_runtime_spec
extract_rustdoc

# ── Assemble LICENSE/LICENSES/ and sboms/ directories ─────────────────────────
mkdir -p "${BUNDLE_DIR}/LICENSES" "${BUNDLE_DIR}/sboms"

# Required contract files
require_file ports.json
require_file LICENSE
require_file INTENT.md
require_file LICENSES/uci-schema/LICENSE
require_file LICENSES/uci-schema/README.md
require_file THIRD_PARTY_LICENSES.html

# Project license and licensing intent at top-level
cp LICENSE "${BUNDLE_DIR}/LICENSE"
cp INTENT.md "${BUNDLE_DIR}/INTENT.md"

# Non-package-manager dependency licenses
cp -r LICENSES/uci-schema "${BUNDLE_DIR}/LICENSES/uci-schema"

# Auto-generated third-party license report under LICENSES/
cp THIRD_PARTY_LICENSES.html "${BUNDLE_DIR}/LICENSES/THIRD_PARTY_LICENSES.html"

# CycloneDX SBOMs (generated by CI sbom stage)
require_file sbom-runtime.cdx.json
cp sbom-runtime.cdx.json "${BUNDLE_DIR}/sboms/${PROJECT}-${TAG}-runtime.cdx.json"

if [[ "$VARIANT" == "devkit" ]]; then
  require_file sbom-builder.cdx.json
  cp sbom-builder.cdx.json "${BUNDLE_DIR}/sboms/${PROJECT}-${TAG}-builder.cdx.json"
fi

# ── Devkit extras ─────────────────────────────────────────────────────────────
if [[ "$VARIANT" == "devkit" ]]; then
  echo "Archiving source..."
  mkdir -p "${BUNDLE_DIR}/source"
  git archive --format=tar --prefix="${PROJECT}-${TAG}-src/" HEAD \
    | gzip -n > "${BUNDLE_DIR}/source/${PROJECT}-${TAG}-src.tar.gz"

  cp compose.dev.yaml "${BUNDLE_DIR}/compose.dev.yaml"
  set_compose_image "${BUNDLE_DIR}/compose.dev.yaml" "${BUILDER_IMAGE_REF}"
fi

load_uci_provenance

# ── Generate manifest.json ────────────────────────────────────────────────────
jq -n \
  --arg project "$PROJECT" \
  --arg tag "$TAG" \
  --arg commit "$COMMIT" \
  --arg pipeline_url "$PIPELINE_URL" \
  --arg created_at "$CREATED_AT" \
  --arg variant "$VARIANT" \
  --arg registry "$REGISTRY" \
  --arg uci_version "$UCI_SCHEMA_VERSION" \
  --arg uci_repo "$UCI_REPO" \
  --arg uci_ref "$UCI_REF" \
  --arg uci_filename "$UCI_SCHEMA_FILENAME" \
  --slurpfile ports ports.json \
  '{
    project: $project,
    tag: $tag,
    commit: $commit,
    pipeline_url: $pipeline_url,
    created_at: $created_at,
    variant: $variant,
    license: "Apache-2.0",
    images: (
      if $variant == "devkit" then
        [
          {
            name: $project,
            tag: $tag,
            registry: $registry,
            file: "images/\($project)-\($tag).tar"
          },
          {
            name: "\($project)-builder",
            tag: $tag,
            registry: "\($registry)/builder",
            file: "images/\($project)-builder-\($tag).tar"
          }
        ]
      else
        [
          {
            name: $project,
            tag: $tag,
            registry: $registry,
            file: "images/\($project)-\($tag).tar"
          }
        ]
      end
    ),
    dependencies: [
      {
        name: "uci-schema",
        version: $uci_version,
        license: "Government Owned",
        source: $uci_repo,
        revision: $uci_ref,
        path: "spec/\($uci_version)",
        schema_file: $uci_filename,
        modified: false,
        modifications: []
      }
    ],
    ports: (if $ports == [] then [] else $ports[0] end)
  }' > "${BUNDLE_DIR}/manifest.json"

# ── Generate SHA256SUMS ──────────────────────────────────────────────────────
echo "Generating SHA256SUMS..."
(cd "$BUNDLE_DIR" && find . -type f ! -name SHA256SUMS -print0 \
  | sort -z \
  | xargs -0 sha256sum > SHA256SUMS)

# ── Create final tar ─────────────────────────────────────────────────────────
echo "Creating ${OUTPUT}..."
tar czf "$OUTPUT" "$BUNDLE_DIR"
rm -rf "$BUNDLE_DIR"

echo "=== Done: ${OUTPUT} ==="
