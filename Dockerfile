FROM rust:1.95-slim-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY web ./web
RUN cargo build --locked --release --bin nexus-panel

FROM debian:bookworm-slim
RUN install -d -m 0700 -o 10001 -g 10001 /var/lib/nexus
COPY --from=build /app/target/release/nexus-panel /usr/local/bin/nexus-panel
ENV NEXUS_BIND=0.0.0.0 NEXUS_PORT=8787 NEXUS_DATA_DIR=/var/lib/nexus
USER 10001:10001
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/nexus-panel"]
