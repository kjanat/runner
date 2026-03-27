#!/usr/bin/env bash
set -euo pipefail

REPO="kjanat/runner"
INSTALL_DIR="${RUNNER_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME:?HOME is required}/.local/bin}}"

usage() {
	cat <<'EOF'
Install runner binaries from GitHub Releases.

Usage:
  install.sh [vX.Y.Z]

Arguments:
  vX.Y.Z  Optional release tag. If omitted, installs latest release.

Environment:
  RUNNER_VERSION      Release tag override (e.g. v0.1.0)
  RUNNER_INSTALL_DIR  Destination directory (highest precedence)
  XDG_BIN_HOME        Destination directory fallback before ~/.local/bin
EOF
}

require_command() {
	local cmd="$1"
	if ! command -v "${cmd}" >/dev/null 2>&1; then
		printf 'error: required command not found: %s\n' "${cmd}" >&2
		exit 1
	fi
}

resolve_latest_version() {
	local latest_url version

	latest_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
	version="${latest_url##*/}"
	version="${version%%\?*}"

	if [[ -z "${version}" || "${version}" == "latest" ]]; then
		printf 'error: failed to resolve latest release version\n' >&2
		exit 1
	fi

	printf '%s\n' "${version}"
}

resolve_target() {
	case "$(uname -m)" in
	x86_64) printf 'x86_64-unknown-linux-musl\n' ;;
	aarch64 | arm64) printf 'aarch64-unknown-linux-musl\n' ;;
	*)
		printf 'error: unsupported architecture: %s\n' "$(uname -m)" >&2
		exit 1
		;;
	esac
}

main() {
	if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
		usage
		exit 0
	fi

	if [[ "$#" -gt 1 ]]; then
		usage >&2
		exit 1
	fi

	if [[ "$(uname -s)" != "Linux" ]]; then
		printf 'error: install.sh currently supports Linux only\n' >&2
		exit 1
	fi

	require_command curl
	require_command tar
	require_command sha256sum
	require_command install

	local version="${RUNNER_VERSION:-${1:-}}"
	if [[ -z "${version}" ]]; then
		version="$(resolve_latest_version)"
	fi
	if [[ "${version}" != v* ]]; then
		version="v${version}"
	fi

	local target
	target="$(resolve_target)"

	local asset="runner-${version}-${target}.tar.gz"
	local base_url="https://github.com/${REPO}/releases/download/${version}"

	local tmp_dir
	tmp_dir="$(mktemp -d)"
	trap 'rm -rf "${tmp_dir}"' EXIT

	printf '→ downloading %s\n' "${asset}"
	curl -fL --retry 3 --retry-delay 1 -o "${tmp_dir}/${asset}" "${base_url}/${asset}"
	curl -fL --retry 3 --retry-delay 1 -o "${tmp_dir}/${asset}.sha256" "${base_url}/${asset}.sha256"

	(
		cd "${tmp_dir}"
		sha256sum -c "${asset}.sha256"
	)

	tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"

	for bin in runner run; do
		if [[ ! -f "${tmp_dir}/${bin}" ]]; then
			printf 'error: missing binary in archive: %s\n' "${bin}" >&2
			exit 1
		fi
	done

	mkdir -p "${INSTALL_DIR}"
	install -m 0755 "${tmp_dir}/runner" "${tmp_dir}/run" "${INSTALL_DIR}/"

	printf '✓ installed runner + run to %s\n' "${INSTALL_DIR}"
	printf '  ensure %s is in your PATH\n' "${INSTALL_DIR}"
}

main "$@"
