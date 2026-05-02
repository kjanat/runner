#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
EVENT_NAME="${EVENT_NAME:?EVENT_NAME required}"
INPUT_DIST_TAG="${INPUT_DIST_TAG-}"
INPUT_DRY_RUN="${INPUT_DRY_RUN-false}"
GITHUB_OUTPUT="${GITHUB_OUTPUT:?GITHUB_OUTPUT required}"

if [[ -n "${INPUT_DIST_TAG}" ]]; then
	# Manual override always wins.
	dist_tag="${INPUT_DIST_TAG}"
else
	# Infer from the tag: prerelease (e.g. v1.0.0-rc.1) → next, else latest.
	case "${RELEASE_TAG}" in
		*-*) dist_tag=next ;;
		*) dist_tag=latest ;;
	esac
fi

if [[ "${EVENT_NAME}" == "workflow_dispatch" ]]; then
	dry_run="${INPUT_DRY_RUN}"
else
	dry_run=false
fi

{
	echo "dist-tag=${dist_tag}"
	echo "dry-run=${dry_run}"
} | tee -a "${GITHUB_OUTPUT}"
