#!/usr/bin/env bash
#
# Package built `runner` and `run` binaries into a release tarball matching
# the layout `taiki-e/upload-rust-binary-action` produces, then upload the
# archive and its `.sha256` companion to the GitHub release for the current
# tag.
#
# Used by release.yml for build paths that can't go through
# `taiki-e/upload-rust-binary-action`:
#
# * tier-3 BSD targets requiring `-Z build-std` (the action has no way to
#   inject that flag), and
# * VM-built targets such as OpenBSD (the action runs on the outer Linux
#   host and never sees the VM's filesystem).
#
# Required env:
#   RELEASE_TAG  — git tag, e.g. `v0.6.0`. Same value the matrix consumes.
#   TARGET       — Rust target triple, e.g. `aarch64-unknown-freebsd`.
#   BIN_DIR      — directory containing the freshly built `runner` and `run`
#                  binaries (no `.exe` — BSDs don't use it).
#   GH_TOKEN     — token with `contents: write` on this repo.

set -euo pipefail

: "${RELEASE_TAG:?RELEASE_TAG required}"
: "${TARGET:?TARGET required}"
: "${BIN_DIR:?BIN_DIR required}"
: "${GH_TOKEN:?GH_TOKEN required}"

# Defensive: this script doesn't handle .exe binaries. The release.yml
# matrix only routes BSDs through cargo-build-std today, but a future
# config could route a Windows target here and silently produce a
# broken archive. Bail loudly instead.
if [[ "${TARGET}" == *windows* ]]; then
	echo "error: ${0##*/} does not handle Windows targets (.exe naming)" >&2
	exit 1
fi

archive_basename="runner-${RELEASE_TAG}-${TARGET}"
archive="${archive_basename}.tar.gz"
# `<basename>.sha256`, NOT `<basename>.tar.gz.sha256`. Matches the
# convention `taiki-e/upload-rust-binary-action` uses, which is what
# verify-checksum.sh enforces (`expected="${t%.tar.gz}.sha256"`).
checksum="${archive_basename}.sha256"

staging=$(mktemp -d)
trap 'rm -rf "${staging}"' EXIT

# Lay out the contents the way upload-rust-binary-action does with
# `leading_dir: false` and `include: README.md,LICENSE`: every file at the
# tarball root, no wrapper directory. build-packages.ts only matches by
# basename, but verify-checksum.sh and any user inspecting the archive
# expect this exact layout.
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

# verify-checksum.sh requires the listed name in the .sha256 to match the
# archive's basename exactly (no path component). `sha256sum` writes the
# basename when invoked from the file's directory, so cd in.
sha256sum "${archive}" >"${checksum}"

gh release upload "${RELEASE_TAG}" "${archive}" "${checksum}" --clobber
