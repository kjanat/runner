# Verification: does `npm i -g runner-run` resolve + run on PURE musl?
# Bare Alpine, NO libc6-compat (a glibc shim would mask a musl failure).
# `docker build` IS the test; any broken step fails the build.
#
# Run from the repo root:
#
# ```sh
# docker build --network=host -f tests/docker/musl-resolve.Dockerfile \
# --build-arg VER=0.10.0 --progress=plain --no-cache -t runner-musl-test .
# ```
#
# Override registry version with --build-arg VER=<x.y.z>.
# `--network=host` only needed where Docker's bridge/veth setup is blocked.
#
# What this actually probes: facade `lib/resolve.cjs` does NOT inspect
# platform/arch/libc itself; it walks `optionalDependencies` key order and
# returns the FIRST sub-package whose bin exists. So correctness on Alpine
# hinges entirely on npm having installed ONLY the musl sub-package (npm
# filtering by the `libc` field). If npm also keeps the -gnu package, the
# shim can hand back a glibc binary on musl. Steps 3-6 below make that
# visible instead of letting cargo-binstall mask it.
FROM alpine:3.22@sha256:310c62b5e7ca5b08167e4384c68db0fd2905dd9c7493756d356e893909057601

ARG VER=0.10.0
ENV RR_ROOT=/usr/local/lib/node_modules/runner-run

# ── 1. environment: prove this is musl, show toolchain versions ───────────
RUN set -eux; \
    apk add --no-cache nodejs npm file; \
    echo "================ ENVIRONMENT ================"; \
    echo "alpine    : $(cat /etc/alpine-release)"; \
    echo "node      : $(node --version)"; \
    echo "npm       : $(npm --version)"; \
    echo "libc      : $(ls /lib/ld-musl-* 2>/dev/null || echo 'NO musl loader?!')"; \
    ( ldd --version 2>&1 | head -1 ) || true; \
    if [ -e /lib/libc.musl-*.so.1 ] || ls /lib/ld-musl-* >/dev/null 2>&1; then \
        echo "PASS: confirmed musl libc environment"; \
    else echo "FAIL: not a musl environment"; exit 1; fi

# ── 2. install: full npm output, then the resolved dependency tree ────────
RUN set -eux; \
    echo "================ npm i -g runner-run@${VER} (pure musl) ================"; \
    npm i -g --omit=dev --foreground-scripts "runner-run@${VER}" 2>&1; \
    echo "---------------- npm ls -g (depth 2) ----------------"; \
    npm ls -g --depth=2 2>&1 || true

# ── 3. which @runner-run/* sub-packages did npm actually keep? ────────────
# The decisive question. Expect ONLY linux-x64-musl. If a -gnu package is
# also present, dump its os/cpu/libc so we can see why npm kept it.
RUN set -eux; \
    echo "================ INSTALLED @runner-run SUB-PACKAGES ================"; \
    DIR="${RR_ROOT}/node_modules/@runner-run"; \
    [ -d "${DIR}" ] || DIR="/usr/local/lib/node_modules/@runner-run"; \
    echo "scanning: ${DIR}"; \
    ls -1 "${DIR}" 2>/dev/null || { echo "FAIL: no @runner-run sub-packages installed at all"; exit 1; }; \
    for p in "${DIR}"/*; do \
        [ -d "${p}" ] || continue; \
        echo "---- $(basename "${p}") ----"; \
        node -e 'const j=require(process.argv[1]+"/package.json");console.log(JSON.stringify({name:j.name,version:j.version,os:j.os,cpu:j.cpu,libc:j.libc},null,0))' "${p}"; \
    done; \
    COUNT=$(ls -1 "${DIR}" | wc -l); \
    echo "sub-package count: ${COUNT}"; \
    if ls -1 "${DIR}" | grep -q -- '-gnu' && ls -1 "${DIR}" | grep -q -- 'musl'; then \
        echo "WARN: BOTH -gnu and -musl present, npm did NOT libc-filter; resolve order now decides correctness"; \
    fi

# ── 4. what does the resolve shim hand back? (correct API: resolveBinary) ──
RUN set -eux; \
    echo "================ resolveBinary() OUTPUT ================"; \
    for name in runner run; do \
        P=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary(process.argv[1]))' "${name}" 2>&1) \
            || { echo "FAIL: resolveBinary(${name}) threw:"; echo "${P}"; exit 1; }; \
        echo "resolveBinary(${name}) -> ${P}"; \
        case "${P}" in \
            *musl*) echo "PASS: ${name} resolved to a musl sub-package" ;; \
            *gnu*) echo "FAIL: ${name} resolved to a GLIBC (-gnu) build on musl; shim picked wrong"; exit 1 ;; \
            *) echo "WARN: ${name} path has no libc marker: ${P}" ;; \
        esac; \
    done

# ── 5. binary linkage: file(1) + ldd must say musl, never glibc ───────────
RUN set -eux; \
    echo "================ BINARY LINKAGE ================"; \
    for name in runner run; do \
        BIN=$(node -e 'process.stdout.write(require(process.env.RR_ROOT+"/lib/resolve.cjs").resolveBinary(process.argv[1]))' "${name}"); \
        echo "---- ${name}: ${BIN} ----"; \
        file "${BIN}"; \
        echo "ldd:"; ldd "${BIN}" 2>&1 || true; \
        if ldd "${BIN}" 2>&1 | grep -qi 'musl'; then echo "PASS: ${name} musl-linked"; \
        elif file "${BIN}" | grep -qi 'static'; then echo "PASS: ${name} static (libc-agnostic)"; \
        else echo "FAIL: ${name} not musl-linked / not static"; exit 1; fi; \
        if ldd "${BIN}" 2>&1 | grep -qi 'gnu'; then echo "FAIL: ${name} pulls glibc"; exit 1; fi; \
    done

# ── 6. the real proof: the binaries must actually execute under musl ──────
RUN set -eux; \
    echo "================ EXECUTION ================"; \
    echo "--- runner --version ---"; runner --version; \
    echo "--- run --version ---"; run --version; \
    echo "--- runner --help (head) ---"; runner --help 2>&1 | head -5; \
    echo "================================================"; \
    echo "ALL CHECKS PASSED. npm/musl path is sound for VER=${VER}"

CMD ["sh", "-c", "echo 'musl smoke:'; runner --version && run --version && echo OK"]
