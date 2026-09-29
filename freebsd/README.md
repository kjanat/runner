# FreeBSD packaging

A single prebuilt [FreeBSD port][ports] under `runner/`:

| Port         | Installs                          | Arches         |
| ------------ | --------------------------------- | -------------- |
| `runner-bin` | prebuilt from GitHub release tars | amd64, aarch64 |

It mirrors the AUR `runner-run-bin` package: no compile, just the upstream
release binaries (`runner` + `run`) plus bash, zsh and fish completions.
The release tarballs are the same `*-unknown-freebsd` assets that the main
`release.yml` build matrix already publishes (see the `freebsd-x64` /
`freebsd-arm64` entries in `npm/targets.json`), so this port reuses them
rather than recompiling.

There is no FreeBSD equivalent of the AUR's push-to-git remote — the
official Ports Collection lands through Bugzilla review. So the
self-serviceable channel is a binary `.pkg` attached to each GitHub
release, installable with `pkg add`:

```sh
pkg add https://github.com/kjanat/runner/releases/latest/download/runner-freebsd-amd64.pkg
```

Completions auto-load from the canonical `${LOCALBASE}` dirs (no `eval`
line needed in a user's rc):

- bash: `${LOCALBASE}/share/bash-completion/completions/{runner,run}`
- zsh: `${LOCALBASE}/share/zsh/site-functions/{_runner,_run}`
- fish: `${LOCALBASE}/share/fish/vendor_completions.d/{runner,run}.fish`

PowerShell has no system autoload dir, so the pwsh script is installed at
`${LOCALBASE}/share/runner/runner.ps1` for users to dot-source from their
`$PROFILE`:

```powershell
if (Test-Path /usr/local/share/runner/runner.ps1) { . /usr/local/share/runner/runner.ps1 }
```

Completions are clap-dynamic (the shell shells out to the binary for
candidates), so tab-completing in a project picks up the *current* task
list from `package.json` / `turbo.json` / `Justfile` / etc., not a static
snapshot.

## Automation

`.github/workflows/freebsd-release.yml` builds and attaches the `.pkg` on
every `release: published` event (and via manual `workflow_dispatch` with
a `tag` input). Per release it:

1. Rewrites `DISTVERSION` in the `Makefile` and regenerates `distinfo`
   (per-arch `SHA256` + `SIZE`) from the release's published `.sha256`
   companion assets and asset metadata
   (`.github/scripts/publish/freebsd-prepare.sh`).
2. Inside a FreeBSD VM ([`vmactions/freebsd-vm`]) builds the port through
   the standard ports framework (`make package`) against a sparse checkout
   of the ports `Mk/` infrastructure. `make package` runs the staging QA
   checks and the plist check, so a drifted `PLIST_FILES` fails the build
   loudly instead of shipping a broken package.
3. Uploads the resulting amd64 `.pkg` to the GitHub release as
   `runner-freebsd-amd64.pkg` (+ a `.sha256` companion).

The checked-in `Makefile` / `distinfo` values are a reference snapshot; CI
overwrites them before building, so they need not be bumped by hand.

aarch64 users build the port locally — `distinfo` already carries the
aarch64 distfile checksum, so `make package` on an aarch64 host produces
the matching `.pkg` with no edits.

## Validation

Cut a release as usual, or dry-run first:

- **Validate without uploading**: Actions → `freebsd-release` → Run
  workflow, set `tag` and tick `dry-run`. This prepares the port and runs
  the full `make package` build in the FreeBSD VM, but skips the release
  upload.
- **Local build** (on a FreeBSD box, with the port dropped into a ports
  tree at `devel/runner`):

  ```sh
  cd /usr/ports/devel/runner && make stage check-plist package
  ```

[ports]: https://docs.freebsd.org/en/books/porters-handbook/
[`vmactions/freebsd-vm`]: https://github.com/vmactions/freebsd-vm
