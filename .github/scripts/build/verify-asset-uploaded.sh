#!/usr/bin/env bash
#
# Assert the release asset for this matrix leg actually landed on the GitHub
# release. Run immediately after the build+upload step in release.yml's
# `upload-assets` job.
#
# Why this exists: a build leg can report `success` while uploading nothing.
# `taiki-e/upload-rust-binary-action` on the `windows-11-arm` runner has been
# observed to exit 0 in under a second — no `cargo build`, no archive, no
# upload, no diagnostic — leaving the leg green but assetless. Nothing noticed
# until `build-dist` ENOENT'd three jobs later deep in build-packages.ts, by
# which point the draft release was unpublishable and the failure pointed at
# the wrong job. This turns that silent no-op into a red leg at the source,
# with an actionable message — and makes "re-run failed jobs" rebuild it
# instead of skipping a green-but-empty leg.
#
# Required env:
#   RELEASE_TAG        — git tag, e.g. `v0.15.0`. Same value the matrix consumes.
#   TARGET             — Rust target triple, e.g. `aarch64-pc-windows-msvc`.
#   GITHUB_REPOSITORY  — owner/repo, e.g. `kjanat/runner`.
#   GH_TOKEN           — token with `contents: read` on this repo.

set -euo pipefail

: "${RELEASE_TAG:?RELEASE_TAG required}"
: "${TARGET:?TARGET required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"
: "${GH_TOKEN:?GH_TOKEN required}"

archive_basename="runner-${RELEASE_TAG}-${TARGET}"
archive="${archive_basename}.tar.gz"
# `<basename>.sha256`, NOT `<basename>.tar.gz.sha256` — the convention
# taiki-e/upload-rust-binary-action and package-release-asset.sh both produce
# and verify-checksum.sh enforces.
checksum="${archive_basename}.sha256"

# Retry the listing a few times: the upload API is consistent once it returns,
# but a transient network blip on the read shouldn't fail a leg that genuinely
# uploaded. Re-fetch each attempt so a slow-to-appear asset still resolves.
missing=()
for attempt in 1 2 3; do
	mapfile -t assets < <(
		gh release view "${RELEASE_TAG}" \
			--repo "${GITHUB_REPOSITORY}" \
			--json assets \
			--jq '.assets[].name'
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
