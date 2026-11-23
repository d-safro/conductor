# Build: docker build -f deploy/Dockerfile -t conductor .
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
ENV CARGO_BUILD_JOBS=4
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM alpine:3
RUN apk add --no-cache ffmpeg tini
COPY --from=build /src/target/release/conductor /usr/local/bin/conductor
ENV CONDUCTOR_DATA_DIR=/data CONDUCTOR_MUSIC=/music CONDUCTOR_LISTEN=0.0.0.0:4533
VOLUME ["/data"]
EXPOSE 4533
# `conductor health` asks the address Conductor listens on, read as `serve` reads it (CONDUCTOR_LISTEN, else
# config.toml's listen, as in Prelude's compose file), not always 4533: there it was "unhealthy" for good, or asked
# whatever else answered on 4533 (with network_mode: host)
HEALTHCHECK --interval=60s --timeout=5s CMD ["/usr/local/bin/conductor", "health"]
ENTRYPOINT ["/sbin/tini", "--", "conductor"]
