# probatum in a box — for containerized pipelines (e.g. a cidx preset).
#
# Build the static binary first, then the image:
#   cargo build --release --target x86_64-unknown-linux-musl
#   docker build -t probatum .
#
# alpine, not scratch: `run:` checks spawn `sh -c`, so the image needs a shell
# (busybox). Project toolchains (cargo, python…) are NOT shipped here — in a
# real pipeline the workspace's own tooling image runs those; this image only
# carries probatum itself (get:/log:/sh checks).
FROM alpine:3.20
COPY target/x86_64-unknown-linux-musl/release/probatum /usr/local/bin/probatum
WORKDIR /work
ENTRYPOINT ["probatum"]
CMD ["run"]

LABEL org.opencontainers.image.source="https://github.com/probatum-org/probatum"
LABEL org.opencontainers.image.description="Test-oriented check runner: one config, embedded checks, only the failures that matter."
LABEL org.opencontainers.image.licenses="MIT"
