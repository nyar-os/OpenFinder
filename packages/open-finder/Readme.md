# @wae/open-finder

Meta package for OpenFinder. It exposes the typed native surface and the agent
CLI.

```sh
open-finder capabilities --json
open-finder index --volume C: --dry-run --json
open-finder index --volume C: --actor <id> --json
open-finder index --update --json
open-finder search --pattern "*.dll" --path-filter "\\Windows" --json
open-finder audit --limit 20 --json
```

`index` reads `$MFT` directly and needs an elevated process. A denied volume
handle is reported as `dependency_blocked`. `--update` applies USN journal
changes and reports `needsRescan` when the journal no longer covers the saved
cursor.

`install` / `uninstall` are install-or-upgrade plans. Platform writes belong to
the application domain and currently report `not_implemented`.

Native loading failures and capability refusals are machine-readable through the
`wae.error:` payload.

License: Apache-2.0, see the repository-root `License.md`.
