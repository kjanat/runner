# https://just.systems
set unstable

cargo-version := `cargo read-manifest | jq -r .version`
triple := `rustc --print host-tuple`
npm-pkg-name := `cargo metadata --format-version 1 --no-deps | jq -r --arg id "$(cargo metadata --format-version 1 --no-deps | jq -r '.workspace_default_members[0]')" '.packages[] | select(.id == $id).metadata.npm.name'`
npm-pkg-scope := `cargo metadata --format-version 1 --no-deps | jq -r --arg id "$(cargo metadata --format-version 1 --no-deps | jq -r '.workspace_default_members[0]')" '.packages[] | select(.id == $id).metadata.npm.subpkgscope'`
build-pkgscript := "npm" / "scripts" / "build-packages.ts"
downloads-dir := "npm" / "downloads"

schema-dir := "schemas"

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

# Regenerate the committed JSON Schemas.
# Drift guard: just gen-schema && git diff --exit-code schemas/
[group('schema')]
gen-schema:
    @echo "→ regenerating {{ BLUE }}{{ schema-dir }}{{ NORMAL }}"
    @cargo schema --all --output {{ schema-dir }}

[group('npm')]
build-packages only="" skip="false" version=cargo-version:
    #!/usr/bin/env bash
    set -euo pipefail
    args=("--version" "{{ version }}")
    if [[ -n "{{ only }}" ]]; then args+=("--only={{ only }}"); fi
    if [[ "{{ skip }}" == "true" || "{{ skip }}" == "1" ]]; then args+=("--skip-missing"); fi
    echo "→ building packages with args: {{ BLUE }}${args[*]}{{ NORMAL }}"
    node {{ build-pkgscript }} "${args[@]}"
    echo "✓ built packages for {{ MAGENTA }}{{ version }}{{ NORMAL }}"

# Build release bin and verify the facade shims spawn the native binary.
[group('npm')]
test-release version=cargo-version host-triple=triple:
    #!/usr/bin/env bash
    set -euo pipefail
    pkg="$(jq -r --arg t '{{ host-triple }}' '.targets[] | select(.rust == $t) | .pkg' npm/targets.json)"
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

    # Wire optional sub-package so resolve.cjs's require.resolve walks
    # node_modules and finds it. In a real install, npm does this.
    subpkgdir="npm/dist/{{ npm-pkg-name }}/node_modules/{{ npm-pkg-scope }}"
    mkdir -p "${subpkgdir}"
    ln -sfn "../../../${pkg}" "${subpkgdir}/${pkg}"

    for bin in runner run; do
        output="$(node "npm/dist/{{ npm-pkg-name }}/bin/${bin}.cjs" --version)"
        echo "→ {{ BLUE }}${bin} --version{{ NORMAL }}	output: {{ GREEN }}${output}{{ NORMAL }}"
        if [[ "${output}" != *"{{ version }}"* ]]; then
            echo "✗ ${bin}.cjs did not output version {{ version }}"
            exit 1
        fi
    done
    echo "✓ facade resolved native bin for {{ MAGENTA }}${pkg}{{ NORMAL }}"
