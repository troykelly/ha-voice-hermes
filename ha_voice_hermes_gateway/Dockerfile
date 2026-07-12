# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

FROM node:22.22.0-bookworm-slim@sha256:dd9d21971ec4395903fa6143c2b9267d048ae01ca6d3ea96f16cb30df6187d94 AS runtime-builder

WORKDIR /build

COPY package.json package-lock.json ./
RUN npm ci --omit=dev --no-audit --no-fund \
    && test "$(node_modules/workerd/bin/workerd --version)" = "workerd 2026-07-08"

COPY rootfs/usr/local/bin/secure-snapshot.c ./secure-snapshot.c
RUN apt-get update \
    && apt-get install --yes --no-install-recommends gcc libc6-dev \
    && gcc \
      -std=c17 \
      -O2 \
      -Wall \
      -Wextra \
      -Werror \
      -D_FORTIFY_SOURCE=3 \
      -fstack-protector-strong \
      -fPIE \
      -Wl,-z,relro,-z,now \
      -pie \
      -o /build/secure-snapshot \
      /build/secure-snapshot.c \
    && strip /build/secure-snapshot \
    && rm -rf /var/lib/apt/lists/*

FROM ghcr.io/home-assistant/base-debian:bookworm@sha256:8c7a9e207425e79b6b2ed1628a2b6727fa6e518d9fdddcbe3b1ac20440e70492

ARG BUILD_ARCH
ARG BUILD_DATE
ARG BUILD_DESCRIPTION
ARG BUILD_NAME
ARG BUILD_REVISION
ARG BUILD_VERSION

LABEL \
  io.hass.arch="${BUILD_ARCH}" \
  io.hass.description="${BUILD_DESCRIPTION}" \
  io.hass.name="${BUILD_NAME}" \
  io.hass.type="app" \
  io.hass.version="${BUILD_VERSION}" \
  org.opencontainers.image.created="${BUILD_DATE}" \
  org.opencontainers.image.description="${BUILD_DESCRIPTION}" \
  org.opencontainers.image.licenses="MIT AND Apache-2.0" \
  org.opencontainers.image.revision="${BUILD_REVISION}" \
  org.opencontainers.image.source="https://github.com/troykelly/ha-voice-hermes" \
  org.opencontainers.image.title="${BUILD_NAME}" \
  org.opencontainers.image.version="${BUILD_VERSION}"

# The current pinned Home Assistant base predates Debian's July jq security
# rebuild and contains the optional tempio helper compiled with a vulnerable Go
# toolchain. This App uses jq but does not use tempio, so install the exact fixed
# jq packages and remove the unreachable helper from the release image.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
      jq=1.6-2.1+deb12u2 \
      libjq1=1.6-2.1+deb12u2 \
    && rm -f /usr/bin/tempio \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 gateway \
    && useradd \
      --uid 1000 \
      --gid 1000 \
      --home-dir /nonexistent \
      --no-create-home \
      --shell /usr/sbin/nologin \
      gateway \
    && install -d -m 0755 -o root -g root /opt/ha-voice-hermes/gateway

COPY rootfs /
COPY --from=runtime-builder /build/node_modules/workerd/bin/workerd /usr/local/bin/workerd
COPY --from=runtime-builder /build/secure-snapshot /usr/local/bin/secure-snapshot
COPY gateway-build/index.js /opt/ha-voice-hermes/gateway/index.js
COPY gateway-build/index_bg.wasm /opt/ha-voice-hermes/gateway/index_bg.wasm
COPY gateway-build/SHA256SUMS /opt/ha-voice-hermes/gateway/SHA256SUMS
COPY licenses /usr/share/licenses/ha-voice-hermes

RUN cd /opt/ha-voice-hermes/gateway \
    && sha256sum --check SHA256SUMS \
    && rm -f SHA256SUMS /usr/local/bin/secure-snapshot.c \
    && chmod 0555 \
      /usr/local/bin/workerd \
      /usr/local/bin/secure-snapshot \
    && chmod 0755 \
      /usr/local/bin/gateway-healthcheck \
      /usr/local/bin/gateway-supervisor \
      /usr/local/bin/prepare-runtime \
      /etc/services.d/gateway/run \
      /etc/services.d/gateway/finish \
    && chmod 0444 \
      /usr/local/bin/validate-options.jq \
      /opt/ha-voice-hermes/gateway/index.js \
      /opt/ha-voice-hermes/gateway/index_bg.wasm \
      /usr/share/licenses/ha-voice-hermes/Apache-2.0.txt \
      /usr/share/licenses/ha-voice-hermes/MIT.txt \
    && test -s /opt/ha-voice-hermes/gateway/index.js \
    && test -s /opt/ha-voice-hermes/gateway/index_bg.wasm \
    && test "$(/usr/local/bin/workerd --version)" = "workerd 2026-07-08"

ENV S6_BEHAVIOUR_IF_STAGE2_FAILS=2

EXPOSE 8443/tcp
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/gateway-healthcheck"]

ENTRYPOINT ["/init"]
