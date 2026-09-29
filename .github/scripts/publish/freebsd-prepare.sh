#!/usr/bin/env bash
# Substitute the release version into the FreeBSD port's Makefile and
# regenerate its distinfo (per-arch SHA256 + SIZE) from the release's
# published assets — the `.sha256` companion gives the digest and the
# release asset metadata gives the byte size. This avoids running
# `make makesum` on a host that has no ports tree (and no need to download
# the multi-MB tarballs just to size them).
#
# Usage: freebsd-prepare.sh <version-without-leading-v>
# Requires: GH_TOKEN, GITHUB_REPOSITORY (provided by Actions).
set -euo pipefail

version="${1:?usage: freebsd-prepare.sh <version>}"
port_dir="freebsd/runner"
makefile="${port_dir}/Makefile"
distinfo="${port_dir}/distinfo"

# Reject anything that isn't strict semver (with optional prerelease) before
# touching files. The downstream `sed -i` substitution would otherwise be at
# the mercy of `&`, `/`, `\`, and newlines in whatever the workflow handed
# us. Keeping the alphabet to [0-9A-Za-z.-] guarantees a byte-for-byte
# literal substitution.
if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
	echo "error: version '${version}' does not match semver (X.Y.Z or X.Y.Z-prerelease)" >&2
	exit 1
fi

for f in "${makefile}" "${distinfo}"; do
	if [[ ! -f "${f}" ]]; then
		echo "error: ${f} not found" >&2
		exit 1
	fi
done

# Fresh upstream version → DISTVERSION bump. PORTREVISION (if any) is reset
# by simply not carrying one in the checked-in Makefile. The literal tab
# keeps the value-column alignment used throughout the Makefile.
tab="$(printf '\t')"
sed -i -E "s/^DISTVERSION=.*/DISTVERSION=${tab}${version}/" "${makefile}"

# CARCH -> Rust triple. Keep in lockstep with RUST_TARGET_* in the Makefile
# and the freebsd-* entries in npm/targets.json. amd64 is the tier-2
# release-blocking build and is mandatory here; aarch64 is the tier-3
# `experimental` build (continue-on-error in release.yml), so its asset may
# legitimately be missing for a given release — we skip it rather than abort,
# matching how the npm publish treats experimental packages as optional.
declare -A triples=(
	[amd64]='x86_64-unknown-freebsd'
	[aarch64]='aarch64-unknown-freebsd'
)
declare -A required=([amd64]=1 [aarch64]=0)

# distinfo entry for one arch, emitted to stdout. Returns non-zero (without
# printing) when the arch's assets are absent.
emit_arch() {
	local carch="$1" triple="$2" distfile sum size
	distfile="runner-v${version}-${triple}.tar.gz"

	# `.sha256` asset is "<hash>  <filename>"; field 1 is the digest.
	if ! sum="$(gh release download "v${version}" \
		--repo "${GITHUB_REPOSITORY}" \
		--pattern "runner-v${version}-${triple}.sha256" \
		--output - 2>/dev/null | awk 'NR==1{print $1}')" || [[ -z "${sum}" ]]; then
		return 1
	fi
	if [[ ! "${sum}" =~ ^[0-9a-f]{64}$ ]]; then
		echo "error: bad sha256 for ${distfile}: '${sum}'" >&2
		exit 1
	fi

	# Byte size from the release asset metadata (no tarball download).
	size="$(gh api "repos/${GITHUB_REPOSITORY}/releases/tags/v${version}" \
		--jq ".assets[] | select(.name == \"${distfile}\") | .size")"
	if [[ ! "${size}" =~ ^[0-9]+$ ]]; then
		echo "error: bad size for ${distfile}: '${size}'" >&2
		exit 1
	fi

	echo "SHA256 (${distfile}) = ${sum}"
	echo "SIZE (${distfile}) = ${size}"
}

# Regenerate distinfo from scratch so a dropped arch can never leave a stale
# entry behind. TIMESTAMP mirrors what `make makesum` would stamp.
{
	echo "TIMESTAMP = $(date +%s)"
	for carch in amd64 aarch64; do
		if ! emit_arch "${carch}" "${triples[$carch]}"; then
			if [[ "${required[$carch]}" == 1 ]]; then
				echo "error: required ${carch} freebsd asset missing for v${version}" >&2
				exit 1
			fi
			echo "note: skipping ${carch} — no freebsd asset for v${version}" >&2
		fi
	done
} >"${distinfo}.tmp"
mv "${distinfo}.tmp" "${distinfo}"

echo "--- prepared ${makefile} ---"
grep -E '^(PORTNAME|DISTVERSION|PKGNAMESUFFIX)=' "${makefile}"
echo "--- prepared ${distinfo} ---"
cat "${distinfo}"
