#!/usr/bin/env bash
# Stage the facade package, inject optionalDependencies for tier-1/tier-2
# targets only (tier-3 may have failed to publish), stamp the version, pack.
#
# Inputs (env): FACADE_DIR, TARGETS_JSON, VERSION, OUT_DIR
# Outputs: tarball, name -> $GITHUB_OUTPUT

set -euo pipefail

: "${FACADE_DIR:?}" "${TARGETS_JSON:?}" "${VERSION:?}" "${OUT_DIR:?}"

mkdir -p "${OUT_DIR}"
STAGE="$(mktemp -d)"
cp -R "${FACADE_DIR}/." "${STAGE}/"
cp README.md LICENSE "${STAGE}/" 2>/dev/null || true

SCOPE=$(jq -r '.scope' "${TARGETS_JSON}")
FACADE_NAME=$(jq -r '.facade' "${TARGETS_JSON}")

# optionalDependencies: { "<scope>/<pkg>": "<version>" } for tier <= 2 only.
OPT_DEPS=$(jq --arg s "${SCOPE}" --arg v "${VERSION}" \
	'[.targets[] | select(.tier <= 2) | { key: ($s + "/" + .pkg), value: $v }] | from_entries' \
	"${TARGETS_JSON}")

TMP=$(mktemp)
jq --arg v "${VERSION}" --argjson od "${OPT_DEPS}" \
	'.version = $v | .optionalDependencies = $od' \
	"${STAGE}/package.json" >"${TMP}"
mv "${TMP}" "${STAGE}/package.json"

TGZ_NAME=$(cd "${STAGE}" && npm pack --json --pack-destination "${OUT_DIR}" | jq -r '.[0].filename')
TGZ_PATH="${OUT_DIR}/${TGZ_NAME}"

{
	echo "tarball=${TGZ_PATH}"
	echo "name=${FACADE_NAME}"
} >>"$GITHUB_OUTPUT"

echo "Packed facade ${FACADE_NAME}@${VERSION} -> ${TGZ_PATH}"
