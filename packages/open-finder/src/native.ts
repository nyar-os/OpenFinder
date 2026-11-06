/**
 * Lazy loader for the platform-native addon.
 *
 * The addon is an optional dependency. It is loaded only when a native
 * capability is called, so `--help`, `version`, and locale resolution work on
 * machines without the binary.
 *
 * Every successful load reports its own identity on stderr. Without it a stale
 * `.node` from an older checkout reproduces old behaviour while the running
 * CLI looks current, and the only visible symptom is an unrelated error.
 */
import { existsSync, readFileSync, readdirSync, realpathSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { AppError } from './errors.js';

const require = createRequire(import.meta.url);
const PLATFORM_PACKAGE = '@wae/open-finder-win32-x64';
/** Directory name of the platform package inside the repository. */
const PLATFORM_DIR = PLATFORM_PACKAGE.replace('@wae/', '');
/** Workspace root that owns `packages/<dir>`; absent in installed consumers. */
const WORKSPACE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..');

/** Build identity embedded by `crates/napi/build.rs`. */
export interface BuildInfo {
    version: string;
    commit: string;
    builtAtUnix: number;
    target: string;
}

/** Raw surface exported by the Rust `napi` crate. */
export interface NativeBindings {
    version(): string;
    buildInfo(): BuildInfo;
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
    configureAudit(options: { actorKind?: string; actorId?: string; actorSource?: string; session?: string; host?: string }): unknown;
    auditQuery(filter?: { kind?: string; id?: string; fromMs?: number; toMs?: number; capability?: string; limit?: number }): string;
}

let cached: NativeBindings | null = null;

/** Load the native addon once, after proving which artifact it came from. */
export function loadNative(): NativeBindings {
    if (cached) return cached;
    try {
        const resolved = require.resolve(PLATFORM_PACKAGE);
        assertWorkspaceArtifact(resolved);
        cached = require(PLATFORM_PACKAGE) as NativeBindings;
        reportAddonIdentity(resolved, cached);
        return cached;
    }
    catch (error) {
        cached = null;
        if (error instanceof AppError) throw error;
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

/** Whether `WAE_WEBVIEW_DIAG=1` asks for the full candidate list. */
function diagEnabled(): boolean {
    const value = process.env.WAE_WEBVIEW_DIAG;
    return value === '1' || value?.toLowerCase() === 'true';
}

/** The repository artifact directory that a workspace checkout must load. */
function expectedArtifactDir(): string {
    return path.join(WORKSPACE_ROOT, 'packages', PLATFORM_DIR);
}

/**
 * Smoke assertion: in a workspace checkout the addon must come from
 * `packages/<platform package>`, never from a shadow copy under `node_modules`
 * or a copy outside this repository.
 */
function assertWorkspaceArtifact(resolved: string): void {
    if (!isWorkspace()) return;
    const real = safeRealpath(resolved);
    const expected = expectedArtifactDir();
    const inside = real.toLowerCase().startsWith(`${expected.toLowerCase()}${path.sep}`);
    if (inside) return;
    throw new AppError({
        code: 'native_artifact_mismatch',
        platform: `${process.platform}-${process.arch}`,
        path: real,
        message:
            `native addon resolved outside this repository's artifact directory.\n` +
            `  loaded:   ${real}\n` +
            `  expected: ${expected}${path.sep}\n` +
            'A stale copy would silently reproduce old behaviour. ' +
            'Remove the shadow copy or run `pnpm run build:napi`.',
    });
}

/** Report path, size, mtime, and the embedded build identity on stderr. */
function reportAddonIdentity(resolved: string, bindings: NativeBindings): void {
    const real = safeRealpath(resolved);
    const info = safeStat(real);
    const build = safeBuildInfo(bindings);
    const fields = [
        `addon=${real}`,
        `size=${info?.size ?? 'unknown'}`,
        `mtime=${info ? info.mtime.toISOString() : 'unknown'}`,
        `version=${build?.version ?? bindings.version()}`,
        `commit=${build?.commit ?? 'unknown'}`,
        `builtAt=${build ? new Date(build.builtAtUnix * 1000).toISOString() : 'unknown'}`,
        `target=${build?.target ?? `${process.platform}-${process.arch}`}`,
        ...(isWorkspace() ? [`head=${headCommit() ?? 'unknown'}`] : []),
    ];
    write(`native: ${fields.join(' ')}`);

    for (const warning of stalenessWarnings(real, info)) {
        write(`native: warning=${warning}`);
    }

    if (diagEnabled()) {
        write(`native: diag candidates:`);
        for (const candidate of addonCandidates()) {
            const realCandidate = safeRealpath(candidate);
            const stat = safeStat(realCandidate);
            const hit = realCandidate.toLowerCase() === real.toLowerCase() ? ' hit' : '';
            write(`native: diag   ${realCandidate} exists=${Boolean(stat)} size=${stat?.size ?? 0} mtime=${stat ? stat.mtime.toISOString() : 'unknown'}${hit}`);
        }
        write(`native: diag hit=${real}`);
    }
}

/**
 * Paths the loader could have used, in lookup order.
 *
 * Missing candidates stay in the list: the diagnostic exists to show what was
 * considered, including a hoisted copy that would shadow the workspace build.
 */
function addonCandidates(): string[] {
    const candidates: string[] = [];
    try {
        candidates.push(require.resolve(PLATFORM_PACKAGE));
    }
    catch {
        // The module is absent; the workspace and hoisted paths still matter.
    }
    for (const file of [addonFile(expectedArtifactDir()), addonFile(path.join(WORKSPACE_ROOT, 'node_modules', '@wae', PLATFORM_DIR))]) {
        if (!candidates.includes(file)) candidates.push(file);
    }
    return candidates;
}

/** Addon file name inside a platform package. */
function addonFile(dir: string): string {
    const suffix = `${process.platform}-${process.arch}`;
    const app = PLATFORM_DIR.endsWith(`-${suffix}`) ? PLATFORM_DIR.slice(0, -(suffix.length + 1)) : PLATFORM_DIR;
    return path.join(dir, `${app}.${suffix}.node`);
}

/**
 * Staleness warning for a workspace checkout.
 *
 * The CLI mtime alone is not a usable threshold: a full `pnpm run build` links
 * Rust first and compiles TypeScript after, so the CLI is always a few seconds
 * newer and the comparison would fire after every clean build. The native
 * sources answer the real question instead — did the crates change after this
 * addon was linked? The embedded commit stays in the identity line next to
 * `HEAD` so a foreign artifact is still visible without a false warning after
 * TypeScript-only commits.
 */
function stalenessWarnings(real: string, info: { mtime: Date } | null): string[] {
    if (!isWorkspace()) return [];
    const warnings: string[] = [];
    const newest = newestNativeSource();
    if (info && newest && info.mtime.getTime() < newest.mtime.getTime()) {
        warnings.push(
            `addon_older_than_native_sources addon=${real} addon_mtime=${info.mtime.toISOString()} ` +
                `newest_source=${newest.mtime.toISOString()} newest_source_path=${newest.file} ` +
                `cli_mtime=${cliMtime() ?? 'unknown'} hint="run pnpm run build:napi"`,
        );
    }
    return warnings;
}

/** Whether the loader runs from a checkout that owns this repository. */
function isWorkspace(): boolean {
    return existsSync(path.join(WORKSPACE_ROOT, 'pnpm-workspace.yaml'));
}

/** Current commit of the checkout, read from `.git` without spawning git. */
function headCommit(): string | null {
    try {
        const head = readFileSync(path.join(WORKSPACE_ROOT, '.git', 'HEAD'), 'utf8').trim();
        if (!head.startsWith('ref: ')) return head.length > 0 ? head : null;
        const ref = readFileSync(path.join(WORKSPACE_ROOT, '.git', head.slice(5).trim()), 'utf8').trim();
        return ref.length > 0 ? ref : null;
    }
    catch {
        return null;
    }
}

/** Newest file that feeds the addon build. */
function newestNativeSource(): { mtime: Date; file: string } | null {
    let newest: { mtime: Date; file: string } | null = null;
    const consider = (file: string): void => {
        const stat = safeStat(file);
        if (stat && (!newest || stat.mtime.getTime() > newest.mtime.getTime())) {
            newest = { mtime: stat.mtime, file };
        }
    };
    for (const file of ['Cargo.toml', 'Cargo.lock']) consider(path.join(WORKSPACE_ROOT, file));
    const walk = (dir: string): void => {
        let entries: import('node:fs').Dirent[];
        try {
            entries = readdirSync(dir, { withFileTypes: true });
        }
        catch {
            return;
        }
        for (const entry of entries) {
            const child = path.join(dir, entry.name);
            if (entry.isDirectory()) {
                if (entry.name !== 'target' && entry.name !== '.git') walk(child);
            }
            else if (entry.name.endsWith('.rs') || entry.name === 'Cargo.toml') {
                consider(child);
            }
        }
    };
    walk(path.join(WORKSPACE_ROOT, 'crates'));
    return newest;
}

/** Mtime of the running CLI entry, when it is a real file. */
function cliMtime(): string | null {
    const entry = process.argv[1];
    if (!entry) return null;
    const stat = safeStat(entry);
    return stat ? stat.mtime.toISOString() : null;
}

function safeBuildInfo(bindings: NativeBindings): BuildInfo | null {
    if (typeof bindings.buildInfo !== 'function') return null;
    try {
        return bindings.buildInfo();
    }
    catch {
        return null;
    }
}

function safeRealpath(file: string): string {
    try {
        return realpathSync(file);
    }
    catch {
        return file;
    }
}

function safeStat(file: string): { size: number; mtime: Date } | null {
    try {
        const stat = statSync(file);
        return { size: stat.size, mtime: stat.mtime };
    }
    catch {
        return null;
    }
}

function write(line: string): void {
    process.stderr.write(`${line}\n`);
}
