###############################################################################
# Build stage — statically-linked Rust release binary targeting musl so the
# runtime stage can use distroless/static (no libc required).
###############################################################################
FROM rust:1.98.0-alpine@sha256:a10e64dd139b7387337c7fbe8aca31b959b57b2fd4c8ae20a02cf1d6ea424dce AS build

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

ENTRYPOINT ["/magents"]
