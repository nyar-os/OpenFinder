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

/** Options for directory inspection. */
export interface InspectionOptions {
    root?: string;
    limit?: number;
}

/** One inspected filesystem entry. */
export interface InspectionEntry {
    path: string;
    bytes: number;
    kind: string;
}

/** Inspection report. */
export interface InspectionReport {
    root: string;
    platform: string;
    totalBytes: number;
    entryCount: number;
    truncated: boolean;
    entries: InspectionEntry[];
    inspectedAtUnixMs: number;
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

/** Inspect a directory tree. */
export function inspect(options: InspectionOptions = {}): InspectionReport {
    return invoke(() => loadNative().inspect(options) as InspectionReport);
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
