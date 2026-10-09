import type { Ctx } from "./chain.js";
import { view } from "./read.js";
import { PROPOSAL_STATUS, type ProposalStatusName, proposalStatusCode, proposalStatusName } from "./registry.js";

/**
 * Normalise the proposal status.
 *
 * `format.ts` already renders `status` as `{code, name}` for `IGovernance.*`
 * structs. This function only backfills the name when a caller gives a raw code.
 */
export function annotateProposal(p: unknown): Record<string, unknown> {
  const r = { ...(p as Record<string, unknown>) };
  if (typeof r.status === "number" || typeof r.status === "bigint") {
    const code = Number(r.status);
    r.status = { code, name: proposalStatusName(code) };
  }
  return r;
}

export async function listProposals(
  ctx: Ctx,
  kind: "Oip" | "Gip",
  args: { author?: string; status?: ProposalStatusName; offset?: number; limit?: number },
): Promise<{ total: number; offset: number; limit: number; items: unknown[] }> {
  const { author, status } = args;
  if ((author === undefined) === (status === undefined)) {
    throw new Error(
      `provide exactly one of \`author\` or \`status\` (${PROPOSAL_STATUS.join("|")})`,
    );
  }
  const offset = args.offset ?? 0;
  const limit = args.limit ?? 100;
  const byAuthor = author !== undefined;
  const suffix = byAuthor ? "ByAuthor" : "ByStatus";
  const key = byAuthor ? author : proposalStatusCode(status as ProposalStatusName);
  const [metas, total] = await Promise.all([
    view(ctx, "governance", `get${kind}s${suffix}`, [key, offset, limit]) as Promise<
      unknown[]
    >,
    view(ctx, "governance", `${kind.toLowerCase()}Count${suffix}`, [key]),
  ]);
  return { total: Number(total), offset, limit, items: metas.map(annotateProposal) };
}
