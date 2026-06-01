# All build args — defined once at the top.
# CI overrides BUILDER_PREFIX/BUILDER_TAG to pull cached builder from registry.
ARG BUILD_IMAGE=quay.io/almalinuxorg/almalinux:10.1-20260129
ARG RUNTIME_IMAGE=quay.io/almalinuxorg/10-micro:10.1-20260129
ARG BUILDER_PREFIX=
ARG BUILDER_TAG=
ARG UCI_REPO=https://gitlab.com/open-arsenal/uci/standard.git
ARG UCI_REF=73a286fc4b881d9f328dbd64148525606a92c33a
ARG UCI_SCHEMA_VERSION=002.5.0
ARG UCI_SCHEMA_FILENAME=UCI_MessageDefinitions_v2_5_0.xsd
ARG UCI_SECURITY_MARKINGS_FILENAME=UCI_SecurityMarkings_v2_5_0.xsd

# ---- Dev ----
FROM ${BUILD_IMAGE} AS dev

RUN dnf install -y gcc git make ca-certificates curl python3 pkgconf-pkg-config \
    && dnf clean all

COPY rust-toolchain.toml /tmp/rust-toolchain.toml
RUN RUST_VERSION=$(grep 'channel' /tmp/rust-toolchain.toml | awk -F'"' '{print $2}') \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain ${RUST_VERSION} \
    && rm -f /tmp/rust-toolchain.toml

ENV PATH="/root/.cargo/bin:${PATH}"

RUN rustup component add rustfmt clippy \
    && cargo install cargo-audit --version 0.22.1 --locked \
    && cargo install cargo-deny --version 0.19.4 --locked \
    && cargo install cargo-about --version 0.9.0 --locked --features cli

# ---- Builder ----
FROM dev AS builder
ARG UCI_REPO
ARG UCI_REF
ARG UCI_SCHEMA_VERSION
ARG UCI_SCHEMA_FILENAME
ARG UCI_SECURITY_MARKINGS_FILENAME
WORKDIR /build

RUN rm -rf /tmp/uci \
    && mkdir -p /tmp/uci \
    && git -C /tmp/uci init --initial-branch=main \
    && git -C /tmp/uci remote add origin "$UCI_REPO" \
    && git -C /tmp/uci fetch --depth 1 origin "$UCI_REF" \
    && git -C /tmp/uci checkout --detach FETCH_HEAD \
    && mkdir -p "spec/${UCI_SCHEMA_VERSION}" \
    && cp "/tmp/uci/03_OAC-STD-002_RevE_UCI_Schema_v2_5/${UCI_SCHEMA_FILENAME}" "spec/${UCI_SCHEMA_VERSION}/" \
    && cp "/tmp/uci/03_OAC-STD-002_RevE_UCI_Schema_v2_5/${UCI_SECURITY_MARKINGS_FILENAME}" "spec/${UCI_SCHEMA_VERSION}/" \
    && rm -rf /tmp/uci

COPY Cargo.toml Cargo.lock clippy.toml rustfmt.toml ./
COPY .cargo/ .cargo/
COPY scripts/ scripts/
COPY sleet/ sleet/
COPY sleet-client/ sleet-client/
COPY sleet-types/ sleet-types/
COPY config/ config/
RUN sed -i \
    -e "s#^schema_path = .*#schema_path = \"/opt/sleet/spec/${UCI_SCHEMA_VERSION}/${UCI_SCHEMA_FILENAME}\"#" \
    -e "s#^schema_version = .*#schema_version = \"${UCI_SCHEMA_VERSION}\"#" \
    config/container/server.toml

ENV SLEET_SCHEMA_PATH=/build/spec/${UCI_SCHEMA_VERSION}/${UCI_SCHEMA_FILENAME}
RUN python3 scripts/generate_uci_types.py --self-test
RUN cargo build --workspace --release \
    && cargo doc --workspace --no-deps --document-private-items

# ---- Builder Cache ----
# Local builds: resolves to the `builder` stage above (BUILDER_PREFIX and BUILDER_TAG are empty).
# CI builds: resolves to the registry image pushed by the build-builder job.
FROM ${BUILDER_PREFIX}builder${BUILDER_TAG} AS builder-cache

# ---- Runtime ----
FROM ${RUNTIME_IMAGE} AS runtime
ARG UCI_REPO
ARG UCI_REF
ARG UCI_SCHEMA_VERSION
ARG UCI_SCHEMA_FILENAME
LABEL org.sleet.uci.schema_version="${UCI_SCHEMA_VERSION}" \
      org.sleet.uci.schema_filename="${UCI_SCHEMA_FILENAME}" \
      org.sleet.uci.repo="${UCI_REPO}" \
      org.sleet.uci.ref="${UCI_REF}"
RUN echo 'app:x:1000:1000::/home/app:/bin/sh' >> /etc/passwd && \
    echo 'app:x:1000:' >> /etc/group && \
    mkdir -p /home/app /opt/sleet && chown 1000:1000 /home/app /opt/sleet

COPY --from=builder-cache /build/target/release/sleet /usr/local/bin/sleet
COPY --from=builder-cache /build/spec/ /opt/sleet/spec/
COPY --from=builder-cache --chown=1000:1000 /build/config/container/server.toml /opt/sleet/config/server.toml
COPY --from=builder-cache --chown=1000:1000 /build/config/services.d/* /opt/sleet/config/services.d/

USER 1000
WORKDIR /opt/sleet

ENV SLEET_METRICS_PORT=21312

HEALTHCHECK --interval=10s --timeout=3s --retries=3 \
  CMD sleet --health-check || exit 1

ENTRYPOINT ["sleet"]
