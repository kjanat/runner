#!/usr/bin/env bash

set -euo pipefail
shopt -s nullglob

# Required env vars supplied by the workflow. Declaring them as
# self-assignments with ${VAR:?} both fails fast on missing values
# and resolves shellcheck's "referenced but not assigned" warnings.
RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
EVENT_NAME="${EVENT_NAME:?EVENT_NAME required}"
DIST_TAG="${DIST_TAG:?DIST_TAG required}"
DRY_RUN="${DRY_RUN:?DRY_RUN required}"
REGISTRY="${REGISTRY:?REGISTRY required}"
# Optional: set by GHA, absent for local runs.
GITHUB_OUTPUT="${GITHUB_OUTPUT-}"

# The artifact is produced by the unprivileged build job from
# tag-supplied scripts and must be treated as untrusted. Three
# defenses run here before NPM_TOKEN is used:
#   1. Hardcoded allowlist of expected directory names — a malicious
#      build cannot smuggle extra package directories.
#   2. Each package.json's `name` field must equal the expected
#      scope/key — prevents republishing as an unexpected package.
#   3. Each package.json's `version` field must equal the version
#      derived from the trigger tag (trusted metadata) — prevents
#      stamping arbitrary versions onto allowed packages.
# Lists must be kept in sync with npm/targets.json. Tier-3 entries
# (release.yml's `experimental: true`) may legitimately be missing
# because their build job uses `continue-on-error`. Tier-1/2 are
# mandatory for release-triggered runs; for manual workflow_dispatch
# backfills (which run build with --skip-missing) we relax this so
# missing tier-1/2 packages are skipped instead of aborting. The
# façade itself remains mandatory either way.
FACADE="runner-run"
SCOPE="@runner-run"
REQUIRED_PLATFORMS=(
	linux-x64-gnu
	linux-x64-musl
	linux-arm64-gnu
	linux-arm64-musl
	linux-armv7-gnueabihf
	darwin-x64
	darwin-arm64
	win32-x64-msvc
	win32-arm64-msvc
	win32-ia32-msvc
	freebsd-x64
)
OPTIONAL_PLATFORMS=(
	freebsd-arm64
	netbsd-x64
	openbsd-x64
)
EXPECTED_VERSION="${RELEASE_TAG#v}"

# Refuse to proceed if the artifact contains anything outside the
# allowlist — that's either a misconfiguration or an attack.
allowed_set=" ${FACADE} ${REQUIRED_PLATFORMS[*]} ${OPTIONAL_PLATFORMS[*]} "
for dir in npm/dist/*/; do
	base=$(basename "${dir%/}")
	if [[ "${allowed_set}" != *" ${base} "* ]]; then
		echo "error: artifact contains unexpected directory '${base}' (not in allowlist)" >&2
		exit 1
	fi
done

publish_allowed() {
	local dir="$1" expected_name="$2" required="$3"
	local actual_name version published

	if [[ ! -d "${dir}" ]]; then
		if [[ "${required}" == "true" ]]; then
			echo "error: required package ${expected_name} missing from artifact" >&2
			exit 1
		fi
		echo "skip ${expected_name}: not in artifact (optional / experimental platform)"
		return 0
	fi
	if [[ ! -f "${dir}/package.json" ]]; then
		echo "error: ${dir}/package.json missing" >&2
		exit 1
	fi

	# Reject per-package registry overrides. A malicious build could
	# drop a .npmrc or set publishConfig in package.json to redirect
	# the publish (and NPM_TOKEN) to an attacker-controlled registry.
	# CLI --registry does NOT override scoped publishConfig.registry,
	# so the rejection here is the primary defense; the explicit
	# --registry flag below is belt-and-suspenders for non-scoped
	# overrides.
	if [[ -e "${dir}/.npmrc" ]]; then
		echo "error: ${dir}/.npmrc is forbidden (could redirect publish)" >&2
		exit 1
	fi
	if jq -e 'has("publishConfig")' "${dir}/package.json" >/dev/null; then
		echo "error: ${dir}/package.json has publishConfig (could redirect publish)" >&2
		exit 1
	fi

	actual_name=$(jq -r .name "${dir}/package.json")
	if [[ "${actual_name}" != "${expected_name}" ]]; then
		echo "error: ${dir}/package.json declares name '${actual_name}', expected '${expected_name}'" >&2
		exit 1
	fi
	version=$(jq -r .version "${dir}/package.json")
	if [[ "${version}" != "${EXPECTED_VERSION}" ]]; then
		echo "error: ${dir}/package.json declares version '${version}', expected '${EXPECTED_VERSION}' (from tag ${RELEASE_TAG})" >&2
		exit 1
	fi

	# Surface the package URL to the workflow. Repeated writes to the
	# same key resolve last-wins in GITHUB_OUTPUT, so the façade (which
	# publishes last) ends up as the canonical value.
	if [[ -n "${GITHUB_OUTPUT}" ]]; then
		echo "package-url=https://npm.im/${actual_name}" >>"${GITHUB_OUTPUT}"
	fi

	# Skip if already published — npm versions are immutable, so reruns
	# after a partial publish would otherwise fail on the first
	# sub-package that already published.
	published=$(npm view "${actual_name}@${version}" --registry "${REGISTRY}" version 2>/dev/null || true)
	if [[ "${published}" == "${version}" ]]; then
		echo "skip ${actual_name}@${version}: already published"
		return 0
	fi

	local args=(publish --registry "${REGISTRY}" --access public --tag "${DIST_TAG}" --ignore-scripts --provenance)
	if [[ "${DRY_RUN}" == "true" ]]; then args+=(--dry-run); fi
	echo "+ npm ${args[*]}  (cwd: ${dir})"
	# Tolerate the TOCTOU race between the npm view check above and
	# this publish: if another actor publishes the same version in
	# the gap, npm exits with EPUBLISHCONFLICT and we treat that as a
	# no-op (mirrors npm/scripts/publish.ts).
	#
	# The `|| status=$?` form is required: under `set -e`,
	# `output=$(cmd); status=$?` would exit on a failing cmd before
	# status was captured, and `if ! output=$(cmd); then status=$?`
	# captures the negation status (always 0), not npm's real exit
	# code — silently masking real publish failures.
	local output status=0
	output=$(cd "${dir}" && npm "${args[@]}" 2>&1) || status=$?
	if [[ "${status}" -ne 0 ]]; then
		printf '%s\n' "${output}" >&2
		if grep -Eiq 'EPUBLISHCONFLICT|cannot publish over the previously published versions' <<<"${output}"; then
			echo "skip ${actual_name}@${version}: already published (race with concurrent publisher)"
			return 0
		fi
		return "${status}"
	fi
	printf '%s\n' "${output}"
}

# On manual backfills (workflow_dispatch + --skip-missing in build),
# treat tier-1/2 as optional too — the user explicitly opted into a
# partial publish. The façade stays mandatory regardless.
if [[ "${EVENT_NAME}" == "workflow_dispatch" ]]; then
	tier12_required=false
else
	tier12_required=true
fi

# Sub-packages first so the façade's optionalDependencies resolve on install.
for platform in "${REQUIRED_PLATFORMS[@]}"; do
	publish_allowed "npm/dist/${platform}" "${SCOPE}/${platform}" "${tier12_required}"
done
for platform in "${OPTIONAL_PLATFORMS[@]}"; do
	publish_allowed "npm/dist/${platform}" "${SCOPE}/${platform}" false
done

# Façade is mandatory either way — no point publishing a half-empty
# set of platform packages with no entry point.
publish_allowed "npm/dist/${FACADE}" "${FACADE}" true
