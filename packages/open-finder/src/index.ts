/**
 * Typed library surface over the N-API boundary.
 *
 * Every function forwards to the native addon and converts structured native
 * errors into `AppError`. The CLI, skills, and GUI consume this module so they
 * share one error contract.
 */
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

import { AppError, fromNativeError } from './errors.js';
import { loadNative } from './native.js';

/** Capability availability states. `null` means available. */
export type CapabilityState = 'unsupported_platform' | 'not_implemented' | 'dependency_blocked';

/** One capability row. */
export interface CapabilityItem {
    id: string;
    state: CapabilityState | null;
    detail: string | null;
}

/** Domain capability report. */
export interface CapabilitiesReport {
    platform: string;
    arch: string;
    items: CapabilityItem[];
    notes: string[];
}

/** NTFS volume geometry and journal state. */
export interface VolumeReport {
    volume: string;
    volumeSerial: number;
    bytesPerSector: number;
    clusterSize: number;
    recordSize: number;
    recordCount: number;
    journalId: number | null;
    firstUsn: number | null;
    nextUsn: number | null;
    requiresElevation: boolean;
}

/** Index presence and header fields for one volume. */
export interface IndexStatus {
    dir: string;
    present: boolean;
    schema: string | null;
    entryCount: number | null;
    cursor: number | null;
    builtAtMs: number | null;
    updatedAtMs: number | null;
    error: string | null;
}

/** One logical volume. */
export interface VolumeInfo {
    volume: string;
    label: string | null;
    filesystem: string | null;
    ntfs: boolean;
    driveType: string;
    volumeSerial: number | null;
    totalBytes: number | null;
    freeBytes: number | null;
    availableBytes: number | null;
    index: IndexStatus;
}

/** Report from the read-only `volumes` command. */
export interface VolumesReport {
    platform: string;
    indexDir: string;
    volumes: VolumeInfo[];
    notes: string[];
}

/** Recursive directory totals. */
export interface TreeStats {
    files: number;
    dirs: number;
    bytes: number;
    entries: number;
    truncated: boolean;
}

/** One git remote parsed from `.git/config`. */
export interface RemoteRef {
    name: string;
    url: string;
}

/** Minimal git repository state parsed from `.git` without a git process. */
export interface RepoSummary {
    root: string;
    gitDir: string;
    branch: string | null;
    head: string | null;
    detached: boolean;
    remotes: RemoteRef[];
}

/** One inspected path. */
export interface PathReport {
    path: string;
    exists: boolean;
    kind: 'file' | 'dir' | 'symlink' | 'other' | 'missing';
    size: number;
    linkTarget: string | null;
    attributes: string[];
    createdMs: number;
    modifiedMs: number;
    accessedMs: number;
    tree: TreeStats | null;
    repo: RepoSummary | null;
}

/** Options for `inspect`. */
export interface InspectOptions {
    path: string;
    /** Compute a directory's recursive totals. Defaults to true. */
    recursive?: boolean;
    /** Entry cap for the recursive walk. Defaults to 200000. */
    maxEntries?: number;
}

/** Result of a full `$MFT` index build. */
export interface BuildReport {
    volume: string;
    indexDir: string;
    volumeSerial: number;
    recordSize: number;
    recordCount: number;
    scanned: number;
    unallocated: number;
    nameless: number;
    entryCount: number;
    cursor: number;
    tookMs: number;
    method: string;
}

/** Result of an incremental USN journal update. */
export interface UpdateReport {
    applied: number;
    reloaded: number;
    removed: number;
    cursor: number;
    needsRescan: boolean;
    batches: number;
    tookMs: number;
}

/** Entry kind filter. */
export type EntryKind = 'file' | 'dir';

/** Sort key. */
export type SortKey = 'name' | 'path' | 'size' | 'modified';

/** Query filters and paging shared by indexed search and the portable walk. */
export interface QuerySpec {
    /** Glob pattern (`*` and `?`), or a regex when `regex` is true. */
    pattern?: string;
    regex?: boolean;
    matchPath?: boolean;
    pathFilter?: string;
    minSize?: number;
    maxSize?: number;
    modifiedAfterMs?: number;
    modifiedBeforeMs?: number;
    kind?: EntryKind;
    sort?: SortKey;
    descending?: boolean;
    limit?: number;
    offset?: number;
}

/** Indexed search request. */
export interface SearchOptions extends QuerySpec {
    indexDir?: string;
}

/** Portable walk options. This mode is always reported as non-indexed. */
export interface WalkOptions extends QuerySpec {
    root: string;
    maxDepth?: number;
    maxEntries?: number;
}

/** One search result. */
export interface SearchHit {
    frn: number;
    name: string;
    path: string;
    isDir: boolean;
    size: number;
    createdMs: number;
    modifiedMs: number;
    accessedMs: number;
}

