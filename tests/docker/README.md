# Docker-based integration tests

Container tests that verify the `runner-run` npm distribution on real target
environments. The platform packages always come from the npm registry, so the
binaries these tests spawn are the ones consumers actually get.

## [`musl-resolve.Dockerfile`][musl-resolve]

Verifies that `npm i -g runner-run@<ver>` resolves and executes on **pure
Alpine/musl** (no `libc6-compat` glibc shim that would mask a failure).

Tests the published package end to end, nothing from this checkout. npm honours
the `libc` field, so the installed set should contain only
`@runner-run/linux-x64-musl`; this test makes that visible instead of letting an
alternate install path (e.g. cargo-binstall) mask it.

The build itself is the assertion; every stage `exit 1`s on regression:

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

- `--build-arg VER=<x.y.z>`: Test a specific published version (default `0.10.0`).
- `--progress=plain --no-cache`: Use these flags to see the per-stage diagnostics.
- `--network=host`: Use this only where Docker's bridge/veth setup is blocked.

Green build = the npm/musl distribution path is sound for that version.

## [`musl-libc-select.Dockerfile`][musl-libc-select]

Verifies that the facade picks the musl build on pure Alpine/musl when **both**
Linux libc variants are installed — the tree Bun and Deno produce, and the one
you get by copying `node_modules` off a glibc machine.

This one grafts the working tree's `npm/facade/lib/resolve.cjs` onto the
installed package, so the selection logic under test is the one in this
checkout. npm will not install the mismatched sibling, so its tarball is fetched
and unpacked next to the musl package by hand.

Stages, each fatal on regression:

1. confirm a pure musl environment (musl loader present, no `libc.so.6`)
2. `npm i -g runner-run@<ver>`, then overlay the local resolver
3. unpack `@runner-run/linux-<arch>-gnu` alongside the musl package
4. `detectLibc()` against the real root filesystem — must say `musl`
5. `resolveBinary()` for `runner` and `run` — must be musl with both installed
6. the resolved binaries execute
7. with the musl package removed, the facade fails with a libc diagnostic and
   never hands back the GNU path

Run from the repo root:

```sh
docker build -f tests/docker/musl-libc-select.Dockerfile \
  --build-arg VER=0.25.0 --progress=plain --no-cache -t runner-musl-libc-test .
```

The libc selection logic itself is also covered by
[`npm/facade/test/resolve.test.mts`](../../npm/facade/test/resolve.test.mts),
which stubs the host signals and needs no container. This test is what proves
the detection layers agree with a real musl root filesystem.

[musl-libc-select]: musl-libc-select.Dockerfile
[musl-resolve]: musl-resolve.Dockerfile
