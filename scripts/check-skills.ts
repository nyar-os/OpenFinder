#!/usr/bin/env node
/**
 * Validate agent skills against the real CLI command tree.
 *
 * Checks:
 * - every `skills/<name>/SKILL.md` has parseable frontmatter with `name` and `description`
 * - `name` matches the directory and is kebab-case
 * - each skill has a `## Safety` section and an explicit confirmation prerequisite
 * - every mutating command referenced by a skill documents a `--dry-run` invocation
 * - every referenced command exists in `--help`
 * - every referenced flag exists in `<command> --help`
 * - no link or path in a skill leaves the published skills tree
 * - the skills package version matches the owning app package and uses `Apache-2.0`
 */
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const skillsRoot = path.join(root, 'packages', 'open-finder-skills', 'skills');
const skillsPackage = path.join(root, 'packages', 'open-finder-skills', 'package.json');
const appPackage = path.join(root, 'packages', 'open-finder', 'package.json');
const cliOverrideIndex = process.argv.indexOf('--cli');
const cli =
    cliOverrideIndex > 0 && process.argv[cliOverrideIndex + 1]
        ? path.resolve(process.argv[cliOverrideIndex + 1])
        : process.env.WAE_SKILLS_CLI
            ? path.resolve(process.env.WAE_SKILLS_CLI)
            : path.join(root, 'packages', 'open-finder', 'dist', 'cli.js');
const mutatingCommands = new Set(['install', 'uninstall']);

function fail(message: string): never {
    console.error(`check-skills failed: ${message}`);
    process.exit(1);
}

