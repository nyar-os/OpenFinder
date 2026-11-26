---
name: workflow
description: Build or update an NTFS index and search it through the OpenFinder CLI without inventing a directory walk.
---

# OpenFinder agent workflow

This skill drives the `open-finder` CLI. The CLI is non-interactive and every
command returns a stable exit code:

- `0` success
- `1` runtime failure
- `2` usage or validation failure
- `3` structured capability failure (`unsupported_platform`, `not_implemented`,
  `dependency_blocked`)

Prefer `--json` and read the `{ "ok": true, "data": ... }` or
`{ "ok": false, "error": ... }` envelope. Do not parse human prose.

## 1. Discover capabilities

   `open-finder capabilities --json`

A capability whose `state` is not `null` is unavailable. `unsupported_platform`
means this host has no NTFS index backend and must not be worked around with a
directory walk. `dependency_blocked` means the request needs an elevated
process or a missing dependency.

## 2. Build or refresh the index

Inspect the volume first. This is read-only and needs no index write.

   `open-finder index --volume <volume> --dry-run --json`

Then build. On Windows this reads `$MFT` directly and requires an elevated
process. A permission failure is reported as `dependency_blocked`.

   `open-finder index --volume <volume> --actor <id> --json`

Apply journal changes incrementally. If the response has
`"needsRescan": true`, the journal no longer covers the saved cursor. Rebuild
with the command above instead of guessing.

   `open-finder index --update --max-records <count> --actor <id> --json`

The index lives under the user configuration directory. Use `--index-dir` or
`OPEN_FINDER_INDEX_DIR` to make the location explicit.

When the response reports `"strategy": "usn-enum"` together with
`"deletionsTracked": false`, the host rejected the journal read call and the
changes came from MFT enumeration. Deletions are then invisible, so say that
when you report results instead of implying the index is exact.

## 3. Read-only diagnostics

Answer "which volumes are indexed" and "what is this path" without PowerShell,
`wmic`, a third-party search tool, or a `git` process. Both commands only read
host state.

   `open-finder volumes --json`

Each row carries `filesystem`, `ntfs`, capacity, and `index` (`dir`, `present`,
`entryCount`, `cursor`). `index.present: false` means that volume has no index;
build one with step 2 before scanning it.

   `open-finder inspect --path <path> --json`

`inspect` reports `exists`, `kind`, `size`, Windows `attributes`, a symlink or
junction `linkTarget`, and, for a directory, recursive `tree` totals. `--shallow`
skips the tree walk and `--max-entries <n>` caps it; when the cap stops the walk,
`tree.truncated` is true.

When the path is inside a git checkout, `repo` carries `root`, `gitDir`,
`branch`, `head`, `detached`, and parsed `remotes`, read from `.git` directly. It
deliberately reports no dirty state and no ahead/behind counts.

## 4. Scan the index

   `open-finder scan --pattern <glob> --path-filter <path> --json`

Filters compose: `--regex`, `--match-path`, `--kind <kind>`,
`--min-size`, `--max-size`, `--modified-after-ms`, `--modified-before-ms`,
`--sort <key>`, `--desc`, `--limit`, `--offset`.

Results are paged. `total` is the full match count, `hasMore` tells you whether
to request the next page. Never raise `--limit` above what you need: pages are
capped at 1000 entries so a million-row answer cannot exhaust the host.

A portable walk that stops at `--max-entries` returns `"truncated": true`. Treat
its `total` as the inspected count and say so instead of reporting a folder as
fully searched.

## 5. Explicit non-index scan

Only use this when the user asked to scan one folder, or when
`capabilities` reports `unsupported_platform` and the user accepted a
non-indexed answer. Results are labelled `"indexed": false` and
`"mode": "portable-walk"`.

   `open-finder scan --mode <mode> --root <path> --pattern <glob> --json`

## 6. Audit

Every call is recorded by the shared audit facility. Use `--actor-kind agent`
and a stable `--actor` id so records are attributable.

   `open-finder audit --kind <kind> --limit <count> --json`

## Safety

- Confirm with the user before any command that writes the index (`index` and
  `index --update`) and before anything that requires an elevated process.
- Report structured failures verbatim: `code`, `capability`, `platform`. Do not
  retry a capability failure with elevated privileges on your own.
- Do not present portable-walk output as an NTFS index result.
- Do not treat `--limit` as a total result count.
- `volumes` and `inspect` are read-only and need no confirmation. Never replace
  them with PowerShell, `wmic`, Everything, or the `git` binary; they are the
  supported internal path.
- Never edit the index files directly. Rebuild or update through the CLI.