/** One page of results. */
export interface SearchPage {
    total: number;
    offset: number;
    limit: number;
    hasMore: boolean;
    hits: SearchHit[];
    mode: string;
    indexed: boolean;
    root: string | null;
    skipped: number;
    /** True when a portable walk stopped at `maxEntries` before the tree was exhausted. */
    truncated: boolean;
    tookMs: number;
}

/** Content predicate. At least one bound is required; that is what separates a content search from a metadata query. */
export interface ContentFilter {
    minLines?: number;
    maxLines?: number;
}

/** Resource bounds for one scan. Exhausting any of them stops the scan and sets `budgetExceeded`. */
export interface ScanBudget {
    maxFiles?: number;
    maxBytes?: number;
    timeoutMs?: number;
}

/** Directory-name preset pruned before traversal. */
export type ExcludePreset = 'code' | 'none';

/** What to do with files that sniff as binary. */
export type BinaryPolicy = 'skip' | 'count';

/** Sort key for scan results. */
export type ScanSort = 'lines' | SortKey;

/** Content search request. Reading file bytes is what separates it from the metadata scan. */
export interface ScanOptions {
    root: string;
    /** Metadata pre-filter applied before any file is opened. */
    candidates?: QuerySpec;
    /** Glob allow-list for files, matched against the name and the full path. */
    include?: string[];
    /** Glob deny-list. A matching directory is pruned without traversing it. */
    exclude?: string[];
    /** Defaults to `code`: prunes node_modules, .git, dist, build, target, __pycache__, .venv, vendor. */
    excludePreset?: ExcludePreset;
    content: ContentFilter;
    sort?: ScanSort;
    descending?: boolean;
    limit?: number;
    binaryPolicy?: BinaryPolicy;
    followSymlinks?: boolean;
    maxDepth?: number;
    maxEntries?: number;
    budget?: ScanBudget;
}

/** One scan result: a search hit plus the measured line count. */
export interface ScanHit extends SearchHit {
    lines: number;
}

/** Outcome of a content scan. */
export interface ScanPage {
    total: number;
    hits: ScanHit[];
    scanned: number;
    skipped: number;
    /** True when a budget stopped the scan early; the result is partial. */
    budgetExceeded: boolean;
    truncated: boolean;
    root: string | null;
    tookMs: number;
    mode: 'content-scan';
}

/** One planned host action. */
export interface PlannedAction {
    kind: string;
    target: string;
    detail: string;
}

/** Install-or-upgrade plan. */
export interface InstallPlan {
    app: string;
    version: string;
    platform: string;
    configDir: string;
    configPath: string;
    markerPath: string;
    shortcutPath: string | null;
    installed: boolean;
    installedVersion: string | null;
    upgrade: boolean;
    requiresElevation: boolean;
    autoStart: boolean;
    actions: PlannedAction[];
}

/** Uninstall plan. */
export interface UninstallPlan {
    app: string;
    version: string;
    platform: string;
    configPath: string;
    shortcutPath: string | null;
    markerPath: string;
    configExists: boolean;
    shortcutExists: boolean;
    requiresElevation: boolean;
    actions: PlannedAction[];
}

/** Options for install planning and application. */
export interface InstallOptions {
    noShortcut?: boolean;
    resetConfig?: boolean;
}

/** Options for uninstall planning and application. */
export interface UninstallOptions {
    keepConfig?: boolean;
}

/** GUI open options. */
export interface GuiOptions {
    contentDir?: string;
    title?: string;
    /**
     * What a second open request does in single-window mode. Defaults to the
     * `window.policy.secondLaunch` value in `wae-runtime.json`, or `focus`.
     */
    secondLaunch?: 'focus' | 'reject';
}

/** Caller identity used by the shared audit facility. */
export interface AuditActor {
    actorKind?: 'user' | 'agent';
    actorId?: string;
    actorSource?: string;
    session?: string;
    host?: string;
}

/** Audit query filter. */
export interface AuditQuery {
    kind?: 'user' | 'agent';
    id?: string;
    fromMs?: number;
    toMs?: number;
    capability?: string;
    limit?: number;
}

/** Success payload shared by the mutating capabilities. */
export interface OkResult {
    ok: true;
}

function invoke<T>(run: () => T): T {
    try {
        return run();
    }
    catch (error) {
        throw fromNativeError(error);
    }
}

/** Native addon version. */
export function version(): string {
    return invoke(() => loadNative().version());
}

/** Domain capability report. */
export function capabilities(): CapabilitiesReport {
    return invoke(() => loadNative().capabilities() as CapabilitiesReport);
}

/** Runtime platform name. */
export function platform(): string {
    return invoke(() => loadNative().platform());
}

/** Resolved index directory used when `--index-dir` is omitted. */
export function defaultIndexDir(): string {
    return invoke(() => loadNative().defaultIndexDir());
}

/** Inspect a volume without writing an index. */
export function inspectVolume(options: { volume: string }): VolumeReport {
    return invoke(() => loadNative().inspectVolume(options) as VolumeReport);
}

