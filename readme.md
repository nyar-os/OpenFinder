# OpenFinder

![NTFS Direct](https://img.shields.io/badge/backend-MFT%20%2B%20USN-brightgreen)
![Cross Platform](https://img.shields.io/badge/platform-win%20%7C%20mac%20%7C%20linux-lightgrey)

OpenFinder builds an index of your files by reading the NTFS Master File Table directly — no directory traversal, no shelling out to `dir`. For follow-up, it watches the USN Journal for incremental updates so rebuilding only takes seconds.

## 🔍 What it does

OpenFinder is a fast file search engine with two execution paths:

- **`$MFT` direct reader (Windows)** — opens the NTFS master file table as a raw handle, enumerates all file records, and builds a searchable index in a fraction of the time a recursive walk would take. Requires administrator elevation.
- **USN Journal incremental update (Windows)** — after the first build, subscribes to the USN Journal to catch creates, deletes, and renames without re-reading the whole volume. Falls back to a full rebuild only when the journal cursor has aged past its retention window.
- **Walk mode (portable)** — on non-Windows platforms, or when elevation is not available on Windows, OpenFinder degrades to a conventional directory walk with the same query API and output shape.

Every mode supports the same query surface: glob patterns, regex matching, size ranges, modification-time windows, file/dir kind filters, multi-key sorting, and pagination.

## 🚀 CLI in action

Build an index for your Windows C: volume (requires elevation):

```bash
open-finder index --volume C: --json
```

Search the indexed volume with a glob, path filter, and size sort:

```bash
open-finder search --pattern "*.dll" --path-filter "\\Windows\\System32" --sort size --desc --json
```

Use regex for installers on a non-indexed volume or a portable OS:

```bash
open-finder search --mode walk --root ./downloads --regex --pattern "^(setup|installer).*\\.exe$" --limit 20
```

## 🏗️ How it works

```mermaid
flowchart LR
    subgraph Windows elevated
        A[$MFT handle] --> B[Index builder<br/>enumerate records<br/>extract names & paths]
        C[USN Journal handle] --> D[Incremental updater<br/>apply deltas<br/>update cursor]
    end
    subgraph Portable
        E[Directory walk] --> F[Query engine]
    end
    B --> F
    D --> F
    F --> G[Index query<br/>glob · regex · range · sort · page]
    G --> H[User-facing API]
    subgraph Windows only
        C
    end
```

The `$MFT` path skips directory enumeration entirely — it reads the flat record table that NTFS maintains for every file on the volume. The trade-off is administrator elevation, which OpenFinder will explicitly request and will never silently skip. The walk path uses the same query engine so your scripts don't need to branch by OS.

## ⚠️ What it does NOT do

- It does **not** bypass NTFS permissions on files themselves. Indexing only reads the `$MFT` metadata; opening file contents still follows your process's access token.
- It does **not** work on FAT32, exFAT, or ReFS. Only NTFS volumes expose the `$MFT` and USN Journal interfaces.
- It does **not** provide real-time indexing without you asking. After the first build, run `index --update` periodically — the USN Journal does not push updates to a running daemon.
- It does **not** mount or decrypt BitLocker-encrypted volumes. The `$MFT` is only readable after the volume is unlocked by the OS.
- It does **not** deduplicate results. Hard-linked files appear as separate entries because OpenFinder indexes by record, not by inode content.

## 🖥️ Platform notes

| Capability | Windows | macOS / Linux |
|------------|---------|---------------|
| `$MFT` direct indexing | ✅ elevated | ❌ unsupported_platform |
| USN Journal incremental update | ✅ elevated | ❌ unsupported_platform |
| Index-based search | ✅ | ✅ (after `index` on a supported platform) |
| Walk-mode search | ✅ | ✅ |

When elevation is not available on Windows, OpenFinder falls back to walk mode and reports the capability gap in the response. It never silently returns empty results.
