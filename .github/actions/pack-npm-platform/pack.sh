#!/usr/bin/env bash
# Build a per-target npm sub-package from already-unpacked binaries.
#
# Inputs (env): PKG, SCOPE, FACADE, VERSION, OS (JSON), CPU (JSON),
#               LIBC (JSON or empty), BIN_DIR, BINARIES, OUT_DIR
# Outputs: tarball, name -> $GITHUB_OUTPUT

set -euo pipefail

: "${PKG:?}" "${SCOPE:?}" "${FACADE:?}" "${VERSION:?}"
: "${OS:?}" "${CPU:?}" "${BIN_DIR:?}" "${BINARIES:?}" "${OUT_DIR:?}"

FULL_NAME="${SCOPE}/${PKG}"
STAGE="$(mktemp -d)"
mkdir -p "${STAGE}/bin" "${OUT_DIR}"

# Copy binaries; preserve .exe on Windows. resolve.cjs re-adds the extension
# based on process.platform, so the file just needs to be present.
for bin in ${BINARIES}; do
    if [[ -f "${BIN_DIR}/${bin}.exe" ]]; then
        cp "${BIN_DIR}/${bin}.exe" "${STAGE}/bin/${bin}.exe"
    elif [[ -f "${BIN_DIR}/${bin}" ]]; then
        cp "${BIN_DIR}/${bin}" "${STAGE}/bin/${bin}"
        chmod +x "${STAGE}/bin/${bin}"
    else
        echo "::error::missing binary ${bin} in ${BIN_DIR}"
        exit 1
    fi
done

cp README.md LICENSE "${STAGE}/" 2>/dev/null || true

# Build package.json. libc field is omitted entirely on darwin/win32 so npm's
# selector doesn't reject those hosts.
LIBC_FRAG='{}'
if [[ -n "${LIBC}" ]]; then
    LIBC_FRAG="$(jq -n --argjson v "${LIBC}" '{libc: $v}')"
fi

jq -n \
    --arg     name        "${FULL_NAME}" \
    --arg     version     "${VERSION}" \
    --arg     description "Prebuilt ${FACADE} binary for ${PKG}" \
    --argjson os          "${OS}" \
    --argjson cpu         "${CPU}" \
    --argjson libc_obj    "${LIBC_FRAG}" \
    '{
        name: $name,
        version: $version,
        description: $description,
        homepage: "https://runner.kjanat.dev",
        bugs: { url: "https://github.com/kjanat/runner/issues" },
        repository: { type: "git", url: "git+https://github.com/kjanat/runner.git" },
        license: "MIT",
        author: "Kaj Kowalski <info+runner@kajkowalski.nl>",
        os: $os,
        cpu: $cpu,
        files: ["bin/", "README.md", "LICENSE"],
        preferUnplugged: true
    } + $libc_obj' > "${STAGE}/package.json"

TGZ_NAME=$(cd "${STAGE}" && npm pack --json --pack-destination "${OUT_DIR}" | jq -r '.[0].filename')
TGZ_PATH="${OUT_DIR}/${TGZ_NAME}"

{
    echo "tarball=${TGZ_PATH}"
    echo "name=${FULL_NAME}"
} >> "$GITHUB_OUTPUT"

echo "Packed ${FULL_NAME}@${VERSION} -> ${TGZ_PATH}"
