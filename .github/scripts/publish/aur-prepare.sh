#!/usr/bin/env bash
# Substitute the release version into an AUR PKGBUILD and, for the prebuilt
# (-bin) package, inject per-arch sha256 sums read from the release's
# published `.sha256` companion assets.
#
# Why not `updpkgsums`? On an x86_64 runner it only fetches sources matching
# the host $CARCH, leaving sha256sums_aarch64 / _armv7h untouched. Reading the
# already-published checksum assets covers every arch without downloading the
# (multi-MB) tarballs. The source package has a single arch-independent source,
# so it lets the deploy action run `updpkgsums` instead (see the workflow).
#
# Usage: aur-prepare.sh <pkgname> <version-without-leading-v>
# Requires: GH_TOKEN, GITHUB_REPOSITORY (provided by Actions).
set -euo pipefail

pkgname="${1:?usage: aur-prepare.sh <pkgname> <version>}"
version="${2:?usage: aur-prepare.sh <pkgname> <version>}"
pkgbuild="aur/${pkgname}/PKGBUILD"

# Reject anything that isn't strict semver (with optional prerelease) before
# touching files. The downstream `sed -i ".../pkgver=${version}/"` would
# otherwise be at the mercy of `&` (sed backreference), `/` (delimiter),
# `\`, and newlines in whatever the workflow handed us. Keeping the alphabet
# to [0-9A-Za-z.-] guarantees the substitution is byte-for-byte literal.
if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
	echo "error: version '${version}' does not match semver (X.Y.Z or X.Y.Z-prerelease)" >&2
	exit 1
fi

if [[ ! -f "${pkgbuild}" ]]; then
	echo "error: ${pkgbuild} not found" >&2
	exit 1
fi

# Fresh upstream version → pkgver bump, pkgrel back to 1.
sed -i -E "s/^pkgver=.*/pkgver=${version}/" "${pkgbuild}"
sed -i -E "s/^pkgrel=.*/pkgrel=1/" "${pkgbuild}"

if [[ "${pkgname}" == 'runner-run-bin' ]]; then
	# Arch CARCH -> Rust triple. Keep in lockstep with the source_<arch>
	# arrays in aur/runner-run-bin/PKGBUILD.
	declare -A triples=(
		[x86_64]='x86_64-unknown-linux-gnu'
		[aarch64]='aarch64-unknown-linux-gnu'
		[armv7h]='armv7-unknown-linux-gnueabihf'
	)
	for carch in "${!triples[@]}"; do
		asset="runner-v${version}-${triples[$carch]}.sha256"
		# `.sha256` asset is "<hash>  <filename>"; field 1 is the digest.
		sum="$(gh release download "v${version}" \
			--repo "${GITHUB_REPOSITORY}" \
			--pattern "${asset}" --output - | awk 'NR==1{print $1}')"
		if [[ ! "${sum}" =~ ^[0-9a-f]{64}$ ]]; then
			echo "error: bad sha256 for ${asset}: '${sum}'" >&2
			exit 1
		fi
		sed -i -E "s/^sha256sums_${carch}=\(.*/sha256sums_${carch}=('${sum}')/" "${pkgbuild}"
	done
fi

echo "--- prepared ${pkgbuild} ---"
cat "${pkgbuild}"
