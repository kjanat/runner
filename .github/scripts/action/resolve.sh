#!/usr/bin/env bash
set -euo pipefail

CURRENT_SCRIPT="$(realpath "${BASH_SOURCE[0]}")"
DIR="$(dirname "${CURRENT_SCRIPT}")"

echo "::group::runner-run | resolve version + target"
trap 'echo "::endgroup::"' EXIT

# env
ACTION_REF="${ACTION_REF:-}"
GITHUB_ACTION_PATH="${GITHUB_ACTION_PATH:-$(realpath "${DIR}/../../..")}"
GITHUB_OUTPUT="${GITHUB_OUTPUT:?}"
INPUT_TARGET="${INPUT_TARGET:-}"
INPUT_VERSION="${INPUT_VERSION:-}"
RUNNER_ARCH="${RUNNER_ARCH:-}"
RUNNER_OS="${RUNNER_OS:-}"
RUNNER_TEMP="${RUNNER_TEMP:?}"
RUNNER_TOOL_CACHE="${RUNNER_TOOL_CACHE:?}"

# vars (dest needs tag+triple — computed after resolution, below)
targets="${GITHUB_ACTION_PATH}/npm/targets.json"
cbhome="${RUNNER_TEMP}/cargo-binstall" # our controlled install root
[[ "${RUNNER_OS}" == "Windows" ]] && ext=".exe" || ext=""

# --- version: resolved by us, not scraped from binstall ---
req="${INPUT_VERSION:-${ACTION_REF}}"
if [[ "${req}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+ ]]; then
	tag="v${req#v}"
else
	# latest/master/empty: follow the /releases/latest redirect.
	# No JSON, no token, no pipe (the curl-23 SIGPIPE bug class).
	url="$(
		curl -fsSLS -o /dev/null -w '%{url_effective}' \
			--retry 5 --retry-all-errors --retry-delay 1 \
			--connect-timeout 3 --max-time 8 \
			https://github.com/kjanat/runner/releases/latest
	)"
	tag="${url##*/}"
	tag="${tag%%\?*}"
	if [[ ! "${tag}" =~ ^v[0-9] ]]; then
		echo "::error::failed to resolve latest runner-run release (got '${tag}')"
		exit 1
	fi
fi

# --- target: input override, else resolved from npm/targets.json
# (the single source of truth shared with the release matrix and
# npm packaging — no hand-maintained map to drift). ---
triple="${INPUT_TARGET}"
if [[ -z "${triple}" ]]; then
	case "${RUNNER_OS}" in
		Linux) node_os=linux ;;
		macOS) node_os=darwin ;;
		Windows) node_os=win32 ;;
		*) node_os="" ;;
	esac

	case "${RUNNER_ARCH}" in
		X64) node_cpu=x64 ;;
		ARM64) node_cpu=arm64 ;;
		ARM) node_cpu=arm ;;
		X86) node_cpu=ia32 ;;
		*) node_cpu="" ;;
	esac

	triple="$(
		jq -r --arg os "${node_os}" --arg cpu "${node_cpu}" '
[ .targets[]
  | select(.os|index($os)) | select(.cpu|index($cpu))
  | select((.libc == null) or (.libc|index("glibc")))
  | .rust ] | first // empty' "${targets}"
	)"
	if [[ -z "${triple}" ]]; then
		echo "::error::No prebuilt target in npm/targets.json for ${RUNNER_OS}-${RUNNER_ARCH}; pass an explicit \`target\` input."
		exit 1
	fi
fi

dest="${RUNNER_TOOL_CACHE}/runner-run/${tag}/${triple}"

cat <<-EOF | tee -a "${GITHUB_OUTPUT}"
	tag=${tag}
	triple=${triple}
	dest=${dest}
	cbhome=${cbhome}
	ext=${ext}
EOF
