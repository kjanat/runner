#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
GITHUB_REPOSITORY="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"

# Scrub before fetch: stale .tar.gz/.sha256 from a previous tag would pass
# verify-checksum.sh (which walks every file in this dir) but be wrong-version
# for the current RELEASE_TAG. Hosted GHA runners get fresh workspaces, but
# self-hosted runners and local invocations don't.
rm -rf npm/downloads
mkdir -p npm/downloads
gh release download "${RELEASE_TAG}" \
	--repo "${GITHUB_REPOSITORY}" \
	--pattern 'runner-*-*.tar.gz' \
	--pattern 'runner-*-*.sha256' \
	--dir npm/downloads
ls -la npm/downloads
