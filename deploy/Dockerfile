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
HEALTHCHECK --interval=60s --timeout=5s CMD wget -qO- http://127.0.0.1:4533/health >/dev/null || exit 1
ENTRYPOINT ["/sbin/tini", "--", "conductor"]
