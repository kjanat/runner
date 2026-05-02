#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"
EVENT_NAME="${EVENT_NAME:?EVENT_NAME required}"

version="${RELEASE_TAG#v}"
# build-packages.ts is tier-aware: tier-3 (experimental) targets
# are silently skipped when missing, tier 1/2 fail the job. Manual
# backfills (workflow_dispatch) extend that leniency to every tier
# via --skip-missing.
args=(npm/scripts/build-packages.ts --version "${version}")
if [[ "${EVENT_NAME}" == "workflow_dispatch" ]]; then
	args+=(--skip-missing)
fi
node "${args[@]}"
