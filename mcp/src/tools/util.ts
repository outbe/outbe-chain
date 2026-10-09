import { toJson } from "../format.js";

/** MCP text-content result. */
export function ok(value: unknown) {
  return { content: [{ type: "text" as const, text: toJson(value) }] };
}

export function okText(text: string) {
  return { content: [{ type: "text" as const, text }] };
}

export function fail(err: unknown) {
  const msg = err instanceof Error ? err.message : String(err);
  return { content: [{ type: "text" as const, text: `error: ${msg}` }], isError: true };
}

/** Wrap an async tool handler with uniform error reporting. */
export function handler<A>(fn: (args: A) => Promise<ReturnType<typeof ok>>) {
  return async (args: A) => {
    try {
      return await fn(args);
    } catch (e) {
      return fail(e);
    }
  };
}

export { view } from "../read.js";
