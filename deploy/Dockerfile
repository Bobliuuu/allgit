# Build:  docker build -f deploy/Dockerfile -t vcs-server .
# Run:    docker run -d --name vcs -p 127.0.0.1:8480:8480 \
#           -v vcs-data:/data -v $PWD/deploy/tokens.example:/etc/vcs/tokens:ro vcs-server
# S3:     add  -e AWS_ACCESS_KEY_ID=... -e AWS_SECRET_ACCESS_KEY=... and override the command:
#           vcs-server --backend 's3://bucket/prefix?endpoint=https://s3.example.com' \
#             --tokens /etc/vcs/tokens --listen 0.0.0.0:8480
# There is no TLS in the server: publish the port on loopback or a private network and put a
# TLS-terminating proxy in front of it (docs/deploy.md).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p vcs-server --bin vcs-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin vcs \
 && mkdir -p /data /etc/vcs && chown vcs /data
COPY --from=build /src/target/release/vcs-server /usr/local/bin/vcs-server
USER vcs
VOLUME /data
EXPOSE 8480
HEALTHCHECK --interval=30s --timeout=3s CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8480 && printf 'GET /healthz HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q 200 <&3"]
ENTRYPOINT ["vcs-server"]
CMD ["--root", "/data", "--tokens", "/etc/vcs/tokens", "--listen", "0.0.0.0:8480"]
