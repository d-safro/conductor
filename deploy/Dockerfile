# Build: docker build -f deploy/Dockerfile -t encore-server .
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM alpine:3
RUN apk add --no-cache ffmpeg tini
COPY --from=build /src/target/release/encore-server /usr/local/bin/encore-server
ENV ENCORE_DATA_DIR=/data ENCORE_MUSIC=/music ENCORE_LISTEN=0.0.0.0:4533
VOLUME ["/data"]
EXPOSE 4533
ENTRYPOINT ["/sbin/tini", "--", "encore-server"]
