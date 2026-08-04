# Verification: does the facade pick the musl build when BOTH Linux libc
# variants are installed? Bare Alpine, NO libc6-compat — a glibc shim would
# let the wrong choice succeed and hide the bug.
# `docker build` IS the test; any broken step fails the build.
#
# Run from the repo root:
#
# ```sh
# docker build --network=host -f tests/docker/musl-libc-select.Dockerfile \
# --build-arg VER=0.24.1 --progress=plain --no-cache -t runner-musl-libc-test .
# ```
#
# Override the registry version with --build-arg VER=<x.y.z>.
# `--network=host` only needed where Docker's bridge/veth setup is blocked.
#
# Unlike musl-resolve.Dockerfile, which tests the published tarball as-is, this
# grafts the WORKING TREE `lib/resolve.cjs` onto the installed package: the
# selection logic under test is the one in this checkout. The platform packages
# and the facade around them still come from the registry, so the binaries that
# get spawned are the real published ones.
#
# npm honours the `libc` field and installs only the musl package, so the GNU
# sibling is fetched from the registry and unpacked next to it — that is exactly
# the tree Bun and Deno produce on their own, and the tree you get by copying
# `node_modules` off a glibc machine.
FROM alpine:3.22@sha256:310c62b5e7ca5b08167e4384c68db0fd2905dd9c7493756d356e893909057601

ARG VER=0.24.1
ENV RR_ROOT=/usr/local/lib/node_modules/runner-run

# ── 1. environment: prove this is musl, show toolchain versions ───────────
RUN set -eux; \
    apk add --no-cache nodejs npm ca-certificates; \
    echo "================ ENVIRONMENT ================"; \
    echo "alpine    : $(cat /etc/alpine-release)"; \
    echo "node      : $(node --version)"; \
    echo "npm       : $(npm --version)"; \
    echo "arch      : $(node -p 'process.arch')"; \
    echo "libc      : $(ls /lib/ld-musl-* 2>/dev/null || echo 'NO musl loader?!')"; \
    if ls /lib/ld-musl-* >/dev/null 2>&1 && ! [ -e /lib/libc.so.6 ]; then \
        echo "PASS: confirmed pure musl environment"; \
    else echo "FAIL: not a pure musl environment"; exit 1; fi

# ── 2. install the published facade (npm keeps the musl package only) ─────
RUN set -eux; \
    echo "================ npm i -g runner-run@${VER} ================"; \
    npm i -g --omit=dev "runner-run@${VER}" 2>&1

# The working-tree resolver replaces the published one; everything else stays.
COPY npm/facade/lib/resolve.cjs /usr/local/lib/node_modules/runner-run/lib/resolve.cjs

# ── 3. graft the GNU sibling in, reproducing a Bun/Deno install ───────────
RUN set -eux; \
    echo "================ PLANTING THE GNU SIBLING ================"; \
    DIR="${RR_ROOT}/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || DIR="/usr/local/lib/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || { echo "FAIL: no @runner-run sub-packages installed at all"; exit 1; }; \
    ARCH="$(node -p 'process.arch')"; \
    GNU="linux-${ARCH}-gnu"; \
    TARBALL="$(npm view "@runner-run/${GNU}@${VER}" dist.tarball)"; \
    echo "fetching ${TARBALL}"; \
    wget -qO /tmp/gnu.tgz "${TARBALL}"; \
    mkdir -p "${DIR}/${GNU}"; \
    tar -xzf /tmp/gnu.tgz -C "${DIR}/${GNU}" --strip-components=1; \
    chmod +x "${DIR}/${GNU}"/bin/*; \
    echo "installed sub-packages:"; ls -1 "${DIR}"; \
    ls -1 "${DIR}" | grep -q -- "-gnu$" || { echo "FAIL: GNU sibling did not land"; exit 1; }; \
    ls -1 "${DIR}" | grep -q -- "-musl$" || { echo "FAIL: musl package missing"; exit 1; }

# ── 4. libc detection against a real musl root, no stubs ─────────────────
RUN set -eux; \
    echo "================ detectLibc() ================"; \
    node -e 'const {libc,source}=require(process.env.RR_ROOT+"/lib/resolve.cjs").detectLibc(); \
        console.log("libc:",libc,"via",source); \
        if(libc!=="musl"){console.error("FAIL: detected "+libc+" on Alpine");process.exit(1)} \
        console.log("PASS: musl detected from a real root filesystem")'

# ── 5. the decisive check: musl wins with both variants installed ─────────
RUN set -eux; \
    echo "================ resolveBinary() WITH BOTH VARIANTS ================"; \
    for name in runner run; do \
        P=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary(process.argv[1]))' "${name}" 2>&1) \
            || { echo "FAIL: resolveBinary(${name}) threw:"; echo "${P}"; exit 1; }; \
        echo "resolveBinary(${name}) -> ${P}"; \
        case "${P}" in \
            *musl*) echo "PASS: ${name} resolved to the musl sub-package" ;; \
            *gnu*) echo "FAIL: ${name} resolved to the GLIBC build on musl"; exit 1 ;; \
            *) echo "FAIL: ${name} path has no libc marker: ${P}"; exit 1 ;; \
        esac; \
    done

# ── 6. the binaries the facade hands back must actually execute ───────────
RUN set -eux; \
    echo "================ EXECUTION ================"; \
    runner --version; \
    run --version

# ── 7. wrong-libc-only: fail in the facade, never spawn the GNU build ─────
RUN set -eux; \
    echo "================ GNU-ONLY TREE ================"; \
    DIR="${RR_ROOT}/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || DIR="/usr/local/lib/node_modules/@runner-run"; \
    MUSL="$(ls -1 "${DIR}" | grep -- '-musl$')"; \
    mv "${DIR}/${MUSL}" "/tmp/${MUSL}"; \
    echo "remaining: $(ls -1 "${DIR}")"; \
    OUT=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary("run"))' 2>&1) \
        && { echo "FAIL: resolveBinary returned ${OUT} with only the GNU build present"; exit 1; }; \
    echo "${OUT}"; \
    echo "${OUT}" | grep -q "No musl binary installed" \
        || { echo "FAIL: diagnostic is not libc-specific"; exit 1; }; \
    echo "${OUT}" | grep -q -- "-gnu" \
        || { echo "FAIL: diagnostic does not name the incompatible package"; exit 1; }; \
    echo "PASS: facade refused the GNU build with a libc diagnostic"; \
    echo "--- the launcher surfaces it too, instead of an opaque ENOENT ---"; \
    node "${RR_ROOT}/bin/run.cjs" --version 2>&1 | grep -q "refusing to spawn" \
        || { echo "FAIL: launcher did not surface the libc error"; exit 1; }; \
    mv "/tmp/${MUSL}" "${DIR}/${MUSL}"; \
    echo "================================================"; \
    echo "ALL CHECKS PASSED. libc selection is sound for VER=${VER}"

CMD ["sh", "-c", "echo 'musl libc-select smoke:'; runner --version && run --version && echo OK"]
