# Does the facade pick the musl build when BOTH Linux libc variants are
# installed? Pure Alpine, no libc6-compat to mask a wrong choice. The build is
# the test. Uses the working-tree resolver against registry platform packages.
#
#   docker build -f tests/docker/musl-libc-select.Dockerfile \
#     --build-arg VER=0.25.0 --progress=plain --no-cache -t runner-musl-libc-test .
# Index digest, not a per-platform one: the build has to resolve to the host's
# architecture. A `linux/386` manifest digest here makes `process.arch` report
# `ia32`, and no Linux GNU/musl pair is published for it.
FROM alpine:latest@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b

ARG VER=0.25.0
ENV RR_ROOT=/usr/local/lib/node_modules/runner-run

RUN set -eux; \
    apk add --no-cache nodejs npm ca-certificates; \
    ls /lib/ld-musl-* >/dev/null 2>&1 && ! [ -e /lib/libc.so.6 ] \
        || { echo "FAIL: not a pure musl environment"; exit 1; }; \
    npm i -g --omit=dev "runner-run@${VER}"

COPY npm/facade/lib/resolve.cjs /usr/local/lib/node_modules/runner-run/lib/resolve.cjs

# npm honours the `libc` field, so the GNU sibling has to be planted by hand.
# That tree is what Bun and Deno produce on their own.
RUN set -eux; \
    DIR="${RR_ROOT}/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || DIR="/usr/local/lib/node_modules/@runner-run"; \
    ARCH="$(node -p 'process.arch')"; \
    case "${ARCH}" in x64 | arm64) ;; \
        *) echo "FAIL: no GNU/musl pair is published for linux-${ARCH}; build this on x64 or arm64"; exit 1 ;; \
    esac; \
    GNU="linux-${ARCH}-gnu"; \
    TARBALL="$(npm view "@runner-run/${GNU}@${VER}" dist.tarball)"; \
    [ -n "${TARBALL}" ] || { echo "FAIL: no tarball for @runner-run/${GNU}@${VER}"; exit 1; }; \
    wget -qO /tmp/gnu.tgz "${TARBALL}"; \
    mkdir -p "${DIR}/${GNU}"; \
    tar -xzf /tmp/gnu.tgz -C "${DIR}/${GNU}" --strip-components=1; \
    ls -1 "${DIR}"

RUN set -eux; \
    node -e 'const {libc,source}=require(process.env.RR_ROOT+"/lib/resolve.cjs").detectLibc(); \
        console.log("detected",libc,"via",source); \
        if(libc!=="musl"){console.error("FAIL: detected "+libc+" on Alpine");process.exit(1)}'; \
    for name in runner run; do \
        P=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary(process.argv[1]))' "${name}"); \
        echo "resolveBinary(${name}) -> ${P}"; \
        case "${P}" in \
            *musl*) ;; \
            *) echo "FAIL: ${name} did not resolve to the musl package"; exit 1 ;; \
        esac; \
    done; \
    runner --version; \
    run --version

# With the musl package gone, the facade must refuse the GNU build by name
# instead of handing back a path that spawns into a bare ENOENT.
RUN set -eux; \
    DIR="${RR_ROOT}/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || DIR="/usr/local/lib/node_modules/@runner-run"; \
    MUSL="$(ls -1 "${DIR}" | grep -- '-musl$')"; \
    mv "${DIR}/${MUSL}" "/tmp/${MUSL}"; \
    OUT=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary("run"))' 2>&1) \
        && { echo "FAIL: resolved ${OUT} with only the GNU build present"; exit 1; }; \
    echo "${OUT}" | grep -q "No usable musl binary" || { echo "FAIL: diagnostic is not libc-specific"; exit 1; }; \
    echo "${OUT}" | grep -q -- "-gnu" || { echo "FAIL: diagnostic does not name the incompatible package"; exit 1; }; \
    node "${RR_ROOT}/bin/run.cjs" --version 2>&1 | grep -q "refusing to spawn" \
        || { echo "FAIL: launcher did not surface the libc error"; exit 1; }; \
    mv "/tmp/${MUSL}" "${DIR}/${MUSL}"; \
    echo "PASS: libc selection is sound for ${VER}"

CMD ["sh", "-c", "runner --version && run --version"]
