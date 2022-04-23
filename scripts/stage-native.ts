#!/usr/bin/env node
/**
 * Copy the native addon into its platform package.
 *
 * This module owns the only native target table and artifact lookup. `build-napi.ts`
 * imports them from here, so the build and staging entry points cannot drift and
 * there is no import cycle between the two scripts.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const APP = 'open-finder';
const CARGO_LIB = 'open_finder_napi';

export interface NativeTarget {
    platform: string;
    arch: string;
    triple: string;
    packageSuffix: string;
    fileName: string;
}

export const NATIVE_TARGETS: NativeTarget[] = [
    {
        platform: 'win32',
        arch: 'x64',
        triple: 'x86_64-pc-windows-msvc',
        packageSuffix: 'win32-x64',
        fileName: `${APP}.win32-x64.node`,
    },
];

export function projectRoot(from: string = import.meta.url): string {
    return path.resolve(path.dirname(fileURLToPath(from)), '..');
}

export function resolveTarget(platform: string = process.platform, arch: string = process.arch): NativeTarget {
    const target = NATIVE_TARGETS.find((item) => item.platform === platform && item.arch === arch);
    if (!target) {
        throw new Error(
            `no platform package for ${platform}-${arch}. This skeleton starts with win32-x64 only. ` +
                'Add a packages/<app>-<platform>-<arch> package and a NATIVE_TARGETS entry before publishing that target.',
        );
    }
    return target;
}

/** Resolve the first existing cargo output for the target. */
export function resolveNapiArtifact(root: string = projectRoot(), target: NativeTarget = resolveTarget()): string {
    const candidates = [
        path.join(root, 'target', target.triple, 'release', `${CARGO_LIB}.dll`),
        path.join(root, 'target', 'release', `${CARGO_LIB}.dll`),
        path.join(root, 'target', target.triple, 'release', `${CARGO_LIB}.node`),
        path.join(root, 'target', 'release', `${CARGO_LIB}.node`),
    ];
    for (const candidate of candidates) {
        if (fs.existsSync(candidate)) return candidate;
    }
    throw new Error(`native artifact not found. Run \`cargo build -p ${CARGO_LIB} --release\` first. Tried:\n${candidates.join('\n')}`);
}

/** Copy the artifact into the platform package and return its destination. */
export function stageNative(root: string = projectRoot(), target: NativeTarget = resolveTarget()): string {
    const artifact = resolveNapiArtifact(root, target);
    const packageDir = path.join(root, 'packages', `${APP}-${target.packageSuffix}`);
    if (!fs.existsSync(packageDir)) {
        throw new Error(`platform package directory is missing: ${packageDir}`);
    }
    const dest = path.join(packageDir, target.fileName);
    fs.copyFileSync(artifact, dest);
    return dest;
}

const entry = process.argv[1] ? pathToFileURL(process.argv[1]).href : '';
if (import.meta.url === entry) {
    try {
        const dest = stageNative();
        console.log(`staged native artifact -> ${dest}`);
    }
    catch (error) {
        console.error(`stage-native failed: ${error instanceof Error ? error.message : error}`);
        process.exit(1);
    }
}
