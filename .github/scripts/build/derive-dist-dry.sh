#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
EVENT_NAME="${EVENT_NAME:?EVENT_NAME required}"
INPUT_DIST_TAG="${INPUT_DIST_TAG-}"
INPUT_DRY_RUN="${INPUT_DRY_RUN-false}"
GITHUB_OUTPUT="${GITHUB_OUTPUT:?GITHUB_OUTPUT required}"

if [[ -n "${INPUT_DIST_TAG}" ]]; then
	# Manual override always wins. Validate shape so a malformed input
	# can't slip flag-like or whitespace values into `npm publish --tag`.
	# npm dist-tags must start with a letter and use [A-Za-z0-9._-] only;
	# they also must not parse as semver (npm enforces this server-side).
	if [[ ! "${INPUT_DIST_TAG}" =~ ^[A-Za-z][A-Za-z0-9._-]*$ ]]; then
		echo "error: INPUT_DIST_TAG '${INPUT_DIST_TAG}' is not a valid npm dist-tag (^[A-Za-z][A-Za-z0-9._-]*$)" >&2
		exit 1
	fi
	dist_tag="${INPUT_DIST_TAG}"
else
	# Infer from the tag: prerelease (e.g. v1.0.0-rc.1) → next, else latest.
	case "${RELEASE_TAG}" in
		*-*) dist_tag=next ;;
		*) dist_tag=latest ;;
	esac
fi

if [[ "${EVENT_NAME}" == "workflow_dispatch" ]]; then
	# Normalize to strict true/false so downstream `[[ "${DRY_RUN}" == "true" ]]`
	# checks aren't fooled by "True"/"1"/"yes" silently meaning dry_run=false.
	case "${INPUT_DRY_RUN,,}" in
		true) dry_run=true ;;
		false | "") dry_run=false ;;
		*)
			echo "error: INPUT_DRY_RUN '${INPUT_DRY_RUN}' must be 'true' or 'false'" >&2
			exit 1
			;;
	esac
else
	dry_run=false
fi

{
	echo "dist-tag=${dist_tag}"
	echo "dry-run=${dry_run}"
} | tee -a "${GITHUB_OUTPUT}"
