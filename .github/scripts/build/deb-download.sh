#!/usr/bin/env bash
#
# Download the prebuilt linux-gnu release tarballs (+ their .sha256 companions)
# that the Debian .debs are repackaged from, for the three arches we ship.
# Mirrors download-release-archives.sh but fetches only the gnu triples Debian
# needs instead of the whole release.
set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
GITHUB_REPOSITORY="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"
DEB_DOWNLOAD_DIR="${DEB_DOWNLOAD_DIR:-debian/.work/download}"

version="${RELEASE_TAG#v}"
triples=(
	x86_64-unknown-linux-gnu
	aarch64-unknown-linux-gnu
	armv7-unknown-linux-gnueabihf
)

# Scrub before fetch: a stale tarball from a previous tag would pass the
# checksum walk in deb-build.sh yet be the wrong version.
rm -rf "${DEB_DOWNLOAD_DIR}"
mkdir -p "${DEB_DOWNLOAD_DIR}"
for triple in "${triples[@]}"; do
	gh release download "${RELEASE_TAG}" \
		--repo "${GITHUB_REPOSITORY}" \
		--pattern "runner-v${version}-${triple}.tar.gz" \
		--pattern "runner-v${version}-${triple}.sha256" \
		--dir "${DEB_DOWNLOAD_DIR}"
done
ls -la "${DEB_DOWNLOAD_DIR}"
