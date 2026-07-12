# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

FROM node:22.22.0-bookworm-slim@sha256:dd9d21971ec4395903fa6143c2b9267d048ae01ca6d3ea96f16cb30df6187d94 AS runtime-builder

WORKDIR /build

COPY package.json package-lock.json ./
COPY licenses/container/workerd-native/LOCK.json ./workerd-native-lock.json
RUN npm ci --omit=dev --no-audit --no-fund \
    && node -e 'const c=require("node:crypto"),f=require("node:fs"),l=require("./workerd-native-lock.json");const a=process.arch==="x64"?"linux-amd64":process.arch==="arm64"?"linux-arm64":"";if(!a)throw new Error(`unsupported workerd architecture: ${process.arch}`);const p="node_modules/workerd/bin/workerd",h=c.createHash("sha256").update(f.readFileSync(p)).digest("hex"),e=l.workerd.architectures[a].binary_sha256;if(h!==e)throw new Error(`workerd binary digest mismatch for ${a}`)' \
    && test "$(node_modules/workerd/bin/workerd --version)" = "workerd 2026-07-08"

COPY rootfs/usr/local/bin/secure-snapshot.c ./secure-snapshot.c
# The slim builder has no CA bundle yet. APT Release signatures and package
# hashes authenticate this immutable HTTP snapshot. Only the resulting
# project-owned executable enters the runtime image; its GCC/glibc source
# versions are also present in the release's exact Debian source closure.
RUN printf '%s\n' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://snapshot.debian.org/archive/debian/20260712T000000Z bookworm main' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://snapshot.debian.org/archive/debian/20260712T000000Z bookworm-updates main' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] http://snapshot.debian.org/archive/debian-security/20260712T000000Z bookworm-security main' \
      > /tmp/ha-voice-hermes-build.list \
    && apt-get \
      -o Dir::Etc::sourcelist=/tmp/ha-voice-hermes-build.list \
      -o Dir::Etc::sourceparts=- \
      -o Acquire::Check-Valid-Until=false \
      update \
    && apt-get \
      -o Dir::Etc::sourcelist=/tmp/ha-voice-hermes-build.list \
      -o Dir::Etc::sourceparts=- \
      -o Acquire::Check-Valid-Until=false \
      install --yes --no-install-recommends \
        gcc=4:12.2.0-3 \
        libc6-dev=2.36-9+deb12u14 \
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
    && rm -f /tmp/ha-voice-hermes-build.list \
    && rm -rf /var/lib/apt/lists/*

FROM ghcr.io/home-assistant/base-debian:bookworm@sha256:8c7a9e207425e79b6b2ed1628a2b6727fa6e518d9fdddcbe3b1ac20440e70492 AS app-rootfs

# The current pinned Home Assistant base predates Debian's July jq security
# rebuild and contains the optional tempio helper compiled with a vulnerable Go
# toolchain. This App uses jq but does not use tempio, so install the exact fixed
# jq packages and remove the unreachable helper from the release image. The
# authenticated snapshot makes the exact package pin reproducible after Debian
# publishes a later stable update.
RUN printf '%s\n' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://snapshot.debian.org/archive/debian/20260712T000000Z bookworm main' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://snapshot.debian.org/archive/debian/20260712T000000Z bookworm-updates main' \
      'deb [check-valid-until=no signed-by=/usr/share/keyrings/debian-archive-keyring.gpg] https://snapshot.debian.org/archive/debian-security/20260712T000000Z bookworm-security main' \
      > /tmp/ha-voice-hermes-runtime.list \
    && apt-get \
      -o Dir::Etc::sourcelist=/tmp/ha-voice-hermes-runtime.list \
      -o Dir::Etc::sourceparts=- \
      -o Acquire::Check-Valid-Until=false \
      update \
    && apt-get \
      -o Dir::Etc::sourcelist=/tmp/ha-voice-hermes-runtime.list \
      -o Dir::Etc::sourceparts=- \
      -o Acquire::Check-Valid-Until=false \
      install --yes --no-install-recommends \
      jq=1.6-2.1+deb12u2 \
      libjq1=1.6-2.1+deb12u2 \
    && rm -f /usr/bin/tempio \
    && rm -f /tmp/ha-voice-hermes-runtime.list \
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
    && find /usr/share/licenses/ha-voice-hermes -type d -exec chmod 0555 {} + \
    && find /usr/share/licenses/ha-voice-hermes -type f -exec chmod 0444 {} + \
    && test -s /opt/ha-voice-hermes/gateway/index.js \
    && test -s /opt/ha-voice-hermes/gateway/index_bg.wasm \
    && test -s /usr/share/licenses/ha-voice-hermes/container/COMPONENTS.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/debian-sources.lock.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-rust/THIRD_PARTY_NOTICES.md \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-rust/UPSTREAM_CARGO.lock \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-native/LOCK.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-native/STATUS.md \
    && jq -e \
      '.legal_gate.status == "covered" and .unresolved == [] and (.components | length == 24) and (.blobs | length == 113)' \
      /usr/share/licenses/ha-voice-hermes/container/workerd-native/LOCK.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/bearssl-3d9be2f60b7764e46836514bcd6e453abdfa864a-MIT.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/bashio-0.17.5-MIT.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/s6-overlay-3.2.2.0-ISC.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/s6-overlay-helpers-0.1.2.1-ISC.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/skarnet-2011-2025-ISC.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/skarnet-2011-2026-ISC.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/skarnet-2015-2025-ISC.txt \
    && test -s /usr/share/licenses/ha-voice-hermes/container/skarnet-2015-2026-ISC.txt \
    && for component in \
      /package/admin/execline-2.9.8.1 \
      /package/admin/s6-2.14.0.1 \
      /package/admin/s6-linux-init-1.2.0.0 \
      /package/admin/s6-linux-utils-2.6.4.0 \
      /package/admin/s6-overlay-3.2.2.0 \
      /package/admin/s6-overlay-helpers-0.1.2.1 \
      /package/admin/s6-portable-utils-2.3.1.1 \
      /package/admin/s6-rc-0.6.0.0 \
      /package/net/s6-networking-2.7.2.1 \
      /package/prog/skalibs-2.14.5.1 \
      /package/web/s6-dns-2.4.1.1 \
      /usr/lib/bashio; do \
        test -e "${component}"; \
      done \
    && missing_notices="$( \
      for package in $(dpkg-query -W -f='${Package}\n'); do \
        test -r "/usr/share/doc/${package}/copyright" || echo "${package}"; \
      done \
    )" \
    && if test -n "${missing_notices}"; then \
      echo "Installed dpkg packages without copyright records: ${missing_notices}" >&2; \
      exit 1; \
    fi \
    && test "$(/usr/local/bin/workerd --version)" = "workerd 2026-07-08"

# Flatten the prepared root filesystem into a scratch stage. This is a licence
# and supply-chain boundary, not a size optimization: files deleted above (in
# particular the unused tempio binary) must not remain distributable through
# an inherited OCI layer.
FROM scratch

ARG BUILD_ARCH
ARG BUILD_DATE
ARG BUILD_DESCRIPTION
ARG BUILD_NAME
ARG BUILD_REVISION
ARG BUILD_VERSION

LABEL \
  io.hass.arch="${BUILD_ARCH}" \
  io.hass.base.name="debian" \
  io.hass.base.version="2026.06.1" \
  io.hass.description="${BUILD_DESCRIPTION}" \
  io.hass.name="${BUILD_NAME}" \
  io.hass.type="app" \
  io.hass.version="${BUILD_VERSION}" \
  org.opencontainers.image.created="${BUILD_DATE}" \
  org.opencontainers.image.base.digest="sha256:8c7a9e207425e79b6b2ed1628a2b6727fa6e518d9fdddcbe3b1ac20440e70492" \
  org.opencontainers.image.base.name="ghcr.io/home-assistant/base-debian:bookworm" \
  org.opencontainers.image.description="${BUILD_DESCRIPTION}" \
  org.opencontainers.image.documentation="https://github.com/troykelly/ha-voice-hermes/blob/main/LICENSE.md" \
  org.opencontainers.image.licenses="MIT AND LicenseRef-ThirdParty-See-Notices" \
  org.opencontainers.image.revision="${BUILD_REVISION}" \
  org.opencontainers.image.source="https://github.com/troykelly/ha-voice-hermes" \
  org.opencontainers.image.title="${BUILD_NAME}" \
  org.opencontainers.image.version="${BUILD_VERSION}"

COPY --from=app-rootfs / /

RUN test ! -e /usr/bin/tempio \
    && test -x /init \
    && test -x /usr/local/bin/workerd \
    && test -s /usr/share/licenses/ha-voice-hermes/container/COMPONENTS.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/debian-sources.lock.json \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-rust/THIRD_PARTY_NOTICES.md \
    && test -s /usr/share/licenses/ha-voice-hermes/container/workerd-native/LOCK.json

ENV \
  LANG="C.UTF-8" \
  DEBIAN_FRONTEND="noninteractive" \
  CURL_CA_BUNDLE="/etc/ssl/certs/ca-certificates.crt" \
  S6_BEHAVIOUR_IF_STAGE2_FAILS=2 \
  S6_CMD_WAIT_FOR_SERVICES_MAXTIME=0 \
  S6_CMD_WAIT_FOR_SERVICES=1 \
  S6_SERVICES_READYTIME=50

EXPOSE 8443/tcp
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD ["/usr/local/bin/gateway-healthcheck"]

ENTRYPOINT ["/init"]
