#!/usr/bin/env bash
# Subcommands for crates-release.yml. One script per workflow; dispatch at the bottom.

set -euo pipefail

readonly API=https://crates.io/api/v1/crates

status() {
	curl --silent --show-error --output /dev/null --write-out '%{http_code}' \
		--max-time 30 --retry 4 --retry-delay 5 --retry-connrefused \
		--header "User-Agent: ${GITHUB_REPOSITORY:-kjanat/runner} crates-release" \
		"${API}/${1}"
}

# Publishable workspace crates as a JSON array, dependencies first.
#
# Usage: crates.sh order
cmd_order() {
	: "${GITHUB_OUTPUT:?GITHUB_OUTPUT required}"
	local crates
	crates="$(cargo metadata --no-deps --format-version 1 | jq -c '
		.packages as $packages
		| ($packages | map({ key: .manifest_path, value: .name }) | from_entries) as $by_manifest
		| [$packages[]
			| select(.publish != [])
			| { name, deps: [.dependencies[] | select(.path != null) | $by_manifest[.path + "/Cargo.toml"] // empty] }]
		| def order($remaining; $done):
			if ($remaining | length) == 0 then []
			else
				($remaining | map(select(all(.deps[]; . as $dep | $done | index($dep)))) | map(.name) | sort) as $ready
				| if ($ready | length) == 0 then error("workspace dependency cycle")
				else $ready + order($remaining | map(select(.name as $name | $ready | index($name) | not)); $done + $ready)
				end
			end;
		order(.; [])
	')"
	echo "crates=${crates}" | tee -a "${GITHUB_OUTPUT}"
}

# Whether <crate> <version> is on crates.io, and whether <crate> exists at all.
#
# Usage: crates.sh state <crate> <version>
cmd_state() {
	: "${GITHUB_OUTPUT:?GITHUB_OUTPUT required}"
	local crate="${1:?usage: crates.sh state <crate> <version>}"
	local version="${2:?usage: crates.sh state <crate> <version>}"
	local code published new

	code="$(status "${crate}/${version}")"
	case "${code}" in
		200) published=true ;;
		404) published=false ;;
		*)
			echo "error: crates.io returned HTTP ${code} for ${crate} ${version}" >&2
			exit 1
			;;
	esac

	code="$(status "${crate}")"
	case "${code}" in
		200) new=false ;;
		404) new=true ;;
		*)
			echo "error: crates.io returned HTTP ${code} for ${crate}" >&2
			exit 1
			;;
	esac

	printf 'published=%s\nnew=%s\n' "${published}" "${new}" | tee -a "${GITHUB_OUTPUT}"
}

# Publish one crate, waiting out crates.io rate limits.
#
# Usage: crates.sh publish <crate> <version>
# Requires: CARGO_REGISTRY_TOKEN.
cmd_publish() {
	local crate="${1:?usage: crates.sh publish <crate> <version>}"
	local version="${2:?usage: crates.sh publish <crate> <version>}"
	if [[ -z "${CARGO_REGISTRY_TOKEN-}" ]]; then
		echo "error: no crates.io token for ${crate}; a new crate needs the crates-io environment's registry token" >&2
		exit 1
	fi

	local attempt output code retry_at wait
	for attempt in 1 2 3 4 5; do
		echo "publish: ${crate} ${version} (attempt ${attempt}/5)"
		if output="$(cargo publish --locked --allow-dirty -p "${crate}" 2>&1)"; then
			printf '%s\n' "${output}"
			return 0
		fi
		printf '%s\n' "${output}"

		code="$(status "${crate}/${version}")"
		if [[ "${code}" == 200 ]]; then
			echo "ok: ${crate} ${version} is on crates.io"
			return 0
		fi
		# crates.io's 429 body: "... try again after Mon, 28 Sep 2026 00:00:00 GMT".
		if [[ ! "${output}" =~ try\ again\ after\ ([A-Za-z]{3},\ [0-9]{2}\ [A-Za-z]{3}\ [0-9]{4}\ [0-9:]{8}\ GMT) ]]; then
			exit 1
		fi
		retry_at="${BASH_REMATCH[1]}"
		wait=$(($(date -u -d "${retry_at}" +%s) - $(date -u +%s) + 5))
		if ((wait > 0)); then
			echo "wait: rate limited until ${retry_at} (${wait}s)"
			sleep "${wait}"
		fi
	done

	echo "error: ${crate} ${version} is still rate limited after 5 attempts" >&2
	exit 1
}

case "${1-}" in
	order)
		shift
		cmd_order "$@"
		;;
	state)
		shift
		cmd_state "$@"
		;;
	publish)
		shift
		cmd_publish "$@"
		;;
	*)
		echo "usage: ${0##*/} order | state <crate> <version> | publish <crate> <version>" >&2
		exit 2
		;;
esac
