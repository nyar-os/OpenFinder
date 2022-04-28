# OpenFinder UI

VMZ search page for the OpenFinder native host.

```bash
pnpm run build
```

`vmz build . --profile static` writes to `dist/static`, then
`scripts/stage-gui.ts` copies it to `dist/gui`, which is the directory the N-API
host loads by default.

The page talks to the host through the WebView bridge:

- send `{ "type": "capabilities" }`, `{ "type": "search", "pattern": "*.dll" }`,
  `{ "type": "searchWalk", "root": "<path>" }`, `{ "type": "installPlan" }`, or
  `{ "type": "close" }`
- receive `{ "ok": true, "data": ... }` or
  `{ "ok": false, "error": { "code", "capability", "platform", "message" } }`

Keep domain logic in `crates/core`. The page renders results, it does not
re-implement matching, sorting, or index updates.
