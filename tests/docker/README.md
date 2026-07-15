# Docker-based integration tests

Container tests that verify the **published** `runner-run` npm distribution on
real target environments. These pull the package from the npm registry, they
do not build from local source, so they validate what consumers actually get.

## [`musl-resolve.Dockerfile`][musl-resolve]

Verifies that `npm i -g runner-run@<ver>` resolves and executes on **pure
Alpine/musl** (no `libc6-compat` glibc shim that would mask a failure).

The facade's `lib/resolve.cjs` is libc-blind, it walks `optionalDependencies`
order and returns the first sub-package whose bin exists. Correctness on musl
therefore depends entirely on the package manager having libc-filtered the
install down to `@runner-run/linux-x64-musl`. This test makes that visible
instead of letting an alternate install path (e.g. cargo-binstall) mask it.

The build itself is the assertion, every stage `exit 1`s on regression:

1. confirm musl libc environment
2. `npm i -g` + dependency tree
3. which `@runner-run/*` sub-packages npm kept (expect musl only)
4. `resolveBinary()` output for `runner` and `run` (must be musl, never `-gnu`)
5. `file` + `ldd` linkage (musl or static, never glibc)
6. binaries actually execute (`--version`, `--help`)

Run from the repo root:

```sh
docker build --network=host -f tests/docker/musl-resolve.Dockerfile \
  --build-arg VER=0.10.0 --progress=plain --no-cache -t runner-musl-test .
```

- `--build-arg VER=<x.y.z>`, test a specific published version (default `0.10.0`).
- `--progress=plain --no-cache`, required to see the per-stage diagnostics.
- `--network=host`, only needed where Docker's bridge/veth setup is blocked.

Green build = the npm/musl distribution path is sound for that version.

[musl-resolve]: musl-resolve.Dockerfile
