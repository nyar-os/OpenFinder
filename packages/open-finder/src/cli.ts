#!/usr/bin/env node
/**
 * Agent-facing CLI skeleton.
 *
 * Contract:
 * - non-interactive, always exits with a stable code
 * - `--json` returns `{ ok, data }` or `{ ok: false, error }`
 * - every mutating command supports `--dry-run`
 * - every command configures the audit actor before calling the native layer
 */
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { createCli, type CommandContext, type ParsedOptions } from '@wae/commander';

import { AppError, EXIT, fromNativeError } from './errors.js';
import {
    auditQuery,
    capabilities,
    configureAudit,
    inspect,
    installApply,
    installPlan,
    runGui,
    uninstallApply,
    uninstallPlan,
    version,
    type CapabilityItem,
    type InstallPlan,
    type InspectionReport,
    type PlannedAction,
    type UninstallPlan,
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

function formatInspection(report: InspectionReport): string {
    const lines = [
        `root: ${report.root}`,
        `platform: ${report.platform}`,
        `entries: ${report.entryCount}${report.truncated ? ' (truncated)' : ''}`,
        `totalBytes: ${report.totalBytes}`,
        '',
    ];
    lines.push(...report.entries.map((entry) => `${entry.bytes}\t${entry.kind}\t${entry.path}`));
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

cli.command('inspect', 'cli.cmd.inspect')
    .option('--root <path>', 'cli.opt.root')
    .option('--limit <n>', 'cli.opt.limit')
    .actionCtx(async (options, _args, ctx) => {
        try {
            bootstrap(options);
            const report = inspect({
                root: stringOption(options, 'root'),
                limit: integerOption(options, 'limit'),
            });
            emit(options, ctx, report, formatInspection(report));
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
            emit(options, ctx, { version: current }, current);
            return EXIT.OK;
        }
        catch (error) {
            return fail(options, ctx, error);
        }
    });

const code = await cli.parse(process.argv);
process.exitCode = code;
