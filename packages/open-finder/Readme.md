# @wae/open-finder

Meta package for OpenFinder. It exposes the typed native surface, the agent
CLI, and the audit configuration used by the CLI and UI.

```sh
open-finder capabilities --json
open-finder inspect --root <path> --limit <n> --json
open-finder install --dry-run --json
```

`install` is install-or-upgrade and idempotent. Without `--dry-run` the skeleton
returns `not_implemented` because platform shortcut and config writes belong to
the application domain. Native loading failures and capability refusals are
machine-readable through the `wae.error:` payload.

License: Apache-2.0, see the repository-root `License.md`.
