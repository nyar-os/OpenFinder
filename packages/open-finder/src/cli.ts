#!/usr/bin/env node
/**
 * Agent-facing CLI.
 *
 * Contract:
 * - non-interactive, always exits with a stable code
 * - `--json` returns `{ ok, data }` or `{ ok: false, error }`
 * - every mutating command supports `--dry-run`
 * - every command configures the audit actor before calling the native layer
 * - exit codes: 0 success, 1 runtime failure, 2 usage, 3 capability unavailable
 */
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { createCli, type CommandContext, type ParsedOptions } from '@wae/commander';

import { AppError, EXIT, fromNativeError } from './errors.js';
import {
    auditQuery,
    capabilities,
    configureAudit,
    defaultIndexDir,
    indexBuild,
    indexUpdate,
    inspectVolume,
    installApply,
    installPlan,
    runGui,
    search,
    uninstallApply,
    uninstallPlan,
    version,
    walk,
    type BuildReport,
    type CapabilityItem,
    type InstallPlan,
    type PlannedAction,
    type QuerySpec,
    type SearchPage,
    type UninstallPlan,
    type UpdateReport,
    type VolumeReport,
} from './index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const localesDir = path.join(here, '..', 'locales');

const cli = createCli('open-finder')
    .locales(localesDir)
    .intro('cli.intro')
    // Root options take no `OptionConfig` in `@wae/commander`. Defaults and
    // enum validation live in `bootstrap()` so the help tree stays stable.
    .option('--actor <id>', 'cli.opt.actor')
    .option('--actor-kind <kind>', 'cli.opt.actor-kind')
    .option('--actor-source <source>', 'cli.opt.actor-source')
    .option('--json', 'cli.opt.json');

function stringOption(options: ParsedOptions, key: string): string | undefined {
    const value = options[key];
    return typeof value === 'string' && value.trim() ? value : undefined;
}

function flag(options: ParsedOptions, key: string): boolean {
    return options[key] === true;
}

function integerOption(options: ParsedOptions, key: string): number | undefined {
    const raw = stringOption(options, key);
    if (raw === undefined) return undefined;
    const value = Number(raw);
    if (!Number.isInteger(value) || value < 0) {
        throw new AppError({ code: 'invalid_argument', message: `\`--${key}\` must be a non-negative integer` });
    }
    return value;
}

/** Configure the audit actor before any capability call. */
function bootstrap(options: ParsedOptions): void {
    const kind = stringOption(options, 'actor-kind');
    if (kind !== undefined && kind !== 'user' && kind !== 'agent') {
        throw new AppError({ code: 'invalid_argument', message: '`--actor-kind` must be `user` or `agent`' });
    }
    configureAudit({
        actorKind: kind === 'agent' ? 'agent' : 'user',
        actorId: stringOption(options, 'actor') ?? 'unknown',
        actorSource: stringOption(options, 'actor-source') ?? 'cli',
    });
}

function emit(options: ParsedOptions, ctx: CommandContext, data: unknown, human: string): void {
    if (flag(options, 'json')) {
        ctx.stdout(JSON.stringify({ ok: true, data }, null, 2));
        return;
    }
    if (human) ctx.stdout(human);
}

function fail(options: ParsedOptions, ctx: CommandContext, error: unknown): number {
    const appError = fromNativeError(error);
    if (flag(options, 'json')) {
        ctx.stdout(JSON.stringify({ ok: false, error: appError.toJSON() }, null, 2));
    }
    else {
        ctx.stderr(`${appError.code}: ${appError.message}`);
    }
    return appError.exitCode;
}

function formatCapabilities(items: CapabilityItem[], notes: string[]): string {
    const lines = items.map((item) => {
        const state = item.state ?? 'supported';
        const detail = item.detail ? ` — ${item.detail}` : '';
        return `${item.id}: ${state}${detail}`;
    });
    if (notes.length) lines.push('', ...notes.map((note) => `note: ${note}`));
    return lines.join('\n');
}

