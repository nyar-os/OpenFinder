# OpenFinder Agent 参考

## CLI 命令

```sh
open-finder capabilities --json
open-finder volumes --json
open-finder inspect --path <path> --json
open-finder index --volume C: --dry-run --json
open-finder index --volume C: --actor <id> --json
open-finder index --update --max-records 200000 --json
open-finder scan --pattern "*.dll" --path-filter "\\Windows" --sort size --desc --json
open-finder scan --regex --pattern "^setup.*\\.exe$" --limit 20 --offset 20 --json
open-finder scan --mode walk --root <path> --pattern "*.log" --json
open-finder audit --kind agent --limit 20 --json
open-finder version --json
```

## Exit codes

- `0` — success
- `1` — runtime failure
- `2` — usage
- `3` — capability unavailable

## Scan 语法

Scan supports glob (`*`, `?`), regex, case-insensitive path filtering, size and modified-time ranges, kind (`file` / `dir`), sorting by `name` / `path` / `size` / `modified`, and paging (`--limit`, `--offset`). Pages are capped at 1000 hits. A portable walk that stops at `--max-entries` sets `truncated: true`, and its `total` then counts the entries it inspected.

## 只读诊断

`volumes` 与 `inspect` 只读取宿主状态，不写索引、不需要提权、不调用外部工具。

- `volumes`：列出逻辑卷的文件系统、容量与 `index` 状态（`dir`、`present`、`entryCount`、`cursor`、`builtAtMs`）。`present: false` 表示该卷尚未建索引。
- `inspect --path <path>`：返回 `exists`、`kind`、`size`、Windows `attributes`、符号链接/junction 的 `linkTarget`；目录默认递归统计 `tree`（`--shallow` 关闭，`--max-entries <n>` 设上限，触顶时 `tree.truncated: true`）。
- 路径位于 git 仓库内时，`repo` 直接解析 `.git` 给出 `root`、`gitDir`、`branch`、`head`、`detached` 与 `remotes`，不启动 `git`，也不计算 dirty 或 ahead/behind。

## Platform boundary

| Capability | Windows | macOS / Linux |
|------------|---------|---------------|
| `index-build` | available, reads `$MFT` through a raw volume handle | `unsupported_platform` |
| `index-update` | available, reads the USN journal | `unsupported_platform` |
| `scan` | available over an existing index | available over an existing index |
| `portable-walk` | available, explicitly non-indexed | available, explicitly non-indexed |
| `volumes` | available, reads the Windows volume API | `unsupported_platform` |
| `inspect` | available | available |

Indexing needs an elevated process. A denied volume handle is reported as `dependency_blocked` with the Win32 error, never as an empty index. The three failure states are `unsupported_platform`, `not_implemented`, and `dependency_blocked`.
