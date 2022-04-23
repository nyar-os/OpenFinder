/**
 * Stable CLI exit codes and the machine-readable error shape.
 *
 * The N-API boundary throws `Error` with a `wae.error:<json>` payload. This
 * module converts that into `AppError` without inventing a second schema.
 */

/** Process exit codes. `USAGE` is produced by `@wae/commander` itself. */
export const EXIT = {
    /** Success. */
    OK: 0,
    /** Runtime failure that is not a missing capability. */
    FAILURE: 1,
    /** Parse / validation failure. */
    USAGE: 2,
    /** Structured capability failure (`unsupported_platform`, `not_implemented`, `dependency_blocked`). */
    CAPABILITY: 3,
} as const;

/** Error payload emitted by the native boundary. */
export interface ErrorPayload {
    code: string;
    capability?: string;
    platform?: string;
    message: string;
    /** File or directory related to an I/O failure. */
    path?: string;
}

/** Error thrown by the typed library and the CLI. */
export class AppError extends Error {
    readonly code: string;
    readonly capability?: string;
    readonly platform?: string;
    readonly path?: string;

    constructor(payload: ErrorPayload) {
        super(payload.message);
        this.name = 'AppError';
        this.code = payload.code;
        this.capability = payload.capability;
        this.platform = payload.platform;
        this.path = payload.path;
    }

    /** Exit code derived from the structured payload. */
    get exitCode(): number {
        return this.capability ? EXIT.CAPABILITY : EXIT.FAILURE;
    }

    /** Machine-readable form. */
    toJSON(): ErrorPayload {
        return {
            code: this.code,
            ...(this.capability ? { capability: this.capability } : {}),
            ...(this.platform ? { platform: this.platform } : {}),
            message: this.message,
            ...(this.path ? { path: this.path } : {}),
        };
    }
}

/** Prefix used by the native boundary when it throws a structured error. */
export const NATIVE_ERROR_PREFIX = 'wae.error:';

/** Whether the value is already an `AppError`. */
export function isAppError(value: unknown): value is AppError {
    return value instanceof AppError;
}

/**
 * Normalize an exception from the native boundary or the filesystem.
 *
 * The native boundary is authoritative. When it sends `wae.error:<json>`, the
 * payload is used as-is. Other exceptions become `native_unavailable` or
 * `internal_error` with a stable code so agents do not parse prose.
 */
export function fromNativeError(value: unknown): AppError {
    if (value instanceof AppError) return value;
    const message = value instanceof Error ? value.message : String(value);
    const index = message.indexOf(NATIVE_ERROR_PREFIX);
    if (index >= 0) {
        try {
            const payload = JSON.parse(message.slice(index + NATIVE_ERROR_PREFIX.length)) as Partial<ErrorPayload>;
            if (typeof payload.message === 'string' && typeof payload.code === 'string') {
                return new AppError({
                    code: payload.code,
                    ...(typeof payload.capability === 'string' ? { capability: payload.capability } : {}),
                    ...(typeof payload.platform === 'string' ? { platform: payload.platform } : {}),
                    message: payload.message,
                    ...(typeof payload.path === 'string' ? { path: payload.path } : {}),
                });
            }
        }
        catch {
            // Fall through to the generic error below.
        }
    }
    const lower = message.toLowerCase();
    if (lower.includes('cannot find module') || lower.includes('no native binding')) {
        return new AppError({
            code: 'native_unavailable',
            message:
                `${message}\nRun \`pnpm run build:napi\` from the repository root, ` +
                'or install the matching platform package.',
        });
    }
    return new AppError({ code: 'internal_error', message });
}
