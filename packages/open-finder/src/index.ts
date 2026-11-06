/**
 * Typed library surface over the N-API boundary.
 *
 * Every function forwards to the native addon and converts structured native
 * errors into `AppError`. The CLI, skills, and GUI consume this module so they
 * share one error contract.
 */
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

/** Open the GUI window. */
export function openGui(options: GuiOptions = {}): { backend: string } {
    return invoke(() => loadNative().openGui(options) as { backend: string });
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