/** List logical volumes and the index each one holds. Read-only. */
export function volumes(): VolumesReport {
    return invoke(() => loadNative().volumes() as VolumesReport);
}

/**
 * Inspect one path: metadata, optional recursive totals, and the nearest git
 * repository parsed in-tree. Read-only; never spawns `git` or a search tool.
 */
export function inspectPath(options: InspectOptions): PathReport {
    return invoke(() => loadNative().inspectPath(options) as PathReport);
}

/** Build a full index by reading `$MFT`. */
export function indexBuild(options: { volume: string; indexDir?: string; recordLimit?: number }): BuildReport {
    return invoke(() => loadNative().indexBuild(options) as BuildReport);
}

/** Apply USN journal changes to an existing index. */
export function indexUpdate(options: { indexDir?: string; maxRecords?: number } = {}): UpdateReport {
    return invoke(() => loadNative().indexUpdate(options) as UpdateReport);
}

/** Query an existing index. */
export function search(options: SearchOptions = {}): SearchPage {
    return invoke(() => loadNative().search(options as Record<string, unknown>) as SearchPage);
}

/** Explicit non-index directory walk. */
export function walk(options: WalkOptions): SearchPage {
    return invoke(() => loadNative().searchWalk(options as unknown as Record<string, unknown>) as SearchPage);
}

/**
 * Content-condition scan. The request crosses the boundary as JSON because the
 * option shape carries glob lists; the core owns validation.
 */
export function scan(options: ScanOptions): ScanPage {
    return invoke(() => JSON.parse(loadNative().scanContent(JSON.stringify(options)) as string) as ScanPage);
}

/** Plan an install or upgrade without touching host state. */
export function installPlan(options: InstallOptions = {}): InstallPlan {
    return invoke(() => loadNative().installPlan(options) as InstallPlan);
}

/** Apply an install or upgrade. */
export function installApply(options: InstallOptions = {}): OkResult {
    return invoke(() => loadNative().installApply(options) as OkResult);
}

/** Plan an uninstall without touching host state. */
export function uninstallPlan(options: UninstallOptions = {}): UninstallPlan {
    return invoke(() => loadNative().uninstallPlan(options) as UninstallPlan);
}

/** Apply an uninstall. */
export function uninstallApply(options: UninstallOptions = {}): OkResult {
    return invoke(() => loadNative().uninstallApply(options) as OkResult);
}

/** Configure the shared audit facility for this process. */
export function configureAudit(actor: AuditActor): OkResult {
    return invoke(() => loadNative().configureAudit(actor) as OkResult);
}

/** Read audit entries for the current actor as structured records. */
export function auditQuery(filter: AuditQuery = {}): Record<string, unknown>[] {
    const json = invoke(() => loadNative().auditQuery(filter));
    return invoke(() => JSON.parse(json) as Record<string, unknown>[]);
}

/**
 * Read `window.policy.secondLaunch` from the contract `wae build` writes next
 * to the CLI entry. A missing or unreadable contract means the default policy:
 * focus the existing window instead of opening a second one.
 */
function secondLaunchFromContract(): 'focus' | 'reject' {
    try {
        const url = new URL('./wae-runtime.json', import.meta.url);
        const value: unknown = JSON.parse(fs.readFileSync(fileURLToPath(url), 'utf8'));
        if (typeof value !== 'object' || value === null || Array.isArray(value)) return 'focus';
        const window = (value as { window?: unknown }).window;
        if (typeof window !== 'object' || window === null) return 'focus';
        const policy = (window as { policy?: unknown }).policy;
        if (typeof policy !== 'object' || policy === null) return 'focus';
        return (policy as { secondLaunch?: unknown }).secondLaunch === 'reject' ? 'reject' : 'focus';
    }
    catch {
        return 'focus';
    }
}

/** Open the GUI window. */
export function openGui(options: GuiOptions = {}): { backend: string } {
    const secondLaunch = options.secondLaunch ?? secondLaunchFromContract();
    return invoke(() => loadNative().openGui({ ...options, secondLaunch }) as { backend: string });
}

/** Pump one GUI event batch. */
export function pumpEvents(): boolean {
    return invoke(() => loadNative().pumpEvents());
}

/** Request GUI close. */
export function closeGui(): boolean {
    return invoke(() => loadNative().closeGui());
}

/** Whether the GUI window is still open. */
export function isGuiOpen(): boolean {
    return invoke(() => loadNative().isGuiOpen());
}

/**
 * Open the GUI and pump until the window closes.
 *
 * The host owns the event loop, so the process stays alive without spawning a
 * second runtime. Implemented here once so the CLI and plugins share it.
 */
export async function runGui(options: GuiOptions = {}): Promise<string> {
    const opened = openGui(options);
    while (pumpEvents()) {
        await new Promise((resolve) => setTimeout(resolve, 16));
    }
    closeGui();
    return opened.backend;
}

export { AppError, EXIT, fromNativeError } from './errors.js';
