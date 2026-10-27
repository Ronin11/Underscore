# The map server, built from the repository root:
#   docker build -f deploy/map-server/Dockerfile -t underscore-maps .
FROM rust:1.98.1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p underscore-mapserver

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/underscore-maps /usr/local/bin/underscore-maps
VOLUME /data
EXPOSE 8790
USER 1000:1000
CMD ["underscore-maps"]
