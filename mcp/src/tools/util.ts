import { humanizeReturn, toJson } from "../format.js";
import { type Ctx, readView } from "../chain.js";
import { resolveContract } from "../registry.js";

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

/** Read a view method and return the humanized result object. */
export async function view(
  ctx: Ctx,
  contract: string,
  method: string,
  args: unknown[] = [],
): Promise<unknown> {
  const entry = resolveContract(contract);
  const { fn, result } = await readView(ctx, entry, method, args);
  return humanizeReturn(fn, result);
}
