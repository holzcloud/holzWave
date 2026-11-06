# The image of the holzWave server, built from this tree in three stages: the page, the Rust build, the runtime.
#
#   docker build -f packaging/docker/Dockerfile -t holzwave-server .
#
# Run it as the Compose file in packaging/compose/ or the Helm chart in packaging/helm/holzwave/ do: read-only root, user
# 10001, every capability dropped and exactly CHOWN, KILL, SETGID and SETUID added. The spawn helper is the only file with
# capabilities (the same four, as file capabilities), so the server itself holds none and every person's terminal runs under
# a user id of its own. Do not add the option that stops a process from gaining privileges (`no-new-privileges`,
# `allowPrivilegeEscalation: false`): it would stop the file capabilities of the helper, and the server then refuses to start.
#
# The image makes no key, no certificate, no account and no password. The operator makes the key with `keygen`, brings the
# certificate (or lets the server get one through ACME) and makes the first administrator with the one-time setup token the
# first start prints on standard output.
#
# Build arguments: ZIG_VERSION (the terminal engine needs this exact version of Zig), RUSTUP_VERSION and RUST_VERSION,
# EXTRA_PACKAGES (Debian packages to add to the runtime image, for the tools people want in their terminals, for example
# "git vim-tiny curl"), VERSION and REVISION (the labels).
#
# Every base image is named by tag and digest. Zig and rustup are checked against a published checksum before they are used,
# the crates and the npm packages come from the lock files (`cargo --locked`, `npm ci`), and the Debian packages come from the
# repository of the pinned base image's release.

# ---- the page ---------------------------------------------------------------------------------------------------
FROM node:26.11.1-trixie-slim@sha256:193fe51b64e77981119c98c2002c9e32a70e2f006fb4d25068ce0558998917f0 AS page
WORKDIR /app
COPY app/package.json app/package-lock.json ./
RUN --mount=type=cache,id=holzwave-npm,target=/root/.npm \
    npm ci --ignore-scripts --no-audit --no-fund
COPY app/tsconfig.json ./
COPY app/scripts ./scripts
COPY app/vendor-src ./vendor-src
COPY app/src ./src
RUN npm run build

# ---- the Rust build ---------------------------------------------------------------------------------------------
FROM debian:trixie-20261027@sha256:2dc419c6d7fe3dad0a2f9fd469c34cc90b0f9ba642d8fde837fe3d7d34a0e7f1 AS build
ARG TARGETARCH
ARG ZIG_VERSION=0.17.1
ARG RUSTUP_VERSION=1.29.2
ARG RUST_VERSION=1.99.1
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    CARGO_TERM_COLOR=never

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates curl git jq pkg-config xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Zig: the address and the checksum come from ziglang.org's index, and the checksum is checked before the unpacking.
RUN set -eu; \
    case "$(uname -m)" in aarch64) key=aarch64-linux ;; x86_64) key=x86_64-linux ;; \
        *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;; esac; \
    index=$(curl --proto '=https' --tlsv1.2 -fsSL https://ziglang.org/download/index.json); \
    url=$(printf '%s' "$index" | jq -r --arg v "$ZIG_VERSION" --arg k "$key" '.[$v][$k].tarball'); \
    sha=$(printf '%s' "$index" | jq -r --arg v "$ZIG_VERSION" --arg k "$key" '.[$v][$k].shasum'); \
    case "$url" in https://ziglang.org/download/*) ;; *) echo "unexpected Zig url: $url" >&2; exit 1 ;; esac; \
    case "$sha" in *[!0-9a-f]* | "") echo "unexpected Zig checksum: $sha" >&2; exit 1 ;; esac; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/zig.tar.xz "$url"; \
    echo "$sha  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir /opt/zig; \
    tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; \
    rm /tmp/zig.tar.xz; \
    ln -s /opt/zig/zig /usr/local/bin/zig; \
    test "$(zig version)" = "$ZIG_VERSION"

# Rust: rustup-init of a pinned rustup, checked against the checksum rust-lang.org publishes next to it, then a pinned toolchain.
RUN set -eu; \
    triple="$(uname -m)-unknown-linux-gnu"; \
    base="https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${triple}"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/rustup-init "${base}/rustup-init"; \
    sha=$(curl --proto '=https' --tlsv1.2 -fsSL "${base}/rustup-init.sha256" | cut -d ' ' -f 1); \
    case "$sha" in *[!0-9a-f]* | "") echo "unexpected rustup checksum: $sha" >&2; exit 1 ;; esac; \
    echo "$sha  /tmp/rustup-init" | sha256sum -c -; \
    chmod 0755 /tmp/rustup-init; \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}"; \
    rm /tmp/rustup-init; \
    test "$(rustc --version | cut -d ' ' -f 2)" = "${RUST_VERSION}"

