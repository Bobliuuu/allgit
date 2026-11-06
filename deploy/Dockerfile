# Build:  docker build -f deploy/Dockerfile -t vcs-server .
# Run:    docker run -d --name vcs -p 127.0.0.1:8480:8480 \
#           -v vcs-data:/data -v $PWD/tokens:/etc/vcs/tokens:ro vcs-server   # your own tokens file (deploy/tokens.example is refused)
# S3:     add  -e AWS_ACCESS_KEY_ID=... -e AWS_SECRET_ACCESS_KEY=... and override the command:
#           vcs-server --backend 's3://bucket/prefix?endpoint=https://s3.example.com' \
#             --tokens /etc/vcs/tokens --listen 0.0.0.0:8480
# The image is built with the `tls` feature (--tls-cert/--tls-key, https:// S3 endpoints). Without a certificate it
# still speaks plain HTTP: publish the port on loopback or a private network, or terminate TLS in front (docs/deploy.md).
# Base images are pinned by digest (multi-arch index digests). Dependabot (docker ecosystem, /deploy) proposes bumps.
FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS build
WORKDIR /src
COPY . .
# tls: HTTPS listener (--tls-cert/--tls-key) and https:// S3 endpoints (real AWS S3 needs it).
RUN cargo build --release --locked -p vcs-server --bin vcs-server --features tls

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin vcs \
 && mkdir -p /data /etc/vcs && chown vcs /data
COPY --from=build /src/target/release/vcs-server /usr/local/bin/vcs-server
USER vcs
VOLUME /data
EXPOSE 8480
# /readyz: 200 only while the backend answers (cached 2 s server side); /healthz is plain liveness.
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8480; printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["vcs-server"]
CMD ["--root", "/data", "--tokens", "/etc/vcs/tokens", "--listen", "0.0.0.0:8480"]
