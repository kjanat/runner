#!/usr/bin/env bash

set -euo pipefail
shopt -s nullglob

# Required env vars supplied by the workflow. Declaring them as
# self-assignments with ${VAR:?} both fails fast on missing values
# and resolves shellcheck's "referenced but not assigned" warnings.
RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
DIST_TAG="${DIST_TAG:?DIST_TAG required}"
DRY_RUN="${DRY_RUN:?DRY_RUN required}"
REGISTRY="${REGISTRY:?REGISTRY required}"
# Optional: set by GHA, absent for local runs.
GITHUB_OUTPUT="${GITHUB_OUTPUT-}"

# The artifact is built by release.yml's `build-dist` job (tag-push
# context) and downloaded here via cross-workflow `download-artifact`.
# We still treat it as untrusted: defense-in-depth against a tampered
# artifact at the cross-workflow handoff or a malicious tag committer.
# Three defenses run before npm is invoked:
#   1. Hardcoded allowlist of expected directory names — a tampered
#      artifact cannot smuggle extra package directories.
#   2. Each package.json's `name` field must equal the expected
#      scope/key — prevents republishing as an unexpected package.
#   3. Each package.json's `version` field must equal the version
#      derived from the trigger tag (trusted metadata) — prevents
#      stamping arbitrary versions onto allowed packages.
# Single source of truth: npm/targets.json. `experimental: true`
# packages may legitimately be missing because their build matrix uses
# `continue-on-error`. Everything else is mandatory for release-triggered
# runs; for manual workflow_dispatch backfills we relax this so missing
# required packages are skipped instead of aborting. The façade itself
# remains mandatory either way.
TARGETS_JSON="${GITHUB_WORKSPACE:-.}/npm/targets.json"
FACADE=$(jq -r '.facade' "${TARGETS_JSON}")
SCOPE=$(jq -r '.scope' "${TARGETS_JSON}")
mapfile -t REQUIRED_PLATFORMS < <(jq -r '.targets[] | select((.experimental // false) | not) | .pkg' "${TARGETS_JSON}")
mapfile -t OPTIONAL_PLATFORMS < <(jq -r '.targets[] | select(.experimental // false) | .pkg' "${TARGETS_JSON}")
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

# publish_allowed publishes a single package from a built artifact directory
# when it exists and its package.json matches the expected name and version,
# skips optional or already-published packages, and exits on integrity or policy
# failures.
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
	# the publish to an attacker-controlled registry.
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

	# optionalDependencies validation. The facade is the only package that
	# legitimately ships optionalDependencies (one entry per built platform
	# package, all pinned to EXPECTED_VERSION). Platform packages must have
	# none — a tampered platform package could otherwise smuggle attacker-
	# controlled deps that npm would happily install transitively.
	if [[ "${expected_name}" == "${FACADE}" ]]; then
		local dep_name dep_version platform expected_dep_set=" ${REQUIRED_PLATFORMS[*]} ${OPTIONAL_PLATFORMS[*]} "
		while IFS=$'\t' read -r dep_name dep_version; do
			[[ -z "${dep_name}" ]] && continue
			if [[ "${dep_name}" != "${SCOPE}/"* ]]; then
				echo "error: facade optionalDependencies entry '${dep_name}' not under scope '${SCOPE}'" >&2
				exit 1
			fi
			platform="${dep_name#"${SCOPE}/"}"
			if [[ "${expected_dep_set}" != *" ${platform} "* ]]; then
				echo "error: facade optionalDependencies references unexpected package '${dep_name}'" >&2
				exit 1
			fi
			if [[ "${dep_version}" != "${EXPECTED_VERSION}" ]]; then
				echo "error: facade optionalDependencies['${dep_name}'] = '${dep_version}', expected '${EXPECTED_VERSION}'" >&2
				exit 1
			fi
		done < <(jq -r '(.optionalDependencies // {}) | to_entries[] | "\(.key)\t\(.value)"' "${dir}/package.json")

		# Required platforms must all be referenced.
		for platform in "${REQUIRED_PLATFORMS[@]}"; do
			if ! jq -e --arg dep "${SCOPE}/${platform}" '(.optionalDependencies // {}) | has($dep)' "${dir}/package.json" >/dev/null; then
				echo "error: facade optionalDependencies missing required package '${SCOPE}/${platform}'" >&2
				exit 1
			fi
		done
	else
		if jq -e '(.optionalDependencies // {}) | length > 0' "${dir}/package.json" >/dev/null; then
			echo "error: ${dir}/package.json has optionalDependencies; only ${FACADE} may declare any" >&2
			exit 1
		fi
	fi

	# Surface the package URL to the workflow. Repeated writes to the
	# same key resolve last-wins in GITHUB_OUTPUT, so the façade (which
	# publishes last) ends up as the canonical value.
	if [[ -n "${GITHUB_OUTPUT}" ]]; then
		echo "package-url=https://npm.im/package/${actual_name}/v/${version}" >>"${GITHUB_OUTPUT}"
	fi

	# Skip if already published — npm versions are immutable, so reruns
	# after a partial publish would otherwise fail on the first
	# sub-package that already published. Bound the probe at 120s so a
	# hung registry can't stall the whole publish job. Non-timeout
	# failures (e.g. E404 when the version isn't published yet) drop
	# through to the publish step, which surfaces real errors.
	local view_status=0
	published=$(timeout 120s npm view "${actual_name}@${version}" --registry "${REGISTRY}" version 2>/dev/null) || view_status=$?
	if [[ ${view_status} -eq 124 ]]; then
		echo "error: 'npm view ${actual_name}@${version}' timed out after 120s" >&2
		return 1
	fi
	if [[ "${published}" == "${version}" ]]; then
		echo "skip ${actual_name}@${version}: already published"
		return 0
	fi

	local args=(publish --registry "${REGISTRY}" --access public --tag "${DIST_TAG}" --ignore-scripts --provenance)
	if [[ "${DRY_RUN}" == "true" ]]; then args+=(--dry-run); fi
	echo "+ npx -y npm@latest ${args[*]}  (cwd: ${dir})"
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
	output=$(cd "${dir}" && timeout 120s npx -y npm@latest "${args[@]}" 2>&1) || status=$?
	if [[ "${status}" -eq 124 ]]; then
		printf '%s\n' "${output}" >&2
		echo "error: 'npx -y npm@latest publish' for ${actual_name}@${version} timed out after 120s" >&2
		return 1
	fi
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

# Tier-1/2 are always required: the artifact is built by release.yml's
# build-dist (where missing tier-1/2 tarballs already fail loud),
# so a missing dir here means the artifact was tampered with or the
# build silently dropped a target — either case warrants a hard fail.
# Sub-packages first so the façade's optionalDependencies resolve on install.
for platform in "${REQUIRED_PLATFORMS[@]}"; do
	publish_allowed "npm/dist/${platform}" "${SCOPE}/${platform}" true
done
for platform in "${OPTIONAL_PLATFORMS[@]}"; do
	publish_allowed "npm/dist/${platform}" "${SCOPE}/${platform}" false
done

# Façade is mandatory either way — no point publishing a half-empty
# set of platform packages with no entry point.
publish_allowed "npm/dist/${FACADE}" "${FACADE}" true
