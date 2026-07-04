#!/usr/bin/env bash
# Subcommands for release.yml. One script per workflow; dispatch at the bottom.

set -euo pipefail

# Package built `runner` and `run` binaries into a release tarball matching
# the layout `taiki-e/upload-rust-binary-action` produces, then upload the
# archive and its `.sha256` companion to the GitHub release for the current
# tag. Used for build paths that can't go through the action: tier-3 BSD
# targets requiring `-Z build-std`, and VM-built targets whose filesystem
# the action never sees.
#
# Required env: RELEASE_TAG, TARGET, BIN_DIR, GH_TOKEN (contents: write).
cmd_package_asset() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"
	: "${TARGET:?TARGET required}"
	: "${BIN_DIR:?BIN_DIR required}"
	: "${GH_TOKEN:?GH_TOKEN required}"

	# Defensive: this path doesn't handle .exe binaries. Bail loudly if a
	# future matrix config routes a Windows target here.
	if [[ "${TARGET}" == *windows* ]]; then
		echo "error: package-asset does not handle Windows targets (.exe naming)" >&2
		exit 1
	fi

	local archive_basename="runner-${RELEASE_TAG}-${TARGET}"
	local archive="${archive_basename}.tar.gz"
	# `<basename>.sha256`, NOT `<basename>.tar.gz.sha256`. Matches the
	# convention `taiki-e/upload-rust-binary-action` uses, which is what
	# verify-checksums enforces (`expected="${t%.tar.gz}.sha256"`).
	local checksum="${archive_basename}.sha256"

	# Not `local`: the EXIT trap runs at top level, after locals are gone.
	staging=$(mktemp -d)
	trap 'rm -rf "${staging-}"' EXIT

	# Lay out the contents the way upload-rust-binary-action does with
	# `leading_dir: false` and `include: README.md,LICENSE`: every file at
	# the tarball root, no wrapper directory.
	local bin src
	for bin in runner run; do
		src="${BIN_DIR}/${bin}"
		if [[ ! -f "${src}" ]]; then
			echo "error: ${src} not found — build step did not produce ${bin}" >&2
			exit 1
		fi
		cp "${src}" "${staging}/${bin}"
		chmod +x "${staging}/${bin}"
	done
	cp README.md LICENSE "${staging}/"

	tar -C "${staging}" -czf "${archive}" runner run README.md LICENSE
	sha256sum "${archive}" >"${checksum}"

	gh release upload "${RELEASE_TAG}" "${archive}" "${checksum}" --clobber
}

# Assert the release asset for this matrix leg actually landed on the GitHub
# release; a build leg can report `success` while uploading nothing
# (observed: taiki-e action silent no-op on windows-11-arm). Turns the silent
# no-op into a red leg at the source.
#
# Required env: RELEASE_TAG, TARGET, GITHUB_REPOSITORY, GH_TOKEN (contents: read).
cmd_verify_asset() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"
	: "${TARGET:?TARGET required}"
	: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"
	: "${GH_TOKEN:?GH_TOKEN required}"

	local archive_basename="runner-${RELEASE_TAG}-${TARGET}"
	local archive="${archive_basename}.tar.gz"
	local checksum="${archive_basename}.sha256"

	# Retry the listing a few times: a transient network blip on the read
	# shouldn't fail a leg that genuinely uploaded.
	local missing=() assets=() asset want have found attempt
	for attempt in 1 2 3; do
		# Read line-by-line rather than `mapfile`: macOS runners ship
		# bash 3.2, which predates the builtin.
		assets=()
		while IFS= read -r asset; do
			assets+=("${asset}")
		done < <(
			# `|| true`: a failed listing leaves `assets` empty, which the
			# retry loop treats as missing and re-fetches.
			gh release view "${RELEASE_TAG}" \
				--repo "${GITHUB_REPOSITORY}" \
				--json assets \
				--jq '.assets[].name' || true
		)

		missing=()
		for want in "${archive}" "${checksum}"; do
			found=''
			for have in ${assets[@]+"${assets[@]}"}; do
				if [[ "${have}" == "${want}" ]]; then
					found=1
					break
				fi
			done
			if [[ -z "${found}" ]]; then
				missing+=("${want}")
			fi
		done

		if [[ "${#missing[@]}" -eq 0 ]]; then
			break
		fi
		if [[ "${attempt}" -lt 3 ]]; then
			sleep "$((attempt * 2))"
		fi
	done

	if [[ "${#missing[@]}" -gt 0 ]]; then
		echo "error: build+upload for ${TARGET} reported success but these assets are not on release ${RELEASE_TAG}:" >&2
		printf '  - %s\n' "${missing[@]}" >&2
		echo "the build step produced no artifact — inspect its log for a silent no-op." >&2
		exit 1
	fi

	echo "verified ${archive} and ${checksum} on release ${RELEASE_TAG}"
}

