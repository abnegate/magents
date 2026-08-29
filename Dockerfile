###############################################################################
# Build stage — statically-linked Rust release binary targeting musl so the
# runtime stage can use distroless/static (no libc required).
###############################################################################
FROM rust:1.97.1-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS build

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
FROM gcr.io/distroless/static-debian12:nonroot@sha256:f5b485ea962d9bd1186b2f6b3a061191539b905b82ec395de78cbfae51f20e35

COPY --from=build /src/target/release/magents /magents

ENTRYPOINT ["/magents"]
