#!/usr/bin/env bash
# Subcommands for docker-release.yml. One script per workflow; dispatch at the bottom.

set -euo pipefail

readonly DOCKERFILE=.github/docker/Dockerfile
readonly CONTEXT=.github/docker/context
readonly README=.github/docker/README.md
readonly README_RENDERED=.github/docker/README.rendered.md

# npm package dir -> buildx TARGETARCH. musl-static serves Alpine and Debian
# stages alike, so the gnu builds have no place in the image.
readonly ARCH_MAP=(
	"linux-x64-musl:amd64"
	"linux-arm64-musl:arm64"
)

group() { echo "::group::$1"; }
endgroup() { echo "::endgroup::"; }

# Lay out npm/dist binaries as context/<TARGETARCH>/{runner,run}.
cmd_prepare() {
	rm -rf "${CONTEXT}"

	local entry pkg arch src dest bin
	for entry in "${ARCH_MAP[@]}"; do
		pkg="${entry%%:*}"
		arch="${entry##*:}"
		src="npm/dist/${pkg}/bin"
		dest="${CONTEXT}/${arch}"

		mkdir -p "${dest}"
		for bin in runner run; do
			if [[ ! -f "${src}/${bin}" ]]; then
				echo "error: ${src}/${bin} not found; the dist artifact is missing ${pkg}" >&2
				exit 1
			fi
			cp "${src}/${bin}" "${dest}/${bin}"
			chmod +x "${dest}/${bin}"
		done
	done

	group "prepared build context"
	ls -lR "${CONTEXT}"
	endgroup
}

# Build (and optionally push) the image.
#
# Required env: META (JSON object from docker/metadata-action), DRY_RUN (true does not push).
# Optional env: PLATFORMS (comma-separated; empty builds for the host arch only).
#
# Multi-platform builds need a container-driver builder; the default docker
# driver rejects them. --load is the docker exporter and cannot take a manifest
# list, so a multi-platform build that is not pushing exports nothing.
cmd_build() {
	: "${META:?META required}"
	: "${DRY_RUN:?DRY_RUN required}"

	case "${DRY_RUN}" in
		true | false) ;;
		*)
			echo "error: DRY_RUN '${DRY_RUN}' must be 'true' or 'false'" >&2
			exit 1
			;;
	esac

	local tags labels annotations
	tags="$(jq -r '.tags // empty' <<<"${META}")"
	labels="$(jq -r '.labels // empty' <<<"${META}")"
	annotations="$(jq -r '.annotations // empty' <<<"${META}")"
	: "${tags:?META.tags required}"

	local args=(buildx build --file "${DOCKERFILE}")

	local platforms="${PLATFORMS-linux/amd64,linux/arm64}"
	if [[ -n "${platforms}" ]]; then
		args+=(--platform "${platforms}")
	fi

	local tag
	while IFS= read -r tag; do
		[[ -n "${tag}" ]] && args+=(--tag "${tag}")
	done <<<"${tags}"

	local label
	while IFS= read -r label; do
		[[ -n "${label}" ]] && args+=(--label "${label}")
	done <<<"${labels}"

	# GHCR reads the description off the index annotation, not the config label.
	local annotation
	while IFS= read -r annotation; do
		[[ -n "${annotation}" ]] && args+=(--annotation "${annotation}")
	done <<<"${annotations}"

	if [[ "${DRY_RUN}" == "false" ]]; then
		args+=(--push)
	elif [[ "${platforms}" == *,* ]]; then
		args+=(--output type=cacheonly)
	else
		args+=(--load)
	fi

	group "docker ${args[*]} ${CONTEXT}"
	docker "${args[@]}" "${CONTEXT}"
	endgroup
}

# Assert the pushed manifest lists every platform, then execute both binaries
# on each. A push can succeed while publishing a short manifest, and an image
# that merely exists proves nothing about the binaries inside it.
#
# Required env: TAGS. Only the first tag is probed; they share a digest.
# Executing a non-native platform needs QEMU registered on the host.
cmd_verify() {
	: "${TAGS:?TAGS required}"

	local platforms="${PLATFORMS:-linux/amd64,linux/arm64}"
	local ref
	ref="$(head -n1 <<<"${TAGS}")"
	if [[ -z "${ref}" ]]; then
		echo "error: TAGS is empty" >&2
		exit 1
	fi

	group "manifest ${ref}"
	docker buildx imagetools inspect "${ref}"
	endgroup

	local published
	published="$(
		docker buildx imagetools inspect "${ref}" \
			--format '{{range .Manifest.Manifests}}{{.Platform.OS}}/{{.Platform.Architecture}}{{"\n"}}{{end}}'
	)"

	local platform raw=() wanted=() missing=()
	IFS=',' read -r -a raw <<<"${platforms}"
	for platform in "${raw[@]}"; do
		[[ -n "${platform}" ]] && wanted+=("${platform}")
	done

	for platform in "${wanted[@]}"; do
		grep -qxF "${platform}" <<<"${published}" || missing+=("${platform}")
	done

	if [[ "${#missing[@]}" -gt 0 ]]; then
		echo "error: ${ref} was pushed without these platforms:" >&2
		printf '  - %s\n' "${missing[@]}" >&2
		exit 1
	fi

	local bin
	for platform in "${wanted[@]}"; do
		for bin in runner run; do
			group "${platform} /${bin} --version"
			docker run --rm --pull always --platform "${platform}" \
				--entrypoint "/${bin}" "${ref}" --version
			endgroup
		done
	done
}

# Substitute the release version into the Docker Hub overview.
#
# Required env: RELEASE_TAG.
cmd_readme() {
	: "${RELEASE_TAG:?RELEASE_TAG required}"

	local version="${RELEASE_TAG#v}"
	local minor="${version%.*}"

	sed -e "s/{{version}}/${version}/g" -e "s/{{minor}}/${minor}/g" "${README}" >"${README_RENDERED}"

	if grep -q '{{' "${README_RENDERED}"; then
		echo "error: unsubstituted placeholder left in ${README_RENDERED}:" >&2
		grep -n '{{' "${README_RENDERED}" >&2
		exit 1
	fi

	group "rendered ${README_RENDERED}"
	cat "${README_RENDERED}"
	endgroup
}

case "${1-}" in
	prepare) cmd_prepare ;;
	build) cmd_build ;;
	verify) cmd_verify ;;
	readme) cmd_readme ;;
	*)
		echo "usage: ${0##*/} <prepare|build|verify|readme>" >&2
		exit 2
		;;
esac
