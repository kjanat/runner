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
cargo test
cargo clippy --all-targets --all-features -- -D clippy::all
dprint check        # or: cargo fmt --check
```

Cargo aliases live in `.cargo/config.toml` (`t`, `cl`, `l`/`lint`, …).

## Generated artifacts

Both are rendered from the CLI definition behind off-by-default features,
never hand-edited.

- **`runner.toml` JSON Schema**, committed under `schemas/`:

  ```sh
  just gen-schema                  # cargo schema --output schemas/runner.toml.schema.json
  git diff --exit-code schemas/    # drift guard
  ```

- **Man pages** (`man`/`schema` features), generated at release time, not
  committed:

  ```sh
  cargo man                        # → stdout
  cargo man -o man                 # → ./man (gitignored)
  ```
