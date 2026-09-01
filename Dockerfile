# syntax=docker/dockerfile:1

# The official Docker image (ghcr.io/waka/sghtmltopdf)
#
# * The bare binary is not distributed; only this server-mode image and the Ruby gem
# * Targets are linux/amd64 and linux/arm64 (Debian-based, glibc only)
# * Japanese fonts are bundled so Japanese PDFs work with no extra setup
# * ENTRYPOINT/CMD make it usable both ways: no arguments = server, arguments = CLI

ARG RUST_VERSION=1.96

# ---------------------------------------------------------------------------
# 1. Fetch the Japanese fonts to bundle
# ---------------------------------------------------------------------------
# BIZ UDPGothic / BIZ UDPMincho, Regular and Bold (SIL OFL 1.1, about 23MB total).
# They must be static TrueType (glyf).
#
# We reuse the same rust image as the build so no package has to be added just for curl (its buildpack-deps base already has curl and CA certificates)
# This only needs to run on the build host, hence --platform=$BUILDPLATFORM.
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS fonts
# Pinned to a google/fonts commit. When bumping it, update docker/fonts.sha256 too
ARG GOOGLE_FONTS_COMMIT=7ff85c87f93ea6cca5f41c69f2e4edcb90240f26
WORKDIR /fonts
COPY docker/fonts.sha256 ./
RUN set -eux; \
    base="https://raw.githubusercontent.com/google/fonts/${GOOGLE_FONTS_COMMIT}/ofl"; \
    curl -fsSL -o BIZUDPGothic-Regular.ttf "${base}/bizudpgothic/BIZUDPGothic-Regular.ttf"; \
    curl -fsSL -o BIZUDPGothic-Bold.ttf    "${base}/bizudpgothic/BIZUDPGothic-Bold.ttf"; \
    curl -fsSL -o BIZUDPMincho-Regular.ttf "${base}/bizudpmincho/BIZUDPMincho-Regular.ttf"; \
    curl -fsSL -o BIZUDPMincho-Bold.ttf    "${base}/bizudpmincho/BIZUDPMincho-Bold.ttf"; \
    curl -fsSL -o OFL-BIZUDPGothic.txt     "${base}/bizudpgothic/OFL.txt"; \
    curl -fsSL -o OFL-BIZUDPMincho.txt     "${base}/bizudpmincho/OFL.txt"; \
    sha256sum -c fonts.sha256; \
    rm fonts.sha256

# ---------------------------------------------------------------------------
# 2. Build the binary
# ---------------------------------------------------------------------------
# The build always runs on the build host's architecture (--platform=$BUILDPLATFORM)
# Running rustc under QEMU takes tens of minutes, so arm64 is cross-compiled
# Nothing links against system libraries, but ring (used by rustls) compiles C sources, so gcc and the target libc headers (libc6-dev-arm64-cross) are required
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS builder
ARG TARGETARCH
RUN set -eux; \
    case "${TARGETARCH}" in \
      amd64) target=x86_64-unknown-linux-gnu ;; \
      arm64) target=aarch64-unknown-linux-gnu; \
             apt-get update; \
             apt-get install -y --no-install-recommends \
                 gcc-aarch64-linux-gnu libc6-dev-arm64-cross; \
             rm -rf /var/lib/apt/lists/* ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    echo "${target}" > /target.txt; \
    rustup target add "${target}"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core ./core
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH} \
    set -eux; \
    target=$(cat /target.txt); \
    cargo build --release --locked --target "${target}"; \
    cp "target/${target}/release/sghtmltopdf" /usr/local/bin/sghtmltopdf

# ---------------------------------------------------------------------------
# 3. Runtime image
# ---------------------------------------------------------------------------
# TLS root certificates are compiled into the binary (rustls + webpki-roots), so ca-certificates is not needed
FROM debian:bookworm-slim
LABEL org.opencontainers.image.title="sghtmltopdf" \
      org.opencontainers.image.description="An HTML-to-PDF renderer that does not depend on Chromium, WebKit or Gecko" \
      org.opencontainers.image.source="https://github.com/waka/sghtmltopdf" \
      org.opencontainers.image.licenses="MIT"

COPY --from=builder /usr/local/bin/sghtmltopdf /usr/local/bin/sghtmltopdf
# Placed under a standard directory that fontdb scans (no fontconfig needed).
COPY --from=fonts /fonts/BIZUDP*.ttf /usr/share/fonts/truetype/sghtmltopdf/
COPY --from=fonts /fonts/OFL-*.txt /usr/share/doc/sghtmltopdf/fonts/

WORKDIR /work
USER 10001:10001

EXPOSE 8080
ENTRYPOINT ["sghtmltopdf"]
CMD ["server", "--listen", "0.0.0.0:8080"]
