#!/bin/sh
set -eu

REPO="kjanat/runner"

usage() {
	cat <<'EOF'
Install runner binaries from GitHub Releases.

Usage:
  install.sh [X.Y.Z|vX.Y.Z]

Arguments:
  X.Y.Z|vX.Y.Z  Optional release tag. If omitted, installs latest release.

Environment:
  RUNNER_VERSION      Release tag override (e.g. 0.1.0 or v0.1.0)
  RUNNER_INSTALL_DIR  Destination directory (highest precedence)
  XDG_BIN_HOME        Destination directory (overrides auto-detection)

Without an override, reuses the directory of an existing runner install
of ours (verified by its version banner; upgrade in place). Otherwise
installs to ~/bin or ~/.local/bin, preferring a directory that is already
on PATH (and, among those, one that already exists). Falls back to
~/.local/bin.
EOF
}

print_step() {
	printf '==> %s\n' "$1"
}

print_item() {
	printf '  - %s\n' "$1"
}

require_command() {
	cmd="$1"
	if ! command -v "${cmd}" >/dev/null 2>&1; then
		printf 'error: required command not found: %s\n' "${cmd}" >&2
		exit 1
	fi
}

resolve_latest_version() {
	latest_url="$(curl -fsSLS -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
	version="${latest_url##*/}"
	version="${version%%\?*}"

	if [ -z "${version}" ] || [ "${version}" = "latest" ]; then
		printf 'error: failed to resolve latest release version\n' >&2
		exit 1
	fi

	printf '%s\n' "${version}"
}

resolve_target() {
	arch="$(uname -m)"

	case "${arch}" in
		x86_64) printf 'x86_64-unknown-linux-musl\n' ;;
		aarch64 | arm64) printf 'aarch64-unknown-linux-musl\n' ;;
		*)
			printf 'error: unsupported architecture: %s\n' "${arch}" >&2
			exit 1
			;;
	esac
}

# These predicates print "yes"/"no" rather than returning an exit status:
# callers invoke them via command substitution and test the printed string.
# That keeps them composable under `set -e` without ShellCheck SC2310 — a
# function used directly as a condition silently disables set -e inside it.
dir_on_path() {
	case ":${PATH:-}:" in
		*:"$1":*) printf 'yes\n' ;;
		*) printf 'no\n' ;;
	esac
}

# Prints "yes" only when the given path is one of OUR binaries, identified by
# its "<name> <semver>" version banner (e.g. "runner 0.12.2"). Guards against
# unrelated system tools that merely happen to be named runner or run.
is_our_runner() {
	if [ ! -x "$1" ]; then
		printf 'no\n'
		return
	fi
	out="$("$1" -V 2>/dev/null || true)"
	case "${out}" in
		"runner "[0-9]*.[0-9]*.[0-9]* | "run "[0-9]*.[0-9]*.[0-9]*) printf 'yes\n' ;;
		*) printf 'no\n' ;;
	esac
}

