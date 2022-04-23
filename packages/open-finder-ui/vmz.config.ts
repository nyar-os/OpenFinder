import { defineConfig } from '@vmz/vmz';

/**
 * The WebView host loads local static HTML from `packages/open-finder/dist/gui`.
 * Builtin profile `static` → assembly `web-static`.
 */
export default defineConfig({
    delivery: {
        default: 'static',
        profiles: {
            static: {
                host: 'browser',
                assembly: 'web-static',
            },
        },
    },
});