function assertNoOutOfPackageReferences(source: string, file: string, dir: string): void {
    for (const match of source.matchAll(/\]\(([^)\s]+)(?:\s+[^)]*)?\)/g)) {
        const target = match[1];
        if (/^(?:https?:|mailto:)/i.test(target) || target.startsWith('#')) continue;
        if (/^(?:[a-z]:[\\/]|[\\/]|~[\\/]|file:)/i.test(target) || target.includes('..')) {
            fail(`${file}: link target \`${target}\` must stay inside the skills package`);
        }
        const resolved = path.resolve(dir, target.split('#')[0]);
        const relative = path.relative(skillsRoot, resolved);
        if (relative.startsWith('..') || path.isAbsolute(relative) || !fs.existsSync(resolved)) {
            fail(`${file}: link target \`${target}\` is not a file inside the skills package`);
        }
    }
    for (const match of source.matchAll(/(?:^|[\s`"'(（])((?:[a-z]:[\\/]|~[\\/]|file:\/\/)[^\s`"'()）]+)/gi)) {
        fail(`${file}: filesystem path \`${match[1]}\` must not appear in a published skill`);
    }
}

function runCli(args: string[]): string {
    return execFileSync(process.execPath, [cli, ...args], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

function commandTree(): Set<string> {
    const lines = runCli(['--help']).split(/\r?\n/);
    const start = lines.findIndex((line) => line.trim() === 'Commands:');
    if (start < 0) fail('could not find the `Commands:` section in `open-finder --help`');
    const commands = new Set<string>();
    for (let index = start + 1; index < lines.length; index += 1) {
        const line = lines[index];
        if (!line.trim()) break;
        const match = line.match(/^\s{2,}([a-z][a-z0-9-]*)\s{2,}/);
        if (match) commands.add(match[1]);
    }
    if (commands.size === 0) fail('no commands parsed from `open-finder --help`');
    return commands;
}

const flagCache = new Map<string, Set<string>>();
function flagsFor(command: string): Set<string> {
    const cached = flagCache.get(command);
    if (cached) return cached;
    const lines = runCli([command, '--help']).split(/\r?\n/);
    const start = lines.findIndex((line) => line.trim() === 'Options:');
    if (start < 0) fail(`could not find the \`Options:\` section in \`open-finder ${command} --help\``);
    const flags = new Set<string>(['--help']);
    for (let index = start + 1; index < lines.length; index += 1) {
        const line = lines[index];
        if (!line.trim() || /^\s{2,}[a-z][a-z0-9-]*\s{2,}/.test(line)) break;
        const match = line.match(/^\s+(--[a-z0-9-]+)/);
        if (match) flags.add(match[1]);
    }
    flagCache.set(command, flags);
    return flags;
}

type Frontmatter = Record<string, string>;

function parseFrontmatter(source: string, file: string): { meta: Frontmatter; body: string } {
    const lines = source.split(/\r?\n/);
    if (lines[0]?.trim() !== '---') fail(`${file}: frontmatter must start on line 1 with \`---\``);
    const end = lines.findIndex((line, index) => index > 0 && line.trim() === '---');
    if (end < 0) fail(`${file}: frontmatter is not closed with \`---\``);
    const meta: Frontmatter = {};
    for (const line of lines.slice(1, end)) {
        if (!line.trim()) continue;
        const match = line.match(/^([a-zA-Z][a-zA-Z0-9_-]*):\s*(.*)$/);
        if (!match) fail(`${file}: cannot parse frontmatter line \`${line}\``);
        meta[match[1]] = match[2].trim().replace(/^['"]|['"]$/g, '');
    }
    return { meta, body: lines.slice(end + 1).join('\n') };
}

function readJson(file: string): Record<string, unknown> {
    return JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
}

if (!fs.existsSync(cli)) {
    fail(`CLI build missing at ${cli}. Run \`pnpm run build:ts\` first.`);
}
if (!fs.existsSync(skillsRoot)) {
    fail('`packages/open-finder-skills/skills` is missing');
}

const commands = commandTree();
const app = readJson(appPackage);
const skillsManifest = readJson(skillsPackage);
if (skillsManifest.license !== 'Apache-2.0') fail('skills package `license` must be `Apache-2.0`');
if (skillsManifest.version !== app.version) {
    fail(`skills package version \`${skillsManifest.version}\` must match the app version \`${app.version}\``);
}
const files = skillsManifest.files;
if (!Array.isArray(files) || !files.includes('skills') || files.length > 2) {
    fail('skills package `files` must contain `skills` plus at most one readme');
}

const skillDirs = fs
    .readdirSync(skillsRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
if (skillDirs.length === 0) fail('no skill directories found');

let checkedInvocations = 0;
for (const dir of skillDirs) {
    const file = path.join(skillsRoot, dir, 'SKILL.md');
    if (!fs.existsSync(file)) fail(`${dir}: missing SKILL.md`);
    const source = fs.readFileSync(file, 'utf8');

    const { meta, body } = parseFrontmatter(source, `skills/${dir}/SKILL.md`);
    if (!meta.name) fail(`${dir}: frontmatter is missing \`name\``);
    if (!meta.description) fail(`${dir}: frontmatter is missing \`description\``);
    if (!/^[a-z0-9][a-z0-9-]*$/.test(meta.name)) fail(`${dir}: \`name\` must be kebab-case`);
    if (meta.name !== dir) fail(`${dir}: frontmatter \`name\` does not match the directory`);

    if (!/^##\s+Safety\b/m.test(body)) fail(`${dir}: missing a \`## Safety\` section`);
    if (!/confirm|确认/i.test(body)) fail(`${dir}: safety must require explicit confirmation before mutating commands`);
    assertNoOutOfPackageReferences(source, `skills/${dir}/SKILL.md`, path.join(skillsRoot, dir));

    const invocations = [...source.matchAll(/`(open-finder [^`\n]+)`/g)].map((match) => match[1].trim());
    if (invocations.length === 0) fail(`${dir}: no backticked \`open-finder\` invocation found`);

    const dryRunCommands = new Set<string>();
    const referencedMutating = new Set<string>();
    for (const invocation of invocations) {
        const tokens = invocation.split(/\s+/);
        const command = tokens[1];
        if (!commands.has(command)) fail(`${dir}: unknown command \`${command}\` in \`${invocation}\``);
        const allowed = flagsFor(command);
        const flags: string[] = [];
        for (const token of tokens.slice(2)) {
            if (token.startsWith('<')) continue;
            if (token.startsWith('--')) {
                const flag = token.split('=')[0];
                if (!allowed.has(flag)) fail(`${dir}: flag \`${flag}\` is not defined for \`${command}\``);
                flags.push(flag);
            }
            else {
                fail(`${dir}: unexpected positional token \`${token}\` in \`${invocation}\``);
            }
        }
        if (mutatingCommands.has(command)) {
            referencedMutating.add(command);
            if (flags.includes('--dry-run')) dryRunCommands.add(command);
        }
        checkedInvocations += 1;
    }

    for (const command of referencedMutating) {
        if (!dryRunCommands.has(command)) {
            fail(`${dir}: \`${command}\` must document a \`--dry-run\` invocation before execution`);
        }
    }

    console.log(`ok ${dir}: ${invocations.length} invocation(s)`);
}

console.log(`check-skills ok: ${skillDirs.length} skills, ${checkedInvocations} CLI invocations, ${commands.size} commands`);