# Choose where to install. Explicit overrides win; otherwise, if a runner of
# OURS is already on PATH (verified by its version banner, not just its name),
# reuse its directory (upgrade in place); failing that, pick between ~/bin and
# ~/.local/bin, preferring a directory on PATH, and among those one that
# already exists (~/bin breaks ties). Default: ~/.local/bin.
resolve_install_dir() {
	if [ -n "${RUNNER_INSTALL_DIR:-}" ]; then
		printf '%s\n' "${RUNNER_INSTALL_DIR}"
		return
	fi
	if [ -n "${XDG_BIN_HOME:-}" ]; then
		printf '%s\n' "${XDG_BIN_HOME}"
		return
	fi

	# Upgrade in place: reuse the directory of an existing runner install, but
	# only when it is verifiably ours (anchored on `runner`, which we always
	# co-install with `run`). A system binary named runner/run is left alone.
	existing="$(command -v runner 2>/dev/null || true)"
	case "${existing}" in
		*/*)
			existing_is_ours="$(is_our_runner "${existing}")"
			if [ "${existing_is_ours}" = yes ]; then
				printf '%s\n' "${existing%/*}"
				return
			fi
			;;
		*) ;;
	esac

	home="${HOME:?HOME is required}"
	bin="${home}/bin"
	local_bin="${home}/.local/bin"
	bin_on_path="$(dir_on_path "${bin}")"
	local_bin_on_path="$(dir_on_path "${local_bin}")"

	if [ "${bin_on_path}" = yes ] && [ -d "${bin}" ]; then
		printf '%s\n' "${bin}"
	elif [ "${local_bin_on_path}" = yes ] && [ -d "${local_bin}" ]; then
		printf '%s\n' "${local_bin}"
	elif [ "${bin_on_path}" = yes ]; then
		printf '%s\n' "${bin}"
	elif [ "${local_bin_on_path}" = yes ]; then
		printf '%s\n' "${local_bin}"
	else
		printf '%s\n' "${local_bin}"
	fi
}

main() {
	if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
		usage
		exit 0
	fi

	if [ "$#" -gt 1 ]; then
		usage >&2
		exit 1
	fi

	os_name="$(uname -s)"

	if [ "${os_name}" != "Linux" ]; then
		printf 'error: install.sh currently supports Linux only\n' >&2
		exit 1
	fi

	require_command curl
	require_command tar
	require_command sha256sum
	require_command install

	INSTALL_DIR="$(resolve_install_dir)"

	version="${RUNNER_VERSION:-${1:-}}"
	if [ -z "${version}" ]; then
		version="$(resolve_latest_version)"
	fi
	case "${version}" in
		v*) ;;
		*) version="v${version}" ;;
	esac

	target="$(resolve_target)"

	asset="runner-${version}-${target}.tar.gz"
	checksum_asset="runner-${version}-${target}.sha256"
	base_url="https://github.com/${REPO}/releases/download/${version}"

	tmp_dir="$(mktemp -d)"
	trap '[ -n "${tmp_dir:-}" ] && rm -rf "${tmp_dir}"' EXIT

	print_step "Downloading release assets"
	print_item "archive: ${asset}"
	curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${asset}" "${base_url}/${asset}"
	curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${checksum_asset}" "${base_url}/${checksum_asset}"

	(
		cd "${tmp_dir}"
		sha256sum -c --status "${checksum_asset}"
	)

	tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"

	for bin in runner run; do
		if [ ! -f "${tmp_dir}/${bin}" ]; then
			printf 'error: missing binary in archive: %s\n' "${bin}" >&2
			exit 1
		fi
	done

	mkdir -p "${INSTALL_DIR}"
	install -m 0755 "${tmp_dir}/runner" "${tmp_dir}/run" "${INSTALL_DIR}/"

	print_step "Installation complete"
	print_item "location: ${INSTALL_DIR}"

	expected_runner="${INSTALL_DIR}/runner"
	resolved_runner="$(command -v runner || true)"

	if installed_version="$("${expected_runner}" -V)"; then
		print_item "version: ${installed_version}"
	else
		print_item "warning: failed to execute ${expected_runner} -V"
	fi

	# Man pages from the release archive, into the XDG user man path. Verified like the binaries above.
	# Best-effort: a read-only $HOME, a missing asset, or a checksum mismatch must not fail the install.
	man_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/man/man1"
	man_asset="runner-${version}-man.tar.gz"
	man_checksum="runner-${version}-man.sha256"
	if curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${man_asset}" "${base_url}/${man_asset}" 2>/dev/null \
		&& curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${man_checksum}" "${base_url}/${man_checksum}" 2>/dev/null \
		&& (cd "${tmp_dir}" && sha256sum -c --status "${man_checksum}") \
		&& mkdir -p "${man_dir}" \
		&& tar -xzf "${tmp_dir}/${man_asset}" -C "${man_dir}"; then
		print_item "man pages: ${man_dir}"
	else
		print_item "man pages: skipped"
	fi

	install_dir_on_path="$(dir_on_path "${INSTALL_DIR}")"
	if [ "${install_dir_on_path}" = no ]; then
		print_item "PATH: add ${INSTALL_DIR} to your PATH"
	fi

	if [ -n "${resolved_runner}" ] && [ "${resolved_runner}" != "${expected_runner}" ]; then
		print_item 'refresh shell: run hash -r or restart the shell if needed'
	fi
}

main "$@"
