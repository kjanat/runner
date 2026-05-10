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

# Build the release binaries for the host triple, pack the matching npm
# sub-package via the pack-npm-platform composite action's bash logic, and
# verify the facade shims spawn the resulting native binary. Mirrors what
# .github/workflows/release.yml does for one cell — useful for catching
# packaging regressions locally before tagging.
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
    mkdir -p {{ downloads-dir }}/stage {{ downloads-dir }}/npm

    files=(runner run)
    if [[ "{{ os_family() }}" == "windows" ]]; then files=(runner.exe run.exe); fi
    for file in "${files[@]}"; do
        if [[ ! -f "target/release/${file}" ]]; then
            echo "✗ expected target/release/${file} to exist after build"
            exit 1
        fi
    done

    # Stage binaries flat (matches release archive layout)
    rm -rf {{ downloads-dir }}/stage/bin
    mkdir -p {{ downloads-dir }}/stage/bin
    for file in "${files[@]}"; do
        cp "target/release/${file}" "{{ downloads-dir }}/stage/bin/${file}"
    done

    tar czf "{{ downloads-dir }}/runner-v{{ version }}-{{ host-triple }}.tar.gz" \
        -C target/release "${files[@]}"

    # Build per-target package.json identical to what pack-npm-platform/action.yml emits.
    os="$(jq -c '.os'   <<<"${target_json}")"
    cpu="$(jq -c '.cpu' <<<"${target_json}")"
    libc_obj="{}"
    if jq -e '.libc' <<<"${target_json}" >/dev/null; then
        libc_obj="$(jq -c '{libc: .libc}' <<<"${target_json}")"
    fi

    subpkg_dir="{{ downloads-dir }}/npm/${pkg}"
    rm -rf "${subpkg_dir}"; mkdir -p "${subpkg_dir}/bin"
    cp "{{ downloads-dir }}/stage/bin/"* "${subpkg_dir}/bin/"

    jq -n \
        --arg   name        "${scope}/${pkg}" \
        --arg   version     "{{ version }}" \
        --argjson os        "${os}" \
        --argjson cpu       "${cpu}" \
        --argjson libc_obj  "${libc_obj}" \
        '{name: $name, version: $version, os: $os, cpu: $cpu, files: ["bin/"]} + $libc_obj' \
        > "${subpkg_dir}/package.json"

    # Stage facade with optionalDependencies pointing at our just-built sub-pkg.
    facade_dir="{{ downloads-dir }}/npm/${facade}"
    rm -rf "${facade_dir}"; cp -R npm/facade/. "${facade_dir}/"
    opt_deps="$(jq -n --arg k "${scope}/${pkg}" --arg v "{{ version }}" '{($k): $v}')"
    tmp="$(mktemp)"
    jq --arg v "{{ version }}" --argjson od "${opt_deps}" \
        '.version = $v | .optionalDependencies = $od' \
        "${facade_dir}/package.json" > "${tmp}"
    mv "${tmp}" "${facade_dir}/package.json"

    # Wire the sub-pkg under node_modules so resolve.cjs's require.resolve finds it.
    mkdir -p "${facade_dir}/node_modules/${scope}"
    ln -sfn "../../../${pkg}" "${facade_dir}/node_modules/${scope}/${pkg}"

    for bin in runner run; do
        output="$(node "${facade_dir}/bin/${bin}.cjs" --version)"
        echo "→ {{ BLUE }}${bin} --version{{ NORMAL }}	output: {{ GREEN }}${output}{{ NORMAL }}"
        if [[ "${output}" != *"{{ version }}"* ]]; then
            echo "✗ ${bin}.cjs did not output version {{ version }}"
            exit 1
        fi
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
