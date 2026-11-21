# Build: docker build -f deploy/Dockerfile -t maestro .
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc
WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM alpine:3
RUN apk add --no-cache ffmpeg tini
COPY --from=build /src/target/release/maestro /usr/local/bin/maestro
ENV MAESTRO_DATA_DIR=/data MAESTRO_MUSIC=/music MAESTRO_LISTEN=0.0.0.0:4533
VOLUME ["/data"]
EXPOSE 4533
ENTRYPOINT ["/sbin/tini", "--", "maestro"]
