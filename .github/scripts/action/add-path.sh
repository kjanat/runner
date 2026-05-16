#!/usr/bin/env bash
set -euo pipefail

GITHUB_PATH="${GITHUB_PATH:?}"
DEST="${DEST:?}"

echo "::group::runner-run | add to PATH"
trap 'echo "::endgroup::"' EXIT

tee -a "${GITHUB_PATH}" <<<"${DEST}"
