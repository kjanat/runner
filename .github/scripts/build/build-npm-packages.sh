#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"

version="${RELEASE_TAG#v}"

# Man pages come from the `man` job's artifact, downloaded to ./man.
man_arg=()
[[ -d man ]] && man_arg=(--man-dir man)

# build-packages.ts is tier-aware: tier-3 (experimental) targets are
# silently skipped when missing; tier-1/2 missing fails the job. This
# script is only invoked from release.yml's build-npm-dist (tag-push
# context), so a missing tier-1/2 tarball is always a real failure —
# no --skip-missing relaxation. The flag still exists in the script
# for local dev partial builds.
node npm/scripts/build-packages.ts --version "${version}" "${man_arg[@]}"
