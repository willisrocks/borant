# The release image for hubs and VPS hosts. It copies the release binaries
# that the release workflow built, so a multi-arch build needs no Rust
# compile under emulation.
FROM debian:bookworm-slim
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates procps tmux \
 && rm -rf /var/lib/apt/lists/*
COPY dist/${TARGETARCH}/borant /usr/local/bin/borant
ENV BORANT_HOME=/var/lib/borant
VOLUME /var/lib/borant
ENTRYPOINT ["borant"]
CMD ["--help"]
