# runner

The `runner` and `run` binaries, packaged for `COPY --from`.

[runner](https://github.com/kjanat/runner) is a universal project task runner. It detects
the project, finds its tasks, and completes them through one command shape,
whatever the toolchain underneath.

## Usage

This image is a file carrier, not a service. Copy the binaries into a build
stage:

```dockerfile
COPY --from=kjanat/runner:{{version}} /run /usr/local/bin/run
COPY --from=kjanat/runner:{{version}} /runner /usr/local/bin/runner
```

Same image on GHCR, if you prefer to pull from there:

```dockerfile
COPY --from=ghcr.io/kjanat/runner:{{version}} /run /usr/local/bin/run
```

The binaries are musl-static, so one tag serves Alpine and Debian stages alike.
Nothing else is required in the target stage.

It also runs directly, though that is the secondary use:

```sh
docker run --rm kjanat/runner:{{version}} --version
docker run --rm --entrypoint /run -v "$PWD:/w" -w /w kjanat/runner:{{version}} build
```

## Why

Projects whose package scripts call `run` break inside build containers. The
alternative is downloading an installer on every cache bust, which needs curl
and network access in the build stage and pins per Dockerfile instead of per
tag. A published image makes the binaries a content-addressed, cacheable build
artifact.

## Contents

`FROM scratch`, holding exactly two files:

| path      | what                   |
| --------- | ---------------------- |
| `/runner` | the full CLI           |
| `/run`    | alias for `runner run` |

`ENTRYPOINT` is `/runner`.

## Tags and platforms

Semver tags track releases: `{{version}}`, `{{minor}}`, and `latest`. Prereleases never
take `latest`.

Platforms: `linux/amd64`, `linux/arm64`.

Binaries come from the same build as the
[GitHub release](https://github.com/kjanat/runner/releases) and the
[npm platform packages](https://www.npmjs.com/package/runner-run), so a given
version is byte-identical across channels.

## Other install channels

npm, crates.io, AUR, and a shell installer are described in the
[README](https://github.com/kjanat/runner#install).

## Links

- Source and issues: <https://github.com/kjanat/runner>
- Site: <https://runner.kjanat.dev>
- GHCR mirror: <https://github.com/users/kjanat/packages/container/package/runner>

MIT © Kaj Kowalski
