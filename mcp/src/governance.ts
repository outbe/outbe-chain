import type { Ctx } from "./chain.js";
import { view } from "./read.js";
import { PROPOSAL_STATUS, type ProposalStatusName, proposalStatusCode } from "./registry.js";

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
  return { total: Number(total), offset, limit, items: metas };
}
