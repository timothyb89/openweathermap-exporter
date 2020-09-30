FROM clux/muslrust:1.46.0-stable as build

RUN mkdir /src
COPY Cargo.locl Cargo.toml /src/
COPY ./src /src/src

WORKDIR /src

RUN cargo build --release

FROM alpine:3.12
COPY --from=build \
  /src/target/x86_64-unknown-linux-musl/release/openweathermap-exporter \
  /usr/local/bin/
