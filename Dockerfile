# syntax=docker/dockerfile:1

# Build stage: compiles the release binary. assets/ is embedded into the
# binary at compile time via include_bytes! (see src/assets_route.rs), so it
# is needed here but is NOT copied into the runtime stage below.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

# Cache dependency compilation separately from application source: build a
# throwaway crate against just the manifest first, so later source-only
# changes reuse the compiled dependency graph instead of rebuilding it.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
COPY assets ./assets
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --locked

# Runtime stage: debian-slim (not distroless) so the dynamically linked axum
# binary resolves glibc against the same base family it was built on,
# without needing a static/musl rebuild.
FROM debian:bookworm-slim AS runtime

RUN useradd --system --create-home --home-dir /home/plugboard --shell /usr/sbin/nologin plugboard

COPY --from=builder /build/target/release/plugboard /usr/local/bin/plugboard

USER plugboard
WORKDIR /home/plugboard

# plugboard reads its config via --config (default ./plugboard.toml,
# containing device hosts/credentials and auth settings). The deploying
# compose/orchestration is expected to bind-mount a config file to
# /etc/plugboard/plugboard.toml (read-only). Never bake real config or
# secrets into the image.
EXPOSE 8088
ENTRYPOINT ["/usr/local/bin/plugboard"]
CMD ["--config", "/etc/plugboard/plugboard.toml"]
