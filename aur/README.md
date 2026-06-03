# AUR packaging

Two packages on the [AUR](https://aur.archlinux.org/):

| Package          | Builds                            | Arches                  |
| ---------------- | --------------------------------- | ----------------------- |
| `runner-run`     | from source via `cargo`           | x86_64, aarch64         |
| `runner-run-bin` | prebuilt from GitHub release tars | x86_64, aarch64, armv7h |

`runner-run-bin` `provides`/`conflicts` `runner-run`, so it is a drop-in
replacement — install whichever you prefer, not both.

Both packages install bash, zsh, and fish completion files for `runner`
and `run` into the system autoload dirs — no `eval` line needed in a
user's rc. The completions are clap-dynamic (the shell shells out to
the binary for candidates), so tab-completing in a project picks up the
*current* task list from `package.json` / `turbo.json` / `Justfile` /
etc., not a static snapshot.

PowerShell has no system autoload dir on Linux. The pwsh script is
installed at `/usr/share/runner/runner.ps1` for users to dot-source
from their `$PROFILE`:

```powershell
if (Test-Path /usr/share/runner/runner.ps1) { . /usr/share/runner/runner.ps1 }
```

## Automation

`.github/workflows/aur-release.yml` publishes both on every `release:
published` event (and via manual `workflow_dispatch` with a `tag` input).
Per release it:

1. Rewrites `pkgver` → release version and `pkgrel` → 1 in each `PKGBUILD`
   (`.github/scripts/publish/aur-prepare.sh`).
2. For `-bin`, injects per-arch `sha256sums_*` read from the release's
   published `.sha256` companion assets. For the source pkg, the deploy
   action runs `updpkgsums`.
3. Pushes via [`KSXGitHub/github-actions-deploy-aur`], which regenerates
   `.SRCINFO` and commits to `ssh://aur@aur.archlinux.org/<pkgname>.git`.

The checked-in `PKGBUILD` values are a reference snapshot; CI overwrites
them before pushing, so they need not be bumped by hand.

[`KSXGitHub/github-actions-deploy-aur`]: https://github.com/KSXGitHub/github-actions-deploy-aur

## Validation

Cut a release as usual, or dry-run first:

- **Validate without pushing**: Actions → `aur-release` → Run workflow,
  set `tag` and tick `dry-run`. This runs `aur-prepare.sh` and prints the
  finalized `PKGBUILD`s without touching the AUR.
- **Local lint** (on an Arch box, from repo root):

  ```bash
  cd aur/runner-run-bin \
  && updpkgsums \
  && makepkg --printsrcinfo >/dev/null \
  && namcap PKGBUILD
  ```
