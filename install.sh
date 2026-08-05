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

# First release that ships the aarch64-linux-android asset. Older releases
# only have ET_EXEC musl binaries, which Android's bionic loader rejects
# with "unexpected e_type: 2".
ANDROID_MIN_VERSION="0.24.0"

# $1: "yes" when running on Android/Termux (see is_android). Android needs
# the bionic PIE binary; the non-PIE static musl binaries don't load there.
resolve_target() {
	arch="$(uname -m)"

	if [ "${1}" = yes ]; then
		case "${arch}" in
			aarch64 | arm64) printf 'aarch64-linux-android\n' ;;
			*)
				printf 'error: unsupported architecture on Android: %s\n' "${arch}" >&2
				exit 1
				;;
		esac
		return
	fi

	os="$(uname -s)"
	case "${os}" in
		Linux)
			case "${arch}" in
				x86_64) printf 'x86_64-unknown-linux-musl\n' ;;
				aarch64 | arm64) printf 'aarch64-unknown-linux-musl\n' ;;
				*) unsupported_arch "${os}" "${arch}" ;;
			esac
			;;
		FreeBSD)
			# FreeBSD's `uname -m` reports amd64/arm64, not x86_64/aarch64.
			case "${arch}" in
				x86_64 | amd64) printf 'x86_64-unknown-freebsd\n' ;;
				aarch64 | arm64) printf 'aarch64-unknown-freebsd\n' ;;
				*) unsupported_arch "${os}" "${arch}" ;;
			esac
			;;
		*)
			printf 'error: unsupported operating system: %s\n' "${os}" >&2
			exit 1
			;;
	esac
}

unsupported_arch() {
	printf 'error: unsupported architecture on %s: %s\n' "${1}" "${2}" >&2
	exit 1
}

# These predicates print "yes"/"no" rather than returning an exit status:
# callers invoke them via command substitution and test the printed string.
# That keeps them composable under `set -e` without ShellCheck SC2310; a
# function used directly as a condition silently disables set -e inside it.
dir_on_path() {
	case ":${PATH:-}:" in
		*:"$1":*) printf 'yes\n' ;;
		*) printf 'no\n' ;;
	esac
}

# Kernel/userspace signal first (`uname -o` = Android), then Termux-specific
# fallbacks. A bare ANDROID_ROOT is deliberately not enough: Linux dev
# machines carry Android SDK env vars, so it only counts alongside the real
# bionic linker path.
is_android() {
	case "$(uname -o 2>/dev/null || true)" in
		Android)
			printf 'yes\n'
			;;
		*)
			if [ -n "${TERMUX_VERSION:-}" ] \
				|| [ "${PREFIX:-}" = "/data/data/com.termux/files/usr" ] \
				|| { [ "${ANDROID_ROOT:-}" = "/system" ] && [ -x /system/bin/linker64 ]; }; then
				printf 'yes\n'
			else
				printf 'no\n'
			fi
			;;
	esac
}

# version_ge A B: "yes" when dotted version A >= B (numeric per component;
# a leading "v" is ignored). On equal numeric components a pre-release
# (X.Y.Z-pre) sorts below the stable release it points at.
version_ge() {
	a="${1#v}" b="${2#v}" i=1
	while [ "${i}" -le 3 ]; do
		ai="$(printf '%s\n' "${a}" | cut -d. -f"${i}")"
		bi="$(printf '%s\n' "${b}" | cut -d. -f"${i}")"
		ai="${ai%%[!0-9]*}" bi="${bi%%[!0-9]*}"
		if [ "${ai:-0}" -gt "${bi:-0}" ]; then
			printf 'yes\n'
			return
		fi
		if [ "${ai:-0}" -lt "${bi:-0}" ]; then
			printf 'no\n'
			return
		fi
		i=$((i + 1))
	done
	case "${a}" in
		*-*)
			case "${b}" in
				*-*) printf 'yes\n' ;;
				*) printf 'no\n' ;;
			esac
			;;
		*) printf 'yes\n' ;;
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

	case "${os_name}" in
		Linux | FreeBSD) ;;
		*)
			printf 'error: install.sh does not support this OS: %s\n' "${os_name}" >&2
			exit 1
			;;
	esac

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

	on_android="$(is_android)"
	if [ "${on_android}" = yes ]; then
		android_ok="$(version_ge "${version}" "v${ANDROID_MIN_VERSION}")"
		if [ "${android_ok}" = no ]; then
			printf 'error: %s predates Android/Termux support\n' "${version}" >&2
			printf 'error: Android needs the aarch64-linux-android asset first shipped in v%s\n' "${ANDROID_MIN_VERSION}" >&2
			printf 'hint: rerun without a version pin, or: cargo install runner-run --locked\n' >&2
			exit 1
		fi
	fi

	target="$(resolve_target "${on_android}")"

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
		# busybox sha256sum has no --status.
		sha256sum -c "${checksum_asset}" >/dev/null 2>&1
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

	# The version banner is the success criterion, not the exit status: on
	# Android a loader failure surfaces as garbage on stderr with an
	# unreliable exit code, so match the banner like is_our_runner does.
	installed_status=0
	installed_output="$("${expected_runner}" -V 2>&1)" || installed_status=$?

	case "${installed_output}" in
		"runner "[0-9]*.[0-9]*.[0-9]*)
			print_item "version: ${installed_output}"
			;;
		*)
			if [ "${on_android}" = yes ]; then
				printf 'error: installed runner could not execute on Android (exit %s)\n' "${installed_status}" >&2
				printf '  %s\n' "${installed_output:-no output}" >&2
				printf 'error: this should not happen on release >= v%s; please report it:\n' "${ANDROID_MIN_VERSION}" >&2
				printf 'error:   https://github.com/kjanat/runner/issues\n' >&2
				exit 1
			fi

			print_item "warning: failed to execute ${expected_runner} -V"
			if [ -n "${installed_output}" ]; then
				print_item "output: ${installed_output}"
			fi
			;;
	esac

	# Man pages from the release archive, into the XDG user man path. Verified like the binaries above.
	# Best-effort: a read-only $HOME, a missing asset, or a checksum mismatch must not fail the install.
	man_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/man/man1"
	man_asset="runner-${version}-man.tar.gz"
	man_checksum="runner-${version}-man.sha256"
	if curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${man_asset}" "${base_url}/${man_asset}" 2>/dev/null \
		&& curl -fsSL --retry 3 --retry-delay 1 -o "${tmp_dir}/${man_checksum}" "${base_url}/${man_checksum}" 2>/dev/null \
		&& (cd "${tmp_dir}" && sha256sum -c "${man_checksum}" >/dev/null 2>&1) \
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
