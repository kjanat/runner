#!/usr/bin/env bash
#
# Build the Debian .deb packages for runner-run from the prebuilt release
# tarballs (the same `runner-v<ver>-<rust-triple>.tar.gz` assets the GitHub
# release publishes). No compilation happens here: we repackage the exact
# released binaries byte-for-byte, the way the AUR `-bin` and npm channels do.
#
# Debian arch  <-  Rust triple                       <-  glibc, dynamically linked
#   amd64          x86_64-unknown-linux-gnu
#   arm64          aarch64-unknown-linux-gnu
#   armhf          armv7-unknown-linux-gnueabihf
#
# Shell completions are arch-independent (they only embed the installed
# /usr/bin path), so they are generated once from the amd64 binary and shipped
# in all three packages. This means the script must run on an amd64 host (it
# does: release.yml routes it to ubuntu-latest, matching local dev boxes).
#
# Env:
#   VERSION            upstream version, no leading v (e.g. 0.12.0). Required.
#   DEB_DOWNLOAD_DIR   dir holding runner-v$VERSION-<triple>.tar.gz + .sha256.
#                      Default: debian/.work/download
#   DEB_OUT_DIR        output dir for the finished .deb files.
#                      Default: debian/.work/out
#   REPO_ROOT          repo checkout root (control.in, copyright, README.md).
#                      Default: `git rev-parse --show-toplevel`, else $PWD.
#   DEB_SKIP_CHECKSUM  set to 1 to skip .sha256 verification (local dev only).
set -euo pipefail
export LC_ALL=C

VERSION="${VERSION:?VERSION required (upstream version without leading v)}"
DEB_DOWNLOAD_DIR="${DEB_DOWNLOAD_DIR:-debian/.work/download}"
DEB_OUT_DIR="${DEB_OUT_DIR:-debian/.work/out}"
REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
DEB_SKIP_CHECKSUM="${DEB_SKIP_CHECKSUM:-0}"

# Reject anything that isn't strict semver before it reaches the `sed`
# substitutions below — keeps the alphabet to [0-9A-Za-z.-] so the rewrite of
# @VERSION@ into the control file is byte-for-byte literal (same guard as
# aur-prepare.sh). A leading `v`, `&`, `/`, `\`, or a newline is refused here.
if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
	echo "error: VERSION '${VERSION}' is not strict semver (X.Y.Z or X.Y.Z-prerelease)" >&2
	exit 1
fi

# Debian version ordering: a semver prerelease (`-rc.1`) must sort *before* the
# final release, which Debian expresses with `~`, not `-`. 0.12.0 -> 0.12.0;
# 0.13.0-rc.1 -> 0.13.0~rc.1.
DEB_VERSION="${VERSION//-/\~}"

# deb_arch:rust_triple, amd64 first so its extract is ready for completions.
ARCHES=(
	"amd64:x86_64-unknown-linux-gnu"
	"arm64:aarch64-unknown-linux-gnu"
	"armhf:armv7-unknown-linux-gnueabihf"
)

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
COMPL_DIR="${WORK}/completions"
# One RFC 5322 timestamp shared by every package's changelog so the three
# builds are reproducible relative to each other.
CHANGELOG_DATE="$(date -u -R)"

# verify_and_extract <rust-triple> <dest-dir>: checksum the release tarball
# against its published .sha256 companion, then extract its flat contents
# (runner, run, README.md, LICENSE) into <dest-dir>.
verify_and_extract() {
	local triple="$1" dest="$2"
	local tarball="runner-v${VERSION}-${triple}.tar.gz"
	local sumfile="runner-v${VERSION}-${triple}.sha256"

	if [[ ! -f "${DEB_DOWNLOAD_DIR}/${tarball}" ]]; then
		echo "error: missing ${DEB_DOWNLOAD_DIR}/${tarball}" >&2
		exit 1
	fi
	if [[ "${DEB_SKIP_CHECKSUM}" != "1" ]]; then
		if [[ ! -f "${DEB_DOWNLOAD_DIR}/${sumfile}" ]]; then
			echo "error: missing checksum ${DEB_DOWNLOAD_DIR}/${sumfile} (set DEB_SKIP_CHECKSUM=1 to bypass)" >&2
			exit 1
		fi
		(cd "${DEB_DOWNLOAD_DIR}" && sha256sum -c --status "${sumfile}")
	fi

	mkdir -p "${dest}"
	tar -xzf "${DEB_DOWNLOAD_DIR}/${tarball}" -C "${dest}"
	local b
	for b in runner run; do
		if [[ ! -f "${dest}/${b}" ]]; then
			echo "error: ${tarball} did not contain '${b}'" >&2
			exit 1
		fi
	done
}

