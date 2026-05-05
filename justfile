# https://just.systems
set unstable

cargo-version := `cargo read-manifest | jq -r .version`
triple := `rustc --print host-tuple`
npm-pkg-name := `cargo read-manifest | jq -r .metadata.npm.name`
npm-pkg-scope := `cargo read-manifest | jq -r .metadata.npm.subpkgscope`
build-pkgscript := "npm" / "scripts" / "build-packages.ts"
cargobuildboth := "cargo build --bin runner --bin run"
dowloads-dir := "npm" / "downloads"

[arg('bin', pattern='run|runner')]
[arg('profile', pattern='dev|release|')]
[group('bins')]
default bin=env("BIN", "runner") profile="dev" *args:
    just shim {{ bin }} {{ profile }} {{ args }}

install:
    cargo i

[group('bins')]
run *args:
    cargo bin-run --profile={{ env("PROFILE", "release") }} -- {{ args }}

[group('bins')]
runner *args:
    cargo bin-runner --profile={{ env("PROFILE", "release") }} -- {{ args }}

[arg('bin', pattern='run|runner|')]
[arg('profile', pattern='dev|release|')]
[private]
shim bin="runner" profile="release" *args:
    env PROFILE={{ profile }} just {{ bin }} {{ args }}

list:
    @just --list

[group('npm')]
build-packages only="" skip="false" version=cargo-version:
    #!/usr/bin/env bash
    set -euo pipefail
    ONLY="{{ only }}"
    SKIP="{{ skip }}"
    VERSION="{{ version }}"
    SCRIPT={{ build-pkgscript }}
    args=("--version" "${VERSION}")

    if [[ -n "${ONLY}" ]]; then args+=("--only=${ONLY}"); fi
    if [[ "${SKIP}" == "true" || "${SKIP}" == "1" ]]; then args+=("--skip-missing"); fi
    echo "→ building packages with args: {{ BLUE }}${args[*]}{{ NORMAL }}"
    node "${SCRIPT}" "${args[@]}"
    echo "✓ built packages for {{ MAGENTA }}${VERSION}{{ NORMAL }}"

# Build release bin and verify the facade shims spawn the native binary.
[group('npm')]
test-release version=cargo-version host-triple=triple:
    #!/usr/bin/env bash
    set -euo pipefail
    HOST_TARGET="{{ host-triple }}"
    VERSION="{{ version }}"
    pkg="$(node -p "
    require('./npm/targets.json')
      .targets.find(
        t=>t.rust === '${HOST_TARGET}'
      ).pkg
    ")"
    echo "→ host: ${HOST_TARGET} (${pkg})"

    # Build the release bins so we have them to test against.
    {{ cargobuildboth }} --release
    mkdir -p {{ dowloads-dir }}

    if [[ "{{ os_family() }}" == "windows" ]]; then
    	files=("runner.exe" "run.exe")
    else
    	files=("runner" "run")
    fi
    for file in "${files[@]}"; do
        if [[ ! -f "target/release/${file}" ]]; then
            echo "✗ expected target/release/${file} to exist after build"
            exit 1
        fi
    done

    tar czf "{{ dowloads-dir }}/runner-v${VERSION}-${HOST_TARGET}.tar.gz" \
        -C target/release "${files[@]}"
    just build-packages "${pkg}" "true" "${VERSION}"
    # Wire up the optional sub-package so resolve.cjs's `require.resolve`
    # walks node_modules and finds it. In a real install, npm does this.
    subpkgdir="npm/dist/{{ npm-pkg-name }}/node_modules/{{ npm-pkg-scope }}"
    mkdir -p "${subpkgdir}"
    ln -sfn "../../../${pkg}" "${subpkgdir}/${pkg}"
    commands=(
        "node npm/dist/{{ npm-pkg-name }}/bin/runner.cjs --version"
        "node npm/dist/{{ npm-pkg-name }}/bin/run.cjs --version"
    )
    for cmd in "${commands[@]}"; do
    	output="$(eval "${cmd}")"
        echo "→ {{ BLUE }}${cmd}{{ NORMAL }}	output: {{ GREEN }}${output}{{ NORMAL }}"
        if [[ "${output}" != *"${VERSION}"* ]]; then
            echo "✗ ${cmd} did not output version ${VERSION}"
            exit 1
        fi
    done
    echo "✓ facade resolved native bin for {{ MAGENTA }}${pkg}{{ NORMAL }}"
