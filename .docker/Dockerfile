# syntax=docker/dockerfile:1.7

# Keep in sync with rust-toolchain.toml
ARG RUST_VERSION=1.99.0

FROM rust:${RUST_VERSION}-trixie AS builder

WORKDIR /build

# Prime the dependency cache with an empty main.rs so cargo fetches/builds
# crates without re-running on every source change.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
RUN <<'EOF2'
set -eu
mkdir -p src
echo 'fn main() {}' > src/main.rs
cargo build --release --locked
rm -rf src target/release/panamax* target/release/deps/panamax-*
EOF2

COPY src ./src
COPY static ./static
COPY templates ./templates

ARG CARGO_BUILD_EXTRA=""
RUN cargo build --release --locked ${CARGO_BUILD_EXTRA} \
  && strip target/release/panamax


# Runtime: CA certificates for HTTPS, and git for the shallow index checkout sync reads.
FROM debian:trixie-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates git \
  && rm -rf /var/lib/apt/lists/* \
  && git config --system --add safe.directory '*'

COPY --from=builder /build/target/release/panamax /usr/local/bin/panamax

ENTRYPOINT ["/usr/local/bin/panamax"]
CMD ["--help"]
