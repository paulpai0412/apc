# syntax=docker/dockerfile:1
# Built by the native GitHub ARM64 runner, never on the Oracle VM.
FROM node:22-bookworm-slim AS frontend
WORKDIR /src/hmi/frontend
COPY hmi/frontend/package*.json ./
RUN --mount=type=secret,id=proxy_ca \
    if [ -f /run/secrets/proxy_ca ]; then export NODE_EXTRA_CA_CERTS=/run/secrets/proxy_ca; fi; \
    npm ci --strict-ssl=true
COPY hmi/frontend/ ./
COPY deploy/docker/test-loader.mjs /tmp/test-loader.mjs
RUN npm run lint && node --experimental-strip-types --loader /tmp/test-loader.mjs --test tests/*.test.ts && npm run build

FROM rust:1.90-bookworm AS native
ENV CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
WORKDIR /src
COPY hmi/Cargo.* hmi/
COPY hmi/src/ hmi/src/
COPY hmi/config/settings.toml hmi/config/settings.toml
COPY apc_engine/Cargo.* apc_engine/
COPY apc_engine/src/ apc_engine/src/
COPY apc_engine/tests/ apc_engine/tests/
# Separate targets, since this repository deliberately has no Cargo workspace.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=secret,id=proxy_ca \
    if [ -f /run/secrets/proxy_ca ]; then export CARGO_HTTP_CAINFO=/run/secrets/proxy_ca; fi; \
    cargo test --locked --manifest-path hmi/Cargo.toml && \
    cargo test --locked --manifest-path apc_engine/Cargo.toml && \
    cargo build --locked --release --manifest-path hmi/Cargo.toml && \
    cargo build --locked --release --manifest-path apc_engine/Cargo.toml --bin mpc_campaign

FROM debian:bookworm-slim AS com-build
RUN apt-get update && apt-get install -y --no-install-recommends make g++-mingw-w64-x86-64-posix && rm -rf /var/lib/apt/lists/*
WORKDIR /src/opc_da_simulator
COPY opc_da_simulator/ ./
COPY deploy/docker/register.cpp ./register.cpp
RUN make CXX=x86_64-w64-mingw32-g++-posix && \
    x86_64-w64-mingw32-g++-posix -std=c++17 -O2 -static -static-libgcc -static-libstdc++ register.cpp -o build/apc_register.exe -lole32

FROM debian:bookworm-slim AS runtime
ARG TARGETARCH
ARG SOURCE_REVISION=unknown
LABEL org.opencontainers.image.source="https://github.com/paulpai0412/apc" \
      org.opencontainers.image.revision=$SOURCE_REVISION \
      org.opencontainers.image.title="APC isolated research HMI"
RUN test "$TARGETARCH" = arm64
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl python3 python3-numpy libssl3 tini xz-utils \
    && rm -rf /var/lib/apt/lists/*
# The existing real x64 COM simulator needs ARM Wine plus an x64 emulator.
# Pin the upstream distribution and verify before installing. No host binfmt,
# privileged container, Windows machine, or production COM registration.
RUN --mount=type=secret,id=proxy_ca \
    if [ -f /run/secrets/proxy_ca ]; then export CURL_CA_BUNDLE=/run/secrets/proxy_ca; fi; \
    curl --fail --location --retry 3 -o /tmp/hangover.tar \
      https://github.com/AndreRH/hangover/releases/download/hangover-11.16/hangover_11.16_debian12_bookworm_arm64.tar && \
    echo 'dd7373202a168b7200cc36627a25b588511f3783756c2661bf4c022be8380aa8  /tmp/hangover.tar' | sha256sum -c - && \
    mkdir /tmp/hangover && tar -xf /tmp/hangover.tar -C /tmp/hangover && \
    apt-get update && apt-get install -y --no-install-recommends /tmp/hangover/*.deb && \
    rm -rf /tmp/hangover /tmp/hangover.tar /var/lib/apt/lists/*
COPY --from=native /src/hmi/target/release/authrity-hmi /app/bin/authrity-hmi
COPY --from=native /src/apc_engine/target/release/mpc_campaign /app/bin/mpc_campaign
COPY --from=frontend /src/hmi/frontend/dist/ /app/frontend/dist/
COPY --from=com-build /src/opc_da_simulator/build/ /app/com/
COPY validation/*.py /app/validation/
COPY validation/examples/*.json /app/validation/examples/
COPY LICENSE /app/LICENSE
COPY opc_da_simulator/LICENSE.OPC /app/com/LICENSE.OPC
COPY deploy/docker/ /app/container/
RUN useradd --uid 10001 --create-home --home-dir /data apc && \
    mkdir -p /data/config /data/research /data/reports /data/pki && \
    chown -R apc:apc /data && chmod +x /app/container/*.sh && \
    python3 -c 'import numpy; assert 1 <= int(numpy.__version__.split(".")[0]) < 3' && \
    python3 -m unittest discover -s /app/validation -p 'test_*.py'
ENV HOME=/data WINEPREFIX=/data/wine WINEARCH=win64 WINEDEBUG=-all \
    PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 WINEDLLOVERRIDES="mscoree,mshtml=" \
    AUTHRITY_HMI_CONFIG=/app/container/settings.toml
VOLUME ["/data"]
USER 10001:10001
WORKDIR /data
EXPOSE 3000
HEALTHCHECK --interval=15s --timeout=5s --start-period=180s --retries=3 \
    CMD ["python3", "/app/container/healthcheck.py"]
ENTRYPOINT ["/usr/bin/tini", "--", "/app/container/entrypoint.sh"]