# gen_completions <dir-with-runnable-runner+run>: emit per-shell completion
# files into $COMPL_DIR with the current_exe()-baked paths rewritten to the
# installed /usr/bin locations. `runner completions <shell>` is the only
# generator and emits a single stream covering BOTH `runner` and `run`; bash
# and zsh are split into one autoload file per command. (Identical logic to
# aur/runner-run-bin/PKGBUILD.)
gen_completions() {
	local bindir="$1"
	local runner="${bindir}/runner" run="${bindir}/run"
	mkdir -p "${COMPL_DIR}"
	"${runner}" completions bash >"${COMPL_DIR}/bash.combined"
	"${runner}" completions zsh >"${COMPL_DIR}/zsh.combined"
	"${runner}" completions fish >"${COMPL_DIR}/fish.combined"
	"${runner}" completions pwsh >"${COMPL_DIR}/runner.ps1"
	# Longer match first — `<dir>/run` is a prefix of `<dir>/runner`.
	sed -i \
		-e "s|${runner}|/usr/bin/runner|g" \
		-e "s|${run}|/usr/bin/run|g" \
		"${COMPL_DIR}/bash.combined" "${COMPL_DIR}/zsh.combined" \
		"${COMPL_DIR}/fish.combined" "${COMPL_DIR}/runner.ps1"
	awk -v r="${COMPL_DIR}/runner.bash" -v n="${COMPL_DIR}/run.bash" \
		'/^_clap_complete_run\(\) \{$/ {o=n} {print > (o?o:r)}' "${COMPL_DIR}/bash.combined"
	awk -v r="${COMPL_DIR}/_runner" -v n="${COMPL_DIR}/_run" \
		'/^#compdef run$/ {o=n} {print > (o?o:r)}' "${COMPL_DIR}/zsh.combined"
}

# gen_changelog <out.gz>: write a Debian-format changelog, gzip -9n (no name or
# timestamp header) so the data.tar member is reproducible and lintian-clean.
gen_changelog() {
	local out="$1"
	mkdir -p "$(dirname "${out}")"
	{
		echo "runner-run (${DEB_VERSION}) stable; urgency=medium"
		echo
		echo "  * Release ${VERSION}. Upstream changelog:"
		echo "    https://github.com/kjanat/runner/blob/v${VERSION}/CHANGELOG.md"
		echo
		echo " -- Kaj Kowalski <info@kajkowalski.nl>  ${CHANGELOG_DATE}"
	} | gzip -9n >"${out}"
}

