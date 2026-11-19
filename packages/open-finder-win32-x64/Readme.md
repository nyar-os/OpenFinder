# open-finder-win32-x64

Platform package for the `open-finder` native addon on Windows x64.

The `*.node` binary is built by `wae native build` and staged here by
`wae native stage` (configured in `wae.config.ts`). It is not committed. Build before publishing:

```bash
pnpm run build:napi
```

The app package lists this package under `optionalDependencies`, so npm and
pnpm install it only on matching `os` / `cpu` targets.
