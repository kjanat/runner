#!/usr/bin/env bash
# Stage the just-built binaries into a flat tar.gz matching the binstall
# `pkg-fmt = "tgz"` + `bin-dir = "{bin}{binary-ext}"` contract in Cargo.toml.
# Always emits .tar.gz — install.sh and action.yml are the only consumers
# and both fetch .tar.gz only. Git Bash on Windows runners has tar(1).
#
# Inputs (env): TRIPLE, VERSION, BINARIES (space-separated)
# Outputs: archive, sha256, bin-dir -> $GITHUB_OUTPUT

set -euo pipefail

: "${TRIPLE:?missing}"
: "${VERSION:?missing}"
: "${BINARIES:?missing}"

ARCHIVE_NAME="runner-v${VERSION}-${TRIPLE}"
OUT_DIR="${GITHUB_WORKSPACE}/dist/bin-${TRIPLE}"
STAGE="$(mktemp -d)"
mkdir -p "${OUT_DIR}"

for bin in ${BINARIES}; do
	if [[ -f "target/${TRIPLE}/release/${bin}.exe" ]]; then
		cp "target/${TRIPLE}/release/${bin}.exe" "${STAGE}/"
	elif [[ -f "target/${TRIPLE}/release/${bin}" ]]; then
		cp "target/${TRIPLE}/release/${bin}" "${STAGE}/"
	else
		echo "::error::missing target/${TRIPLE}/release/${bin}[.exe]"
		exit 1
	fi
done
cp README.md LICENSE "${STAGE}/"

ARCHIVE="${ARCHIVE_NAME}.tar.gz"
tar -C "${STAGE}" -czf "${OUT_DIR}/${ARCHIVE}" .

# Portable SHA256: sha256sum on Linux + Git Bash, shasum on macOS.
cd "${OUT_DIR}"
if command -v sha256sum >/dev/null 2>&1; then
	sha256sum "${ARCHIVE}" >"${ARCHIVE}.sha256"
else
	shasum -a 256 "${ARCHIVE}" >"${ARCHIVE}.sha256"
fi

{
	echo "archive=${OUT_DIR}/${ARCHIVE}"
	echo "sha256=${OUT_DIR}/${ARCHIVE}.sha256"
	echo "bin-dir=${STAGE}"
} >>"$GITHUB_OUTPUT"
