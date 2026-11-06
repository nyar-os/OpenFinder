# OpenFinder

Fast NTFS file search. The index is built by reading `$MFT` records directly and
kept current with the USN journal. Directory traversal is never used as an
implicit fallback: the portable walk is a separate, explicitly labelled mode.

## Layout

| Path | Responsibility |
|------|----------------|
| `crates/core` | NTFS record parsing, index, query, USN updates, capability states, install planning |
| `crates/napi` | Platform-neutral N-API surface over the `wae` facade |
| `packages/open-finder` | Meta package, typed library, and the `open-finder` CLI |
| `packages/open-finder-ui` | VMZ search page, design tokens, and GUI staging |
| `packages/open-finder-skills` | Agent skill with command and safety guidance |
| `packages/open-finder-win32-x64` | Native platform package (`os` / `cpu` / `optionalDependencies`) |
| `scripts/build-napi.ts` | Build the native addon and resolve its artifact |
| `scripts/stage-native.ts` | Copy the artifact into the platform package |
| `scripts/check-skills.ts` | Validate skill frontmatter and every CLI command and flag |

## Index storage

The index is stored in a directory with two files:

```text
<meta-dir>/meta.json        schema, volume, geometry, USN cursor, counts
<meta-dir>/entries.ndjson   one JSON entry per line, keyed by FRN
```

Each entry carries `frn`, `parentFrn`, `name`, `isDir`, `size`, `createdMs`,
`modifiedMs`, and `accessedMs`. Full paths are rebuilt from parent links at query
time, so renames only need one entry updated instead of every descendant.

Schema id: `open-finder.index/v0`. A different schema is rejected with a message
that asks for a rebuild.

Resolution order for the directory:

1. `--index-dir <path>` on `index` and `search`
2. `OPEN_FINDER_INDEX_DIR`
3. `config.json#indexDir`
4. `<config-dir>/index`

`<config-dir>` is `%APPDATA%\OpenFinder` on Windows and
`$HOME/.config/open-finder` elsewhere. `open-finder version --json` prints the
resolved directory.

## Commands

```sh
open-finder capabilities --json
open-finder index --volume C: --dry-run --json    # geometry and journal state only
open-finder index --volume C: --actor <id> --json # full $MFT build
open-finder index --update --max-records 200000 --json
open-finder search --pattern "*.dll" --path-filter "\\Windows" --sort size --desc --json
open-finder search --regex --pattern "^setup.*\\.exe$" --limit 20 --offset 20 --json
open-finder search --mode walk --root <path> --pattern "*.log" --json
open-finder audit --kind agent --limit 20 --json
open-finder version --json
```

Exit codes: `0` success, `1` runtime failure, `2` usage, `3` capability
unavailable.

Search supports glob (`*`, `?`), regex, case-insensitive path filtering, size
and modified-time ranges, kind (`file` / `dir`), sorting by `name` / `path` /
`size` / `modified`, and paging (`--limit`, `--offset`). Pages are capped at 1000
hits; `total` and `hasMore` describe the full result set. A portable walk that
stops at `--max-entries` sets `truncated: true`, and its `total` then counts the
entries it inspected.

## Platform boundary

| Capability | Windows | macOS / Linux |
|------------|---------|---------------|
| `index-build` | available, reads `$MFT` through a raw volume handle | `unsupported_platform` |
| `index-update` | available, reads the USN journal | `unsupported_platform` |
| `search` | available over an existing index | available over an existing index |
| `portable-walk` | available, explicitly non-indexed | available, explicitly non-indexed |

Indexing needs an elevated process. A denied volume handle is reported as
`dependency_blocked` with the Win32 error, never as an empty index. The three
failure states are `unsupported_platform`, `not_implemented`, and
`dependency_blocked`.

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

`build:napi` writes `dist/napi/<name>` and `stage-native` copies the addon into
the platform package. Keep `CARGO_BUILD_JOBS=2` when running cargo and do not run
another build in parallel with it.

## Audit

Every exported capability is wrapped at the N-API boundary through
`wae::audit` (`wae.audit/v0`). The domain supplies only the capability id, target
count, target summary, and a stable error code. Actor, host, session, schema,
timestamps, duration, and sanitisation stay in the shared facility. Dry-run,
failed, and rejected calls are recorded too.

## License

Apache-2.0. See the repository-level `License.md`.
