#!/usr/bin/env bash

set -euo pipefail
echo "::group::runner-run | install cargo-binstall"
trap 'echo "::endgroup::"' EXIT

CARGO_HOME="${CARGO_HOME:?}"
BINSTALL_SHA="${BINSTALL_SHA:?}"
BINSTALL_REPO="cargo-bins/cargo-binstall"
BINSTALL_SCRIPT="install-from-binstall-release.sh"

# Pre-add CARGO_HOME/bin to this shell's PATH so the installer
# script sees it already present and does NOT write $GITHUB_PATH
# (we never put cargo-binstall on the consumer's PATH; it is
# invoked by absolute path in the next step).
export PATH="${CARGO_HOME}/bin:${PATH}"

curl -L --proto '=https' --tlsv1.2 -sSf \
	"https://raw.githubusercontent.com/${BINSTALL_REPO}/${BINSTALL_SHA}/${BINSTALL_SCRIPT}" \
	| bash
