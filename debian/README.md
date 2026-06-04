# Debian / apt packaging

One binary package, `runner-run`, built for three Debian architectures from
the prebuilt GitHub release binaries (glibc, dynamically linked) — the same
`runner-v<ver>-<rust-triple>.tar.gz` assets the AUR `-bin` and npm channels
repackage, so the shipped binaries are byte-for-byte the released ones.

| Debian arch | Rust triple                     |
| ----------- | ------------------------------- |
| `amd64`     | `x86_64-unknown-linux-gnu`      |
| `arm64`     | `aarch64-unknown-linux-gnu`     |
| `armhf`     | `armv7-unknown-linux-gnueabihf` |

Each package installs the `runner` and `run` binaries plus bash, zsh, and fish
completions into the canonical system autoload dirs (bash at
`/usr/share/bash-completion/completions/{runner,run}`, zsh at
`/usr/share/zsh/site-functions/{_runner,_run}`, fish at
`/usr/share/fish/vendor_completions.d/{runner,run}.fish`) — no `eval` line in a
user's rc. The PowerShell script has no autoload convention on Linux, so it is
installed at `/usr/share/runner/runner.ps1` to dot-source from `$PROFILE`.
Completions are clap-dynamic — the shell shells out to the binary for
candidates, so tab-completing in a project picks up the *current* task list.

## Install

### apt repository (recommended)

Signed repo at <https://apt.runner.kjanat.dev>; `apt-get upgrade` then tracks
new releases:

```sh
sudo install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://apt.runner.kjanat.dev/runner-run.gpg \
  | sudo tee /etc/apt/keyrings/runner-run.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/runner-run.gpg] https://apt.runner.kjanat.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/runner-run.list >/dev/null
sudo apt-get update
sudo apt-get install runner-run
```

### Direct .deb

Every release also attaches the `.deb` files as assets:

```sh
ver=0.12.0 arch=amd64   # arch ∈ amd64 arm64 armhf
curl -fsSLO "https://github.com/kjanat/runner/releases/download/v${ver}/runner-run_${ver}_${arch}.deb"
sudo apt install "./runner-run_${ver}_${arch}.deb"
```

## Automation

`.github/workflows/debian-release.yml` runs on every `release: published` event
(and via manual `workflow_dispatch` with a `tag` input and a `dry-run`
toggle). It has two jobs:

1. **`build-deb`** repackages the release tarballs into the three `.debs`
   (`.github/scripts/build/deb-download.sh` + `deb-build.sh`), lints them with
   `lintian --fail-on error`, attaches them to the GitHub release, and uploads
   them as a workflow artifact. No secrets — always runs.
2. **`publish-apt`** assembles the static apt repo from that artifact, GPG-signs
   `Release` (`InRelease` + detached `Release.gpg`), and pushes it to the
   `gh-pages` branch (`.github/scripts/publish/apt-repo.sh`). `pool/` is fetched
   from the existing branch first, so every past release stays installable
   (`apt install runner-run=<ver>`). Gated behind the `apt` GitHub Environment
   and **inert until `APT_GPG_PRIVATE_KEY` is set** — the `.debs` ship either
   way.

The checked-in `debian/control.in` is a template: `deb-build.sh` substitutes
`@VERSION@` / `@ARCH@` / `@INSTALLED_SIZE@` per package.

### Version mapping

`vX.Y.Z` → `X.Y.Z`. A semver prerelease (`-rc.1`) becomes a Debian `~`
prerelease (`0.13.0~rc.1`) so it sorts *before* the final release. Packages are
native (no Debian revision) since upstream owns the packaging.

## Maintainer setup (one-time)

1. **Signing key** — generate an ed25519 key and export the armored secret:

   ```sh
   gpg --batch --passphrase '' \
     --quick-generate-key 'runner-run apt repository <info@kajkowalski.nl>' ed25519 sign never
   gpg --armor --export-secret-keys 'runner-run apt repository <info@kajkowalski.nl>'
   ```

   Paste the armored secret into a repo **Environment** named `apt` as the
   secret `APT_GPG_PRIVATE_KEY` (add `APT_GPG_PASSPHRASE` too if the key has
   one). The public key is published automatically at
   `https://apt.runner.kjanat.dev/runner-run.gpg`.
2. **GitHub Pages** — after the first publish creates `gh-pages`, set Settings →
   Pages → *Deploy from a branch* → `gh-pages` / root.
3. **Custom domain** — DNS `apt.runner.kjanat.dev` → `CNAME kjanat.github.io`
   (done). The workflow writes the `CNAME` file into `gh-pages`, so Pages serves
   the domain and provisions HTTPS automatically.

## Validation

- **Dry run** (no upload, no push): Actions → `debian-release` → Run workflow,
  set `tag` and tick `dry-run`. Builds + signs everything and prints the tree
  without touching the release or `gh-pages`.
- **Local build + lint** (on a Debian/Ubuntu box, from repo root):

  ```sh
  RELEASE_TAG=v0.12.0 GITHUB_REPOSITORY=kjanat/runner \
    bash .github/scripts/build/deb-download.sh
  VERSION=0.12.0 bash .github/scripts/build/deb-build.sh
  lintian --fail-on error debian/.work/out/*.deb
  ```
