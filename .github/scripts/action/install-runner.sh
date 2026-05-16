#!/usr/bin/env bash
set -euo pipefail

TAG="${TAG:?}"
CBIN="${CBIN:?}"
DEST="${DEST:?}"

echo "::group::runner-run | cargo-binstall runner-run@${TAG#v}"
trap 'echo "::endgroup::"' EXIT

exec "${CBIN}" "runner-run@${TAG#v}" --install-path "${DEST}"