function formatActions(actions: PlannedAction[]): string {
    if (!actions.length) return '(no host actions)';
    return actions.map((action) => `${action.kind}: ${action.target} — ${action.detail}`).join('\n');
}

function formatVolume(report: VolumeReport): string {
    return [
        `volume: ${report.volume}`,
        `serial: ${report.volumeSerial}`,
        `cluster: ${report.clusterSize} bytes`,
        `record: ${report.recordSize} bytes`,
        `mftRecords: ${report.recordCount}`,
        `usn: ${report.journalId === null ? '(unavailable)' : `id ${report.journalId}, first ${report.firstUsn}, next ${report.nextUsn}`}`,
        `requiresElevation: ${report.requiresElevation}`,
    ].join('\n');
}

function formatBuild(report: BuildReport): string {
    return [
        `volume: ${report.volume}`,
        `index: ${report.indexDir}`,
        `records: ${report.scanned} scanned / ${report.recordCount}`,
        `skipped: ${report.unallocated} unallocated, ${report.nameless} nameless`,
        `entries: ${report.entryCount}`,
        `cursor: ${report.cursor}`,
        `method: ${report.method}`,
        `took: ${report.tookMs} ms`,
    ].join('\n');
}

function formatUpdate(report: UpdateReport): string {
    const lines = [
        `applied: ${report.applied}`,
        `reloaded: ${report.reloaded}`,
        `removed: ${report.removed}`,
        `cursor: ${report.cursor}`,
        `batches: ${report.batches}`,
        `took: ${report.tookMs} ms`,
    ];
    if (report.needsRescan) lines.push('needsRescan: true — the journal no longer covers the saved cursor, rebuild with `index --volume <v>`');
    return lines.join('\n');
}

function formatDuration(milliseconds: number): string {
    return new Date(milliseconds).toISOString();
}

function formatPage(page: SearchPage): string {
    const lines = [
        `mode: ${page.mode} (indexed: ${page.indexed})`,
        `root: ${page.root ?? '(unknown)'}`,
        `total: ${page.total}`,
        `page: ${page.offset}..${page.offset + page.hits.length}${page.hasMore ? ' (more available)' : ''}`,
        `skipped: ${page.skipped}`,
        `took: ${page.tookMs} ms`,
        '',
    ];
    for (const hit of page.hits) {
        const kind = hit.isDir ? 'dir ' : 'file';
        lines.push(`${kind}\t${String(hit.size).padStart(10)}\t${formatDuration(hit.modifiedMs)}\t${hit.path}`);
    }
    return lines.join('\n');
}

function formatInstall(plan: InstallPlan): string {
    return [
        `app: ${plan.app} ${plan.version}`,
        `platform: ${plan.platform}`,
        `installed: ${plan.installed}${plan.installedVersion ? ` (${plan.installedVersion})` : ''}`,
        `upgrade: ${plan.upgrade}`,
        `requiresElevation: ${plan.requiresElevation}`,
        `autoStart: ${plan.autoStart}`,
        `config: ${plan.configPath}`,
        `marker: ${plan.markerPath}`,
        `shortcut: ${plan.shortcutPath ?? '(none)'}`,
        '',
        formatActions(plan.actions),
    ].join('\n');
}

function formatUninstall(plan: UninstallPlan): string {
    return [
        `app: ${plan.app} ${plan.version}`,
        `platform: ${plan.platform}`,
        `config: ${plan.configPath} (exists: ${plan.configExists})`,
        `shortcut: ${plan.shortcutPath ?? '(none)'} (exists: ${plan.shortcutExists})`,
        `marker: ${plan.markerPath}`,
        `requiresElevation: ${plan.requiresElevation}`,
        '',
        formatActions(plan.actions),
    ].join('\n');
}

