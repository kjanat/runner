# Host quiet support

`host.diagnostics` may suppress only host-owned diagnostics. It never suppresses
task stdout or stderr. Unsupported requests clamp to the strongest safe mode and
appear in `--explain` with the matrix ID below.

## Safe Reductions

| ID             | Invocation    | Host control                                         | Affected output                          | Task streams | Safe mapping | Evidence / caveat                                                                                           |
| -------------- | ------------- | ---------------------------------------------------- | ---------------------------------------- | ------------ | ------------ | ----------------------------------------------------------------------------------------------------------- |
| `npm`          | `npm run`     | `--silent`; `npm_config_loglevel=silent`             | npm lifecycle banner/logs                | preserved    | `quiet`      | [npm logging](https://docs.npmjs.com/cli/v11/using-npm/logging/#loglevel)                                   |
| `pnpm`         | `pnpm run`    | `--silent`; `loglevel=silent`                        | pnpm reporter                            | preserved    | `quiet`      | [pnpm loglevel](https://pnpm.io/settings/cli#loglevel)                                                      |
| `yarn-classic` | `yarn <task>` | `--silent`                                           | Yarn Classic banner                      | preserved    | `quiet`      | Classic only; Berry rejects/does not expose this global flag                                                |
| `bun`          | `bun run`     | `--silent`; `bunfig.toml [run] silent`               | command echo                             | preserved    | `quiet`      | [Bun `run.silent`](https://bun.com/docs/runtime/bunfig#run-silent-suppress-reporting-the-command-being-run) |
| `deno`         | `deno task`   | `-q`                                                 | Deno diagnostics                         | preserved    | `quiet`      | Supported by Deno CLI parser; behavior remains version-sensitive                                            |
| `make`         | `make`        | `-s` / `--silent` / `--quiet`                        | recipe echo                              | preserved    | `quiet`      | [GNU Make options](https://www.gnu.org/software/make/manual/html_node/Options-Summary.html)                 |
| `go-task`      | `task`        | `-s` / `--silent`; `TASK_SILENT`; Taskfile `silent:` | command echo                             | preserved    | `quiet`      | [Task CLI](https://taskfile.dev/docs/reference/cli#-s---silent)                                             |
| `cargo`        | Cargo alias   | `-q` / `--quiet`; `term.quiet`                       | Cargo status messages                    | preserved    | `quiet`      | [Cargo display options](https://doc.rust-lang.org/cargo/commands/cargo.html#display-options)                |
| `mise`         | `mise run`    | `--quiet`; `[settings] task.quiet`; per-task `quiet` | mise task status                         | preserved    | `quiet`      | Never use `--silent`/silent output mode: it suppresses task output                                          |
| `uv`           | `uv run`      | one `--quiet`                                        | uv diagnostics                           | preserved    | `quiet`      | Repeated `-q` is not forwarded                                                                              |
| `poetry`       | `poetry run`  | `--quiet`                                            | Poetry diagnostics                       | preserved    | `quiet`      | Poetry global option                                                                                        |
| `pipenv`       | `pipenv run`  | `--quiet`; `PIPENV_QUIET`                            | Pipenv diagnostics such as dotenv notice | preserved    | `quiet`      | Pipenv global option                                                                                        |

## Stream Routing

| ID     | Control                     | Effect                                                        | Policy axis                               |
| ------ | --------------------------- | ------------------------------------------------------------- | ----------------------------------------- |
| `pnpm` | `--use-stderr`; `useStderr` | routes all pnpm output to stderr; does not reduce diagnostics | `--host-stream`, never `host.diagnostics` |

## Unsupported Or Excluded

| ID           | Existing control          | Why excluded                                    | Fallback |
| ------------ | ------------------------- | ----------------------------------------------- | -------- |
| `turbo`      | `--output-logs`           | suppresses task logs                            | `normal` |
| `just`       | `--quiet`                 | suppresses recipe output                        | `normal` |
| `node`       | none                      | `node --run` has no host-only diagnostic switch | `normal` |
| `go`         | none                      | `go run` has no host-only diagnostic switch     | `normal` |
| `bacon`      | TUI/output controls       | no host-only quiet contract                     | `normal` |
| `yarn-berry` | no `--silent` global flag | Classic mapping is unsafe on Berry              | `normal` |
| `python`     | `-q`                      | suppresses only the interactive startup banner  | `normal` |

## Local Files

`runner ./script.ts` and its siblings run the file through a runtime
(`bun`, `deno run`, `node`, `uv run`, `python3`, `go run`). That path applies no
host quiet flags on any runtime and `--explain` prints no `host:` line for it.
Per-task stdout/stderr discard still applies and is reported on the `output:`
line.

`nx`, `volta`, `cargo_pm`, `composer`, `bundler`, `git`, `files`, `shell`,
`deno_exec`, `program`, `passthrough`, `workspace`, and `test_support` are
detection, install, shim, helper, or in-process modules rather than direct
task-dispatch hosts. They therefore do not declare a host diagnostic reduction.
In-process Deno execution still honors explicit per-task stdout/stderr discard.
