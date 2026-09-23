# Contributing

## Running the working-tree build

The `bin/` shims always run the current source, not the installed binary:

```sh
./bin/runner <args>
./bin/run <args>
```

With `direnv` (`.envrc` puts `bin/` on `PATH`), plain `runner` / `run` resolve
to the working-tree build.

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D clippy::all
dprint check        # or: cargo fmt --check
```

Cargo aliases live in `.cargo/config.toml` (`t`, `cl`, `l`/`lint`, …).

## Generated artifacts

Both are rendered from the CLI definition, never hand-edited.

- **JSON Schemas**, committed under `schemas/`:

  ```sh
  just gen-schema                   # cargo schema --all --output schemas
  git diff --exit-code schemas/     # drift guard
  ```

- **Man pages** (`man` feature), generated at release time, not committed:

  ```sh
  cargo man                        # → stdout
  cargo man -o packaging/man       # → ./packaging/man (gitignored)
  ```
