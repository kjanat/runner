#!/usr/bin/env bash
#
# Assemble (and optionally GPG-sign) the static apt repository served at
# https://$APT_DOMAIN, the index for `apt-get install runner-run`.
#
# Idempotent and stateless: the .deb files in $DEB_DIR are copied into pool/,
# then the whole dists/ index is regenerated from whatever pool/ now holds.
# Point $APT_REPO_DIR at a checkout of the published branch and pool/ keeps
# every past release, so old versions stay installable (apt install
# runner-run=<ver>). dpkg-scanpackages -m lists all versions, not just newest.
#
# Layout produced (under $APT_REPO_DIR), matching `deb https://$APT_DOMAIN
# $APT_SUITE $APT_COMPONENT`:
#   pool/<component>/r/runner-run/runner-run_<ver>_<arch>.deb
#   dists/<suite>/Release{,.gpg}, InRelease
#   dists/<suite>/<component>/binary-<arch>/{Packages,Packages.gz,Packages.xz,Release}
#   runner-run.gpg / runner-run.asc   (public key, for `signed-by=`)
#   CNAME, .nojekyll, index.html
#
# Env:
#   DEB_DIR        dir containing the *.deb to add. Required.
#   APT_REPO_DIR   repo working tree (pre-seeded from the published branch).
#                  Required.
#   APT_DOMAIN     custom domain, e.g. apt.runner.kjanat.dev. Required.
#   APT_SUITE      distribution. Default: stable
#   APT_COMPONENT  component. Default: main
#   APT_ARCHES     space-separated dpkg arches. Default: "amd64 arm64 armhf"
#   APT_ORIGIN     Release Origin/Label. Default: runner-run
#   APT_SIGN       "true" to sign Release. Default: false
#   GPG_KEY_ID     signing key id/fingerprint (required when APT_SIGN=true).
#   GPG_PASSPHRASE optional passphrase for the signing key (loopback pinentry).
set -euo pipefail
export LC_ALL=C

DEB_DIR="${DEB_DIR:?DEB_DIR required (dir containing *.deb)}"
APT_REPO_DIR="${APT_REPO_DIR:?APT_REPO_DIR required (repo working tree)}"
APT_DOMAIN="${APT_DOMAIN:?APT_DOMAIN required (e.g. apt.runner.kjanat.dev)}"
APT_SUITE="${APT_SUITE:-stable}"
APT_COMPONENT="${APT_COMPONENT:-main}"
APT_ARCHES="${APT_ARCHES:-amd64 arm64 armhf}"
APT_ORIGIN="${APT_ORIGIN:-runner-run}"
APT_SIGN="${APT_SIGN:-false}"
GPG_KEY_ID="${GPG_KEY_ID:-}"
GPG_PASSPHRASE="${GPG_PASSPHRASE:-}"

for tool in dpkg-scanpackages apt-ftparchive; do
	if ! command -v "${tool}" >/dev/null 2>&1; then
		echo "error: required tool '${tool}' not found (install dpkg-dev / apt-utils)" >&2
		exit 1
	fi
done