function queryFrom(options: ParsedOptions): QuerySpec {
    const pattern = stringOption(options, 'pattern');
    if (pattern !== undefined && flag(options, 'regex')) {
        try {
            new RegExp(pattern, 'i');
        }
        catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            throw new AppError({ code: 'invalid_argument', message: `invalid \`--pattern\` regex: ${message}` });
        }
    }
    const kind = stringOption(options, 'kind');
    if (kind !== undefined && kind !== 'file' && kind !== 'dir') {
        throw new AppError({ code: 'invalid_argument', message: '`--kind` must be `file` or `dir`' });
    }
    const sort = stringOption(options, 'sort');
    if (sort !== undefined && sort !== 'name' && sort !== 'path' && sort !== 'size' && sort !== 'modified') {
        throw new AppError({ code: 'invalid_argument', message: '`--sort` must be `name`, `path`, `size`, or `modified`' });
    }
    return {
        pattern,
        regex: flag(options, 'regex'),
        matchPath: flag(options, 'match-path'),
        pathFilter: stringOption(options, 'path-filter'),
        minSize: integerOption(options, 'min-size'),
        maxSize: integerOption(options, 'max-size'),
        modifiedAfterMs: integerOption(options, 'modified-after-ms'),
        modifiedBeforeMs: integerOption(options, 'modified-before-ms'),
        kind: kind as QuerySpec['kind'],
        sort: sort as QuerySpec['sort'],
        descending: flag(options, 'desc'),
        limit: integerOption(options, 'limit'),
        offset: integerOption(options, 'offset'),
    };
}

function requiredVolume(options: ParsedOptions): string {
    const volume = stringOption(options, 'volume');
    if (volume === undefined) {
        throw new AppError({ code: 'invalid_argument', message: '`--volume` is required, for example `--volume C:`' });
    }
    return volume;
}

function indexDirOption(options: ParsedOptions): string | undefined {
    return stringOption(options, 'index-dir');
}

