#!/usr/bin/env bash

set -euo pipefail

RELEASE_TAG="${RELEASE_TAG:?RELEASE_TAG required}"

version="${RELEASE_TAG#v}"

# Render man pages once from the host (x86_64 linux-gnu) binary. roff is
# platform-independent, so a single render serves every platform; the facade
# ships them via its package.json `man` field so `man runner` / `man run`
# work for npm global installs too. The gnu x86_64 tarball is tier-1 and is
# always present in npm/downloads (executable on the ubuntu-latest runner).
man_root="$(mktemp -d)"
trap 'rm -rf "${man_root}"' EXIT
host_tarball="npm/downloads/runner-${RELEASE_TAG}-x86_64-unknown-linux-gnu.tar.gz"
tar -xzf "${host_tarball}" -C "${man_root}" runner
"${man_root}/runner" man --output "${man_root}/man"

# build-packages.ts is tier-aware: tier-3 (experimental) targets are
# silently skipped when missing; tier-1/2 missing fails the job. This
# script is only invoked from release.yml's build-npm-dist (tag-push
# context), so a missing tier-1/2 tarball is always a real failure —
# no --skip-missing relaxation. The flag still exists in the script
# for local dev partial builds.
node npm/scripts/build-packages.ts --version "${version}" --man-dir "${man_root}/man"
