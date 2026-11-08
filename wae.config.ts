/**
 * Build and release configuration for OpenFinder.
 *
 * Pure literals only: `wae` parses this file statically and never executes it.
 * Build order is fixed: native → ts → ui → skills → staging.
 */
export default {
    native: {
        crate: 'open-finder-napi',
        cdylib: 'open_finder_napi',
        targets: [
            {
                platform: 'win32',
                arch: 'x64',
                triple: 'x86_64-pc-windows-msvc',
                package: '@wae/open-finder-win32-x64',
                nodeFile: 'open-finder.win32-x64.node',
                dir: 'packages/open-finder-win32-x64',
            },
        ],
    },
    packages: [
        {
            name: '@wae/open-finder-win32-x64',
            dir: 'packages/open-finder-win32-x64',
            os: ['win32'],
            cpu: ['x64'],
            main: 'open-finder.win32-x64.node',
        },
    ],
    build: {
        ts: { packages: ['@wae/open-finder'] },
        ui: { package: '@wae/open-finder-ui' },
        stage: [
            { from: 'packages/open-finder-ui/dist/static', to: 'packages/open-finder/dist/gui' },
        ],
        outputs: ['packages/open-finder/dist/cli.js', 'packages/open-finder/dist/gui/index.html'],
    },
    skills: {
        packages: [
            {
                dir: 'packages/open-finder-skills/skills',
                packageJson: 'packages/open-finder-skills/package.json',
                appPackageJson: 'packages/open-finder/package.json',
                cliEntry: 'packages/open-finder/dist/cli.js',
                commandName: 'open-finder',
            },
        ],
    },
    release: {
        registry: 'https://registry.npmjs.org',
        tagPattern: '^v(\\d+\\.\\d+\\.\\d+)$',
        packages: [
            { name: '@wae/open-finder', dir: 'packages/open-finder' },
            { name: '@wae/open-finder-skills', dir: 'packages/open-finder-skills' },
            { name: '@wae/open-finder-win32-x64', dir: 'packages/open-finder-win32-x64' },
        ],
    },
    ci: {
        repo: 'nyar-os/OpenFinder',
        workflows: ['check-rust.yml', 'check-typescript.yml', 'publish-npm.yml'],
        requiredJobs: ['check-rust', 'check-typescript'],
    },
    install: {
        shortcutDefault: true,
    },
    audit: {
        actor: { kind: 'flag', id: 'flag', source: 'flag' },
    },
};