cli.command('capabilities', 'cli.cmd.capabilities')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const report = capabilities();
            emit(options, ctx, report, formatCapabilities(report.items, report.notes));
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('index', 'cli.cmd.index')
    .option('--volume <volume>', 'cli.opt.volume')
    .option('--index-dir <path>', 'cli.opt.index-dir')
    .option('--record-limit <n>', 'cli.opt.record-limit')
    .option('--update', 'cli.opt.update')
    .option('--max-records <n>', 'cli.opt.max-records')
    .option('--dry-run', 'cli.opt.dry-run')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            if (flag(options, 'update')) {
                const request = {
                    indexDir: indexDirOption(options),
                    maxRecords: integerOption(options, 'max-records'),
                };
                const report = indexUpdate(request);
                emit(options, ctx, report, formatUpdate(report));
                return EXIT.OK;
            }
            const volume = requiredVolume(options);
            if (flag(options, 'dry-run')) {
                const report = inspectVolume({ volume });
                emit(options, ctx, report, formatVolume(report));
                return EXIT.OK;
            }
            const report = indexBuild({
                volume,
                indexDir: indexDirOption(options),
                recordLimit: integerOption(options, 'record-limit'),
            });
            emit(options, ctx, report, formatBuild(report));
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('search', 'cli.cmd.search')
    .option('--pattern <glob>', 'cli.opt.pattern')
    .option('--regex', 'cli.opt.regex')
    .option('--match-path', 'cli.opt.match-path')
    .option('--path-filter <text>', 'cli.opt.path-filter')
    .option('--min-size <n>', 'cli.opt.min-size')
    .option('--max-size <n>', 'cli.opt.max-size')
    .option('--modified-after-ms <n>', 'cli.opt.modified-after-ms')
    .option('--modified-before-ms <n>', 'cli.opt.modified-before-ms')
    .option('--kind <kind>', 'cli.opt.entry-kind', { enum: ['file', 'dir'] })
    .option('--sort <key>', 'cli.opt.sort', { enum: ['name', 'path', 'size', 'modified'] })
    .option('--desc', 'cli.opt.desc')
    .option('--limit <n>', 'cli.opt.limit')
    .option('--offset <n>', 'cli.opt.offset')
    .option('--index-dir <path>', 'cli.opt.index-dir')
    .option('--mode <mode>', 'cli.opt.mode', { enum: ['index', 'walk'] })
    .option('--root <path>', 'cli.opt.root')
    .option('--max-depth <n>', 'cli.opt.max-depth')
    .option('--max-entries <n>', 'cli.opt.max-entries')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const mode = stringOption(options, 'mode') ?? 'index';
            const spec = queryFrom(options);
            const page = mode === 'walk'
                ? walk({
                    ...spec,
                    root: stringOption(options, 'root') ?? '.',
                    maxDepth: integerOption(options, 'max-depth'),
                    maxEntries: integerOption(options, 'max-entries'),
                })
                : search({ ...spec, indexDir: indexDirOption(options) });
            emit(options, ctx, page, formatPage(page));
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('install', 'cli.cmd.install')
    .option('--dry-run', 'cli.opt.dry-run')
    .option('--no-shortcut', 'cli.opt.no-shortcut')
    .option('--reset-config', 'cli.opt.reset-config')
    .actionCtx(async (options, _args, ctx) => {
        const request = {
            noShortcut: flag(options, 'no-shortcut'),
            resetConfig: flag(options, 'reset-config'),
        };
        try {
            bootstrap(options);
            const plan = installPlan(request);
            if (flag(options, 'dry-run')) {
                emit(options, ctx, { dryRun: true, plan }, `dry-run\n\n${formatInstall(plan)}`);
                return EXIT.OK;
            }
            installApply(request);
            emit(options, ctx, { dryRun: false, plan }, `installed\n\n${formatInstall(plan)}`);
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('uninstall', 'cli.cmd.uninstall')
    .option('--dry-run', 'cli.opt.dry-run')
    .option('--keep-config', 'cli.opt.keep-config')
    .actionCtx(async (options, _args, ctx) => {
        const request = { keepConfig: flag(options, 'keep-config') };
        try {
            bootstrap(options);
            const plan = uninstallPlan(request);
            if (flag(options, 'dry-run')) {
                emit(options, ctx, { dryRun: true, plan }, `dry-run\n\n${formatUninstall(plan)}`);
                return EXIT.OK;
            }
            uninstallApply(request);
            emit(options, ctx, { dryRun: false, plan }, `uninstalled\n\n${formatUninstall(plan)}`);
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('gui', 'cli.cmd.gui')
    .option('--content-dir <path>', 'cli.opt.content-dir')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const backend = await runGui({ contentDir: stringOption(options, 'content-dir') });
            emit(options, ctx, { backend }, `closed (${backend})`);
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('audit', 'cli.cmd.audit')
    .option('--kind <kind>', 'cli.opt.kind', { enum: ['user', 'agent'] })
    .option('--id <id>', 'cli.opt.id')
    .option('--capability <name>', 'cli.opt.capability')
    .option('--from-ms <n>', 'cli.opt.from-ms')
    .option('--to-ms <n>', 'cli.opt.to-ms')
    .option('--limit <n>', 'cli.opt.limit')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const entries = auditQuery({
                kind: stringOption(options, 'kind') === 'agent' ? 'agent' : stringOption(options, 'kind') === 'user' ? 'user' : undefined,
                id: stringOption(options, 'id'),
                capability: stringOption(options, 'capability'),
                fromMs: integerOption(options, 'from-ms'),
                toMs: integerOption(options, 'to-ms'),
                limit: integerOption(options, 'limit'),
            });
            emit(options, ctx, entries, JSON.stringify(entries, null, 2));
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

cli.command('version', 'cli.cmd.version')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const current = version();
            emit(options, ctx, { version: current, indexDir: defaultIndexDir() }, current);
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

const code = await cli.parse(process.argv);
process.exitCode = code;
