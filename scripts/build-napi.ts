#!/usr/bin/env node
/**
 * Build the Rust cdylib, then stage it through `stage-native.ts`.
 *
 * The target table and artifact lookup live in `stage-native.ts` and are
 * imported here, so the build and staging entry points share one implementation
 * and do not form an import cycle.
 */
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

import { projectRoot, resolveTarget, stageNative, type NativeTarget } from './stage-native.ts';

export async function buildNapi(root: string = projectRoot()): Promise<string> {
    const target: NativeTarget = resolveTarget();
    const result = spawnSync(process.env.CARGO ?? 'cargo', ['build', '-p', 'open_finder-napi', '--release'], {
        cwd: root,
        stdio: 'inherit',
        env: {
            ...process.env,
            CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '2',
        },
    });
    if (result.status !== 0) {
        throw new Error(`cargo build failed with exit code ${result.status ?? 'unknown'}`);
    }
    return stageNative(root, target);
}

const entry = process.argv[1] ? pathToFileURL(process.argv[1]).href : '';
if (import.meta.url === entry) {
    try {
        const dest = await buildNapi();
        console.log(`built and staged native addon -> ${dest}`);
    }
    catch (error) {
        console.error(`build-napi failed: ${error instanceof Error ? error.message : error}`);
        process.exit(1);
    }
}
