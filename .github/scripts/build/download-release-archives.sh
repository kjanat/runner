#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
GITHUB_REPOSITORY="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"

mkdir -p npm/downloads
gh release download "${RELEASE_TAG}" \
	--repo "${GITHUB_REPOSITORY}" \
	--pattern 'runner-*-*.tar.gz' \
	--pattern 'runner-*-*.sha256' \
	--dir npm/downloads
ls -la npm/downloads