WORKDIR /src
COPY . .
COPY --from=page /app/dist ./app/dist

# The release build of the server and of the spawn helper with the default features: the `in-process-workers` feature, which
# runs a worker as the server's own user, is not in the image. The cache mounts keep the registry and the target directory
# between builds; the two binaries are copied out of the cache mount into a normal layer.
RUN --mount=type=cache,id=holzwave-cargo-registry,target=/opt/cargo/registry,sharing=locked \
    --mount=type=cache,id=holzwave-cargo-git,target=/opt/cargo/git,sharing=locked \
    --mount=type=cache,id=holzwave-zig,target=/root/.cache/zig,sharing=locked \
    --mount=type=cache,id=holzwave-target-${TARGETARCH},target=/src/target,sharing=locked \
    set -eu; \
    cargo build --release --locked -p holzwave-server -p holzwave-spawn-helper; \
    mkdir /out; \
    install -m 0755 target/release/holzwave-server /out/holzwave-server; \
    install -m 0755 target/release/holzwave-spawn-helper /out/holzwave-spawn-helper; \
    strip --strip-unneeded /out/holzwave-server /out/holzwave-spawn-helper

# ---- the runtime ------------------------------------------------------------------------------------------------
FROM debian:trixie-20261027-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4 AS runtime
ARG EXTRA_PACKAGES=""
ARG VERSION=0.0.1-beta2
ARG REVISION=unknown
ARG DEBIAN_FRONTEND=noninteractive

# bash for the terminals, ca-certificates for single sign-on and ACME, tini as the first process, libcap2-bin until the file
# capabilities are set. The user and the group 10001 have no home and no login shell. /data is the one volume, and
# /data/homes is the directory the spawn helper keeps the homes of the people in; both are mode 0711 (a person's worker, which
# runs under a user id of its own, walks through them to its own home, and the helper refuses a home directory that others can
# write). A fresh named volume copies both from the image. The key directory is the place a fresh named volume copies its owner
# from; /etc/holzwave and the binaries belong to root.
# shellcheck disable=SC2086
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends bash ca-certificates tini libcap2-bin ${EXTRA_PACKAGES}; \
    groupadd --gid 10001 holzwave; \
    useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin holzwave; \
    install -d -o root -g root -m 0755 /etc/holzwave; \
    install -d -o 10001 -g 10001 -m 0700 /etc/holzwave/key /run/holzwave; \
    install -d -o 10001 -g 10001 -m 0711 /data /data/homes

# The server is world-executable (the workers of the people run it); the helper is not: root and group 10001 only, so that a
# person cannot execute it. The paths are the defaults of workers.helper_path and workers.worker_path.
COPY --from=build --chown=0:0 --chmod=0755 /out/holzwave-server /usr/local/bin/holzwave-server
COPY --from=build --chown=0:10001 --chmod=0750 /out/holzwave-spawn-helper /usr/local/bin/holzwave-spawn-helper
COPY --chown=0:0 --chmod=0644 LICENSE /usr/share/doc/holzwave/LICENSE

# The file capabilities are set here, after the owner and the mode (a change of owner removes them), so that no build step has
# to carry extended attributes between stages. Then the tool goes, the package index goes, and no file keeps a setuid or setgid
# bit.
RUN set -eu; \
    setcap cap_setuid,cap_setgid,cap_chown,cap_kill=p /usr/local/bin/holzwave-spawn-helper; \
    test "$(getcap /usr/local/bin/holzwave-spawn-helper | wc -l)" = 1; \
    apt-get purge -y --auto-remove libcap2-bin; \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/* /var/log/apt /var/log/dpkg.log /var/log/alternatives.log; \
    find / -xdev -type f \( -perm -4000 -o -perm -2000 \) -exec chmod u-s,g-s {} +; \
    test -z "$(find / -xdev -type f \( -perm -4000 -o -perm -2000 \) -print)"

LABEL org.opencontainers.image.title="holzWave server" \
      org.opencontainers.image.description="The holzWave terminal in the browser: one server, terminals that run under a user id of their own" \
      org.opencontainers.image.source="https://github.com/holzcloud/holzWave" \
      org.opencontainers.image.licenses="GPL-3.0-only" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"

# The two paths are the only settings the image carries, and they are paths: the operator mounts the files.
ENV LANG=C.UTF-8 \
    HOLZWAVE_SERVER_CONFIG=/etc/holzwave/server.json \
    HOLZWAVE_SERVER_KEY_FILE=/etc/holzwave/key/server.key

USER 10001:10001
VOLUME /data
EXPOSE 8443
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/holzwave-server"]
CMD ["serve"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD ["/usr/local/bin/holzwave-server", "healthcheck"]
