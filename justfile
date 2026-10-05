# https://just.systems
set unstable

# Version/triple live in recipe parameter defaults (evaluated per invocation), not globals; just evaluates globals on every run.
build-pkgscript := "packaging" / "npm" / "scripts" / "build-packages.ts"
downloads-dir := "packaging" / "npm" / "downloads"
targets-json := "packaging" / "npm" / "targets.json"

schema-dir := "schemas"

lint-targets := "x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-unknown-freebsd aarch64-unknown-linux-gnu wasm32-wasip1 wasm32-wasip2"
# Test builds for these hit rust-lang/rust-clippy#17566.
lint-targets-no-tests := "x86_64-unknown-illumos x86_64-pc-solaris"
lint-targets-libraries := "wasm32-unknown-unknown"

[arg('bin', pattern='run|runner')]
[arg('profile', pattern='dev|release|')]
[group('bins')]
default bin=env("BIN", "runner") profile="dev" *args:
    env PROFILE={{ profile }} just {{ bin }} {{ args }}

[group('bins')]
run *args:
    cargo bin-run --profile={{ env("PROFILE", "dev") }} -- {{ args }}

[group('bins')]
runner *args:
    cargo bin-runner --profile={{ env("PROFILE", "dev") }} -- {{ args }}

ls:
    @just --list

# Shell lint at the strictest level, matching CI. -o all enables the optional
# checks; default severity misses SC231x entirely.
[doc('Lint shell scripts at the strictest level, matching CI')]
[group('lint')]
lint-sh:
    shellcheck -x -o all .github/scripts/*.sh install.sh

[doc('Lint every crate for each target the host does not compile, matching CI')]
[group('lint')]
lint-targets:
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target add {{ lint-targets }} {{ lint-targets-no-tests }} {{ lint-targets-libraries }}
    clippy() {
        echo "→ clippy {{ BLUE }}${1}{{ NORMAL }}"
        cargo clippy --workspace --all-features --target "$@" -- -D warnings -D clippy::all
    }
    for target in {{ lint-targets }}; do clippy "${target}" --all-targets; done
    for target in {{ lint-targets-no-tests }}; do clippy "${target}" --lib --bins; done
    for target in {{ lint-targets-libraries }}; do clippy "${target}" --all-targets --exclude runner-run; done

[doc('Lint every feature combination of the CLI, matching CI')]
[group('lint')]
lint-features:
    cargo hack clippy --feature-powerset --package runner-run --all-targets -- -D warnings -D clippy::all

# Drift guard: just gen-schema && git diff --exit-code schemas/
[doc('Regenerate the committed JSON Schemas')]
[group('schema')]
gen-schema:
    @echo "→ regenerating {{ BLUE }}{{ schema-dir }}{{ NORMAL }}"
    @cargo schema --all --output {{ schema-dir }}

[group('npm')]
test-facade:
    bun run test:facade

[group('npm')]
build-packages only="" skip="false" version=`cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "runner-run") | .version'`:
    #!/usr/bin/env bash
    set -euo pipefail
    args=("--version" "{{ version }}")
    if [[ -n "{{ only }}" ]]; then args+=("--only={{ only }}"); fi
    if [[ "{{ skip }}" == "true" || "{{ skip }}" == "1" ]]; then args+=("--skip-missing"); fi
    echo "→ building packages with args: {{ BLUE }}${args[*]}{{ NORMAL }}"
    node {{ build-pkgscript }} "${args[@]}"
    echo "✓ built packages for {{ MAGENTA }}{{ version }}{{ NORMAL }}"

# Build the distribution image locally. Never pushes; needs packaging/npm/dist
# populated by `just build-packages` or a downloaded dist artifact. Defaults to
# the host arch; passing several needs a container-driver buildx builder.
[doc('Build the distribution image locally')]
[group('docker')]
docker-image version=`cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "runner-run") | .version'` platforms="":
    #!/usr/bin/env bash
    set -euo pipefail
    echo "→ preparing context for {{ MAGENTA }}{{ version }}{{ NORMAL }}"
    bash .github/scripts/docker.sh prepare
    echo "→ building {{ BLUE }}{{ platforms }}{{ NORMAL }}"
    TAGS="runner:{{ version }}" PLATFORMS="{{ platforms }}" PUSH=false \
        bash .github/scripts/docker.sh build
    echo "✓ built {{ MAGENTA }}runner:{{ version }}{{ NORMAL }}"

# Build release bin, pack the npm artifacts, and smoke-test them like CI.
[group('npm')]
test-release version=`cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "runner-run") | .version'` host-triple=`rustc --print host-tuple`:
    #!/usr/bin/env bash
    set -euo pipefail
    pkg="$(jq -r --arg t '{{ host-triple }}' '.targets[] | select(.rust == $t) | .pkg' {{ targets-json }})"
    if [[ -z "${pkg}" ]]; then
        echo "✗ no npm package mapped for host triple: {{ host-triple }}" >&2
        exit 1
    fi
    echo "→ host: {{ host-triple }} (${pkg})"

    cargo bbr
    mkdir -p {{ downloads-dir }}

    files=(runner run)
    if [[ "{{ os_family() }}" == "windows" ]]; then files=(runner.exe run.exe); fi
    for file in "${files[@]}"; do
        if [[ ! -f "target/release/${file}" ]]; then
            echo "✗ expected target/release/${file} to exist after build"
            exit 1
        fi
    done

    tar czf "{{ downloads-dir }}/runner-v{{ version }}-{{ host-triple }}.tar.gz" \
        -C target/release "${files[@]}"
    just build-packages "${pkg}" "true" "{{ version }}"

    # Same pack-install-execute smoke CI runs before npm publish.
    RELEASE_TAG="v{{ version }}" HOST_PKG="${pkg}" bash .github/scripts/npm.sh smoke
    echo "✓ smoke passed for {{ MAGENTA }}${pkg}{{ NORMAL }}"
