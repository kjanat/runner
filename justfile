# https://just.systems
set unstable

cargo-version := `cargo read-manifest | jq -r .version`
triple := `rustc --print host-tuple`
downloads-dir := "npm" / "downloads"

[arg('bin', pattern='run|runner')]
[arg('profile', pattern='dev|release|')]
[group('bins')]
default bin=env("BIN", "runner") profile="dev" *args:
    env PROFILE={{ profile }} just {{ bin }} {{ args }}

install:
    cargo i

[group('bins')]
run *args:
    cargo bin-run --profile={{ env("PROFILE", "dev") }} -- {{ args }}

[group('bins')]
runner *args:
    cargo bin-runner --profile={{ env("PROFILE", "dev") }} -- {{ args }}

ls:
    @just --list

# Build the release binaries for the host triple, then drive the same pack.sh
# scripts the CI composite actions use, and verify the facade shims spawn the
# resulting native binary. This is the local-fidelity end-to-end test for
# packaging — exercises the exact code paths .github/workflows/release.yml does
# for one cell, so packaging regressions surface here before tagging.
[group('npm')]
test-release version=cargo-version host-triple=triple:
    #!/usr/bin/env bash
    set -euo pipefail
    target_json="$(jq --arg t '{{ host-triple }}' '.targets[] | select(.rust == $t)' npm/targets.json)"
    pkg="$(jq -r '.pkg' <<<"${target_json}")"
    scope="$(jq -r '.scope' npm/targets.json)"
    facade="$(jq -r '.facade' npm/targets.json)"
    if [[ -z "${pkg}" || "${pkg}" == "null" ]]; then
        echo "✗ no npm package mapped for host triple: {{ host-triple }}" >&2
        exit 1
    fi
    echo "→ host: {{ host-triple }} (${pkg})"

    cargo bbr
    rm -rf {{ downloads-dir }}; mkdir -p {{ downloads-dir }}/sub {{ downloads-dir }}/facade

    # `pack.sh` reads its inputs from $GITHUB_OUTPUT — emulate by writing to a tempfile.
    out="$(mktemp)"
    export GITHUB_OUTPUT="${out}"
    export GITHUB_WORKSPACE="${PWD}"

    # Step 1: build-target/pack.sh — stages binaries, makes tar.gz + sha256.
    # We already built; map target/release into where pack.sh expects target/<triple>/release.
    mkdir -p "target/{{ host-triple }}/release"
    files=(runner run); [[ "{{ os_family() }}" == "windows" ]] && files=(runner.exe run.exe)
    for f in "${files[@]}"; do cp -f "target/release/${f}" "target/{{ host-triple }}/release/${f}"; done
    : > "${out}"
    TRIPLE='{{ host-triple }}' VERSION='{{ version }}' BINARIES='runner run' \
        bash .github/actions/build-target/pack.sh
    bin_dir="$(grep '^bin-dir=' "${out}" | cut -d= -f2-)"

    # Step 2: pack-npm-platform/pack.sh — stamp + npm pack.
    : > "${out}"
    PKG="${pkg}" SCOPE="${scope}" FACADE="${facade}" VERSION='{{ version }}' \
        OS="$(jq -c '.os' <<<"${target_json}")" \
        CPU="$(jq -c '.cpu' <<<"${target_json}")" \
        LIBC="$(jq -c 'if has("libc") then .libc else null end' <<<"${target_json}" | sed 's/^null$//')" \
        BIN_DIR="${bin_dir}" BINARIES='runner run' \
        OUT_DIR="${PWD}/{{ downloads-dir }}/sub" \
        bash .github/actions/pack-npm-platform/pack.sh

    # Step 3: pack-npm-facade/pack.sh — generate facade with optionalDependencies.
    : > "${out}"
    FACADE_DIR=npm/facade TARGETS_JSON=npm/targets.json VERSION='{{ version }}' \
        OUT_DIR="${PWD}/{{ downloads-dir }}/facade" \
        bash .github/actions/pack-npm-facade/pack.sh

    # Step 4: unpack the facade tgz, link the sub-package under node_modules so
    # resolve.cjs's require.resolve walks up and finds it (npm does this in real installs).
    facade_tgz="$(ls {{ downloads-dir }}/facade/*.tgz | head -1)"
    sub_tgz="$(ls {{ downloads-dir }}/sub/*.tgz | head -1)"
    facade_unpack="{{ downloads-dir }}/facade-unpack"
    sub_unpack="{{ downloads-dir }}/sub-unpack/${pkg}"
    rm -rf "${facade_unpack}" "$(dirname "${sub_unpack}")"
    mkdir -p "${facade_unpack}" "${sub_unpack}"
    tar -xzf "${facade_tgz}"  -C "${facade_unpack}" --strip-components=1
    tar -xzf "${sub_tgz}"     -C "${sub_unpack}"    --strip-components=1
    mkdir -p "${facade_unpack}/node_modules/${scope}"
    ln -sfn "../../../sub-unpack/${pkg}" "${facade_unpack}/node_modules/${scope}/${pkg}"

    for bin in runner run; do
        output="$(node "${facade_unpack}/bin/${bin}.cjs" --version)"
        echo "→ {{ BLUE }}${bin} --version{{ NORMAL }}	output: {{ GREEN }}${output}{{ NORMAL }}"
        [[ "${output}" == *"{{ version }}"* ]] || { echo "✗ ${bin}.cjs did not output version {{ version }}"; exit 1; }
    done
    echo "✓ facade resolved native bin for {{ MAGENTA }}${pkg}{{ NORMAL }}"

# Bump version across Cargo.toml, pyproject.toml, and npm/facade/package.json.
# Use this before tagging a release so the workflow's verify-versions step passes.
bump version:
    #!/usr/bin/env bash
    set -euo pipefail
    sed -i.bak -E 's/^version *= *"[^"]+"/version = "{{ version }}"/' Cargo.toml pyproject.toml
    rm -f Cargo.toml.bak pyproject.toml.bak
    tmp="$(mktemp)"
    jq --arg v "{{ version }}" '.version = $v' npm/facade/package.json > "${tmp}"
    mv "${tmp}" npm/facade/package.json
    echo "✓ bumped to {{ MAGENTA }}{{ version }}{{ NORMAL }}"
