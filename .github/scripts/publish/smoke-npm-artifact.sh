#!/usr/bin/env bash

set -euo pipefail
shopt -s nullglob

# Install the packed tarballs into a scratch project and execute every
# bin — runs on the exact bytes `npm publish` ships.

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
EXPECTED_VERSION="${RELEASE_TAG#v}"
TARGETS_JSON="${GITHUB_WORKSPACE:-.}/npm/targets.json"
FACADE=$(jq -r '.facade' "${TARGETS_JSON}")
SCOPE=$(jq -r '.scope' "${TARGETS_JSON}")
HOST_PKG=linux-x64-gnu # ubuntu-latest; matches release.yml build-dist

scratch=$(mktemp -d)
trap 'rm -rf "${scratch}"' EXIT

(cd "npm/dist/${HOST_PKG}" && npm pack --pack-destination "${scratch}" >/dev/null)
(cd "npm/dist/${FACADE}" && npm pack --pack-destination "${scratch}" >/dev/null)

mkdir "${scratch}/app"
(cd "${scratch}/app" && npm install --no-audit --no-fund --ignore-scripts "${scratch}"/*.tgz)

assert_version() {
	local label="$1" out
	shift
	out=$("$@")
	if [[ "${out}" != *"${EXPECTED_VERSION}"* ]]; then
		echo "error: ${label}: expected ${EXPECTED_VERSION}, got: ${out}" >&2
		exit 1
	fi
	echo "ok ${label}: ${out}"
}

platform_dir="${scratch}/app/node_modules/${SCOPE}/${HOST_PKG}"

# Raw binaries — the files whose exec bits the artifact handoff used to drop.
raw_bins=("${platform_dir}/bin/"*)
if [[ "${#raw_bins[@]}" -eq 0 ]]; then
	echo "error: no binaries under ${platform_dir}/bin/" >&2
	exit 1
fi
for raw in "${raw_bins[@]}"; do
	assert_version "raw $(basename "${raw}")" "${raw}" --version
done

# Every bin target, whatever the bin field's shape.
while IFS= read -r target; do
	assert_version "bin ${target}" "${platform_dir}/${target}" --version
done < <(jq -r '.bin | if type == "string" then [.] else [.[]] end | .[]' "${platform_dir}/package.json")

# Linked bins (facade shims + platform bin).
linked=("${scratch}/app/node_modules/.bin/"*)
if [[ "${#linked[@]}" -eq 0 ]]; then
	echo "error: no bins linked in scratch install" >&2
	exit 1
fi
for bin in "${linked[@]}"; do
	assert_version "$(basename "${bin}")" "${bin}" --version
done
