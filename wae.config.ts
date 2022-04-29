/**
 * Build and release configuration for OpenFinder.
 *
 * This file is pure data so it can be parsed statically. `wae build` reads it
 * to order native → TypeScript → UI → skills validation → staging.
 */
export default {
    native: {
        crate: 'open-finder-napi',
        target: 'win32-x64',
        cdylib: 'open_finder_napi',
        artifact: 'open-finder.win32-x64.node',
        platformPackage: '@wae/open-finder-win32-x64',
    },
    packages: {
        native: {
            dir: 'packages/open-finder-win32-x64',
            os: ['win32'],
            cpu: ['x64'],
        },
    },
    build: {
        order: ['native', 'typescript', 'ui', 'skills'],
    },
    skills: {
        dir: 'packages/open-finder-skills/skills',
        check: 'scripts/check-skills.ts',
    },
    release: {
        packages: ['@wae/open-finder', '@wae/open-finder-skills', '@wae/open-finder-win32-x64'],
        tag: 'vX.Y.Z',
    },
    ci: {
        workflows: ['check-rust.yml', 'check-typescript.yml'],
    },
};
