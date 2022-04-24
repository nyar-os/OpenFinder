/**
 * Lazy loader for the platform-native addon.
 *
 * The addon is an optional dependency. It is loaded only when a native
 * capability is called, so `--help`, `version`, and locale resolution work on
 * machines without the binary.
 */
import { createRequire } from 'node:module';
import { AppError } from './errors.js';

const require = createRequire(import.meta.url);
const PLATFORM_PACKAGE = '@wae/open-finder-win32-x64';

/** Raw surface exported by the Rust `napi` crate. */
export interface NativeBindings {
    version(): string;
    capabilities(): unknown;
    platform(): string;
    defaultIndexDir(): string;
    inspectVolume(options: { volume: string }): unknown;
    indexBuild(options: { volume: string; indexDir?: string; recordLimit?: number }): unknown;
    indexUpdate(options: { indexDir?: string; maxRecords?: number }): unknown;
    search(options: Record<string, unknown>): unknown;
    searchWalk(options: Record<string, unknown>): unknown;
    installPlan(options?: { noShortcut?: boolean; resetConfig?: boolean }): unknown;
    installApply(options?: { noShortcut?: boolean; resetConfig?: boolean }): unknown;
    uninstallPlan(options?: { keepConfig?: boolean }): unknown;
    uninstallApply(options?: { keepConfig?: boolean }): unknown;
    openGui(options?: { contentDir?: string; title?: string }): unknown;
    pumpEvents(): boolean;
    closeGui(): boolean;
    isGuiOpen(): boolean;
    configureAudit(options: {
        actorKind?: string;
        actorId?: string;
        actorSource?: string;
        session?: string;
        host?: string;
    }): unknown;
    auditQuery(filter?: {
        kind?: string;
        id?: string;
        fromMs?: number;
        toMs?: number;
        capability?: string;
        limit?: number;
    }): string;
}

let cached: NativeBindings | null = null;

/** Load the native addon once. */
export function loadNative(): NativeBindings {
    if (cached) return cached;
    try {
        cached = require(PLATFORM_PACKAGE) as NativeBindings;
        return cached;
    }
    catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new AppError({
            code: 'native_unavailable',
            platform: `${process.platform}-${process.arch}`,
            message:
                `cannot load ${PLATFORM_PACKAGE}: ${message}\n` +
                'Run `pnpm run build:napi` from the repository root, ' +
                'or install the matching platform package.',
        });
    }
}