shopt -s nullglob
debs=("${DEB_DIR}"/*.deb)
shopt -u nullglob
if [[ "${#debs[@]}" -eq 0 ]]; then
	echo "error: no .deb files in ${DEB_DIR}" >&2
	exit 1
fi

pool="pool/${APT_COMPONENT}/r/runner-run"
dist="dists/${APT_SUITE}"

# dpkg-scanpackages override: pins Priority/Section in the index and silences
# its "missing from override file" warning. Absolute path so it survives the cd.
override="$(mktemp)"
trap 'rm -f "${override}"' EXIT
printf 'runner-run optional devel\n' >"${override}"

mkdir -p "${APT_REPO_DIR}/${pool}"
cp -f "${debs[@]}" "${APT_REPO_DIR}/${pool}/"

cd "${APT_REPO_DIR}"

# Rebuild the index from scratch so a removed/replaced pool file can never
# leave a dangling entry behind.
rm -rf "${dist}"

for arch in ${APT_ARCHES}; do
	bindir="${dist}/${APT_COMPONENT}/binary-${arch}"
	mkdir -p "${bindir}"
	# Run from the repo root so Filename: is `pool/...` (apt resolves it
	# relative to the repo root, i.e. the dir holding dists/ and pool/).
	dpkg-scanpackages -m -a "${arch}" "pool" "${override}" >"${bindir}/Packages"
	gzip -9nk -f "${bindir}/Packages"
	xz -9k -f "${bindir}/Packages"
	cat >"${bindir}/Release" <<-EOF
		Archive: ${APT_SUITE}
		Suite: ${APT_SUITE}
		Component: ${APT_COMPONENT}
		Origin: ${APT_ORIGIN}
		Label: ${APT_ORIGIN}
		Architecture: ${arch}
	EOF
done

# Top-level Release with checksums over every file under dists/<suite>.
apt-ftparchive \
	-o "APT::FTPArchive::Release::Origin=${APT_ORIGIN}" \
	-o "APT::FTPArchive::Release::Label=${APT_ORIGIN}" \
	-o "APT::FTPArchive::Release::Suite=${APT_SUITE}" \
	-o "APT::FTPArchive::Release::Codename=${APT_SUITE}" \
	-o "APT::FTPArchive::Release::Components=${APT_COMPONENT}" \
	-o "APT::FTPArchive::Release::Architectures=${APT_ARCHES}" \
	-o "APT::FTPArchive::Release::Description=runner-run apt repository" \
	release "${dist}" >"${dist}/Release"

if [[ "${APT_SIGN}" == "true" ]]; then
	if [[ -z "${GPG_KEY_ID}" ]]; then
		echo "error: APT_SIGN=true but GPG_KEY_ID is empty" >&2
		exit 1
	fi
	gpg_opts=(--batch --yes --armor --local-user "${GPG_KEY_ID}")
	if [[ -n "${GPG_PASSPHRASE}" ]]; then
		gpg_opts+=(--pinentry-mode loopback --passphrase "${GPG_PASSPHRASE}")
	fi
	# Inline-signed (InRelease) + detached (Release.gpg) so both old and new
	# apt acquisition paths verify.
	gpg "${gpg_opts[@]}" --clearsign --output "${dist}/InRelease" "${dist}/Release"
	gpg "${gpg_opts[@]}" --detach-sign --output "${dist}/Release.gpg" "${dist}/Release"
	# Publish the public key both armored (human-inspectable) and dearmored
	# (the form `signed-by=/etc/apt/keyrings/runner-run.gpg` expects).
	gpg --armor --export "${GPG_KEY_ID}" >"runner-run.asc"
	gpg --export "${GPG_KEY_ID}" >"runner-run.gpg"
else
	echo "note: APT_SIGN!=true — repository left UNSIGNED (apt will refuse it)" >&2
fi

# GitHub Pages plumbing: custom domain + skip Jekyll (which would drop files
# whose names start with '_' or '.').
printf '%s\n' "${APT_DOMAIN}" >CNAME
: >.nojekyll

cat >index.html <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>runner-run apt repository</title>
<style>body{font:16px/1.6 system-ui,sans-serif;max-width:46rem;margin:3rem auto;padding:0 1rem}code,pre{background:#f4f4f5;border-radius:6px}pre{padding:1rem;overflow:auto}code{padding:.1rem .3rem}a{color:#2563eb}</style>
</head>
<body>
<h1>runner-run apt repository</h1>
<p><a href="https://runner.kjanat.dev">runner</a> is a universal project task runner. Install it on Debian/Ubuntu and derivatives:</p>
<pre><code>sudo install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://${APT_DOMAIN}/runner-run.gpg | sudo tee /etc/apt/keyrings/runner-run.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/runner-run.gpg] https://${APT_DOMAIN} ${APT_SUITE} ${APT_COMPONENT}" \\
  | sudo tee /etc/apt/sources.list.d/runner-run.list >/dev/null
sudo apt-get update
sudo apt-get install runner-run</code></pre>
<p>Architectures: ${APT_ARCHES}. Source &amp; other install channels:
<a href="https://github.com/kjanat/runner">github.com/kjanat/runner</a>.</p>
</body>
</html>
EOF

echo "--- apt repository assembled at ${APT_REPO_DIR} ---"
find "${dist}" -type f | sort
echo "pool:"
find "pool" -type f | sort
