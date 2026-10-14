###############################################################################
# Build stage — statically-linked Rust release binary targeting musl so the
# runtime stage can use distroless/static (no libc required).
###############################################################################
FROM rust:1.99.0-alpine@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627 AS build

RUN apk add --no-cache musl-dev pkgconfig

WORKDIR /src

# Cache the dependency graph first; the real sources overwrite the stub later.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo fetch --locked

COPY src ./src
COPY skills ./skills
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --locked \
    && strip target/release/magents

###############################################################################
# Runtime stage — distroless static. Binary is statically linked against musl,
# so no libc is required. No shell, no package manager.
###############################################################################
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

COPY --from=build /src/target/release/magents /magents

# Official MCP Registry ownership check + default stdio MCP for Glama introspection.
# CLI usage still works: `docker run ... list --live` replaces CMD.
LABEL io.modelcontextprotocol.server.name="io.github.abnegate/magents"
ENTRYPOINT ["/magents"]
CMD ["mcp"]
