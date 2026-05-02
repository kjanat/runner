#!/usr/bin/env bash

set -euo pipefail
shopt -s nullglob

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"

tarballs=(*.tar.gz)
sums=(*.sha256)

# Refuse to proceed unless every tarball has a matching .sha256 and
# every .sha256 has a matching tarball — otherwise an unchecksummed
# binary could slip through to publish.
if [[ "${#tarballs[@]}" -eq 0 ]]; then
	echo "error: no tarballs downloaded for ${RELEASE_TAG}" >&2
	exit 1
fi
for t in "${tarballs[@]}"; do
	expected="${t%.tar.gz}.sha256"
	if [[ ! -f "${expected}" ]]; then
		echo "error: tarball ${t} has no matching ${expected}" >&2
		exit 1
	fi
done

# Verify each .sha256 references a tarball whose name matches its
# own basename — defends against a release where foo.sha256 was
# swapped to reference bar.tar.gz, which would leave foo.tar.gz
# unchecked while sha256sum -c silently re-verifies bar.
for s in "${sums[@]}"; do
	inner=$(awk '{sub(/^\*/, "", $2); print $2}' "${s}")
	expected="${s%.sha256}.tar.gz"
	if [[ ! -f "${expected}" ]]; then
		echo "error: checksum file ${s} has no matching ${expected}" >&2
		exit 1
	fi
	if [[ "${inner}" != "${expected}" ]]; then
		echo "error: ${s} references '${inner}', expected '${expected}'" >&2
		exit 1
	fi
done

for sum in "${sums[@]}"; do
	sha256sum -c --status "${sum}"
done
