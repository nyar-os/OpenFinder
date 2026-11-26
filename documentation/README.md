# OpenFinder 开发者手册

## Layout

| Path | Responsibility |
|------|----------------|
| `crates/core` | NTFS record parsing, index, query, USN updates, capability states, install planning |
| `crates/napi` | Platform-neutral N-API surface over the `wae` facade |
| `packages/open-finder` | Meta package, typed library, and the `open-finder` CLI |
| `packages/open-finder-ui` | VMZ search page, design tokens, and GUI staging |
| `packages/open-finder-skills` | Agent skill with command and safety guidance |
| `packages/open-finder-win32-x64` | Native platform package (`os` / `cpu` / `optionalDependencies`) |
| `wae native build` | Build the native addon and resolve its artifact (configured in `wae.config.ts`) |
| `wae native stage` | Copy the artifact into the platform package (configured in `wae.config.ts`) |
| `scripts/check-skills.ts` | Validate skill frontmatter and every CLI command and flag |

## Index storage

```text
<meta-dir>/meta.json        schema, volume, geometry, USN cursor, counts
<meta-dir>/entries.ndjson   one JSON entry per line, keyed by FRN
```

Each entry carries `frn`, `parentFrn`, `name`, `isDir`, `size`, `createdMs`, `modifiedMs`, and `accessedMs`. Full paths are rebuilt from parent links at query time.

Schema id: `open-finder.index/v0`. A different schema is rejected with a message that asks for a rebuild.

Resolution order for the directory:

1. `--index-dir <path>` on `index` and `scan`
2. `OPEN_FINDER_INDEX_DIR`
3. `config.json#indexDir`
4. `<config-dir>/index`

`<config-dir>` is `%APPDATA%\OpenFinder` on Windows and `$HOME/.config/open-finder` elsewhere. `open-finder version --json` prints the resolved directory.

## Build and verify

```sh
pnpm install
pnpm run build:napi
pnpm run build:ts
pnpm run build:ui
open-finder capabilities --json
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`build:napi` runs `wae native build` and `stage:native` runs `wae native stage`. The native target table and artifact lookup live in `wae.config.ts`. Keep `CARGO_BUILD_JOBS=2` when running cargo and do not run another build in parallel with it.

`volumes` and `inspect` are read-only diagnostics: they report logical drives with index status, and one path's metadata, recursive size, and minimal `.git` state. They never write the index and never spawn an external tool.

## Audit

Every exported capability is wrapped at the N-API boundary through `wae::audit` (`wae.audit/v0`). The domain supplies only the capability id, target count, target summary, and a stable error code. Actor, host, session, schema, timestamps, duration, and sanitisation stay in the shared facility. Dry-run, failed, and rejected calls are recorded too.