# Download every release tarball + checksum into npm/downloads.
# Required env: RELEASE_TAG, GITHUB_REPOSITORY, GH_TOKEN.
cmd_download_archives() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"
	: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"

	# Scrub before fetch: stale files from a previous tag would pass
	# verify-checksums but be wrong-version. Hosted runners get fresh
	# workspaces; self-hosted runners and local invocations don't.
	rm -rf npm/downloads
	mkdir -p npm/downloads
	gh release download "${RELEASE_TAG}" \
		--repo "${GITHUB_REPOSITORY}" \
		--pattern 'runner-*-*.tar.gz' \
		--pattern 'runner-*-*.sha256' \
		--dir npm/downloads
	ls -la npm/downloads
}

# Verify every downloaded tarball against its .sha256 (run in npm/downloads).
# Required env: RELEASE_TAG.
cmd_verify_checksums() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"
	shopt -s nullglob

	local tarballs=(*.tar.gz)
	local sums=(*.sha256)

	# Every tarball needs a matching .sha256 and vice versa — otherwise an
	# unchecksummed binary could slip through to publish.
	if [[ "${#tarballs[@]}" -eq 0 ]]; then
		echo "error: no tarballs downloaded for ${RELEASE_TAG}" >&2
		exit 1
	fi
	local t s expected inner
	for t in "${tarballs[@]}"; do
		expected="${t%.tar.gz}.sha256"
		if [[ ! -f "${expected}" ]]; then
			echo "error: tarball ${t} has no matching ${expected}" >&2
			exit 1
		fi
	done

	# Each .sha256 must reference a tarball matching its own basename —
	# defends against a swapped reference leaving a tarball unchecked.
	for s in "${sums[@]}"; do
		inner=$(awk '{sub(/^\*/, "", $2); print $2}' "${s}")
		expected="${s%.sha256}.tar.gz"
		if [[ ! -f "${expected}" ]]; then
			echo "error: checksum file ${s} has no matching ${expected}" >&2
			exit 1
		fi
		if [[ "${inner}" != "${expected}" ]]; then
			echo "error: ${s} references '${inner}', expected '${expected}'" >&2
			exit 1
		fi
	done

	local sum
	for sum in "${sums[@]}"; do
		sha256sum -c --status "${sum}"
	done
}

# Build the npm packages from npm/downloads via build-packages.ts.
# Required env: RELEASE_TAG.
cmd_build_npm_packages() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"

	local version="${RELEASE_TAG#v}"

	# Man pages come from the `man` job's artifact, downloaded to ./man.
	local man_arg=()
	[[ -d man ]] && man_arg=(--man-dir man)

	# build-packages.ts is tier-aware: missing tier-3 (experimental)
	# tarballs are skipped, missing tier-1/2 fail the job.
	node npm/scripts/build-packages.ts --version "${version}" "${man_arg[@]}"
}

case "${1-}" in
	package-asset) cmd_package_asset ;;
	verify-asset) cmd_verify_asset ;;
	download-archives) cmd_download_archives ;;
	verify-checksums) cmd_verify_checksums ;;
	build-npm-packages) cmd_build_npm_packages ;;
	*)
		echo "usage: ${0##*/} <package-asset|verify-asset|download-archives|verify-checksums|build-npm-packages>" >&2
		exit 2
		;;
esac