# build_one <deb-arch> <bindir>: assemble the package tree and emit the .deb.
build_one() {
	local deb_arch="$1" bindir="$2"
	local pkgroot="${WORK}/pkg-${deb_arch}"
	rm -rf "${pkgroot}"

	# Binaries (already stripped by the release profile; root:root set at build).
	install -Dm0755 "${bindir}/runner" "${pkgroot}/usr/bin/runner"
	install -Dm0755 "${bindir}/run" "${pkgroot}/usr/bin/run"

	# Docs. A native package (no Debian revision) takes changelog.gz; copyright
	# is mandatory under Debian policy.
	install -Dm0644 "${REPO_ROOT}/debian/copyright" "${pkgroot}/usr/share/doc/runner-run/copyright"
	install -Dm0644 "${REPO_ROOT}/README.md" "${pkgroot}/usr/share/doc/runner-run/README.md"
	gen_changelog "${pkgroot}/usr/share/doc/runner-run/changelog.gz"

	# Document the one lintian false positive (pure-Rust yaml-rust2, not C
	# libyaml) so the package lints clean wherever it is checked.
	install -Dm0644 "${REPO_ROOT}/debian/lintian-overrides" "${pkgroot}/usr/share/lintian/overrides/runner-run"

	# Shell completions, auto-loaded from the canonical system dirs.
	install -Dm0644 "${COMPL_DIR}/runner.bash" "${pkgroot}/usr/share/bash-completion/completions/runner"
	install -Dm0644 "${COMPL_DIR}/run.bash" "${pkgroot}/usr/share/bash-completion/completions/run"
	install -Dm0644 "${COMPL_DIR}/_runner" "${pkgroot}/usr/share/zsh/site-functions/_runner"
	install -Dm0644 "${COMPL_DIR}/_run" "${pkgroot}/usr/share/zsh/site-functions/_run"
	# Fish autoloads by command basename, so ship the (identical) combined
	# stream under both names — each command's first <TAB> works in a fresh
	# shell regardless of session order.
	install -Dm0644 "${COMPL_DIR}/fish.combined" "${pkgroot}/usr/share/fish/vendor_completions.d/runner.fish"
	install -Dm0644 "${COMPL_DIR}/fish.combined" "${pkgroot}/usr/share/fish/vendor_completions.d/run.fish"
	# PowerShell has no system autoload dir on Linux — dot-source from $PROFILE.
	install -Dm0644 "${COMPL_DIR}/runner.ps1" "${pkgroot}/usr/share/runner/runner.ps1"

	# DEBIAN/control from the checked-in template.
	local installed_size
	installed_size="$(du -k -s "${pkgroot}/usr" | cut -f1)"
	mkdir -p "${pkgroot}/DEBIAN"
	sed \
		-e "s/@VERSION@/${DEB_VERSION}/" \
		-e "s/@ARCH@/${deb_arch}/" \
		-e "s/@INSTALLED_SIZE@/${installed_size}/" \
		"${REPO_ROOT}/debian/control.in" >"${pkgroot}/DEBIAN/control"

	# md5sums over the payload (paths relative to the package root, DEBIAN/
	# excluded), sorted for a deterministic control.tar member.
	(cd "${pkgroot}" && find usr -type f -print0 | sort -z | xargs -0 md5sum >DEBIAN/md5sums)

	# xz keeps the archive installable on older apt/dpkg (zstd debs need a very
	# recent toolchain). --root-owner-group stamps root:root without fakeroot.
	mkdir -p "${DEB_OUT_DIR}"
	local out="${DEB_OUT_DIR}/runner-run_${DEB_VERSION}_${deb_arch}.deb"
	dpkg-deb -Zxz --root-owner-group --build "${pkgroot}" "${out}" >/dev/null
	echo "built ${out}"
}

main() {
	if [[ ! -f "${REPO_ROOT}/debian/control.in" ]]; then
		echo "error: ${REPO_ROOT}/debian/control.in not found (wrong REPO_ROOT?)" >&2
		exit 1
	fi
	mkdir -p "${DEB_OUT_DIR}"

	# Extract every arch up front (verifies checksums), then generate the
	# shared completions from the amd64 binaries.
	local pair deb_arch triple
	declare -A extracted=()
	for pair in "${ARCHES[@]}"; do
		deb_arch="${pair%%:*}"
		triple="${pair#*:}"
		verify_and_extract "${triple}" "${WORK}/extract-${deb_arch}"
		extracted["${deb_arch}"]="${WORK}/extract-${deb_arch}"
	done
	gen_completions "${extracted[amd64]}"

	for pair in "${ARCHES[@]}"; do
		deb_arch="${pair%%:*}"
		build_one "${deb_arch}" "${extracted[${deb_arch}]}"
	done

	echo "--- packages in ${DEB_OUT_DIR} ---"
	ls -la "${DEB_OUT_DIR}"/*.deb
}

main "$@"
