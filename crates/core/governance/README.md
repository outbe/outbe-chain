# Governance

`outbe-governance` is the on-chain registry of normative texts (meta-canon,
canon) and improvement proposals (OIP, GIP). Status changes and canon writes
can still go through the authorities-gated precompile API. Approved OIP/GIP
records can also be materialized by the committee vote path.

The governance precompile address is
`0x0000000000000000000000000000000000001018` (`GOVERNANCE_ADDRESS`).

## Committee flow (OIP / GIP via vote)

Same process as
[`governance_oip_gip.feature`](../../testing/e2e-harness/features/governance_oip_gip.feature):

```text
validator -> outbe-cli vote propose (target=GOVERNANCE, JSON kind+text)
validators -> outbe-cli vote cast --yes
vote.begin_block (quorum) -> GovernanceVoteTarget
                           -> create_approved_oip / create_approved_gip
```

On approval the record is created already `Approved` (no Draft on this path).
Author is the vote proposal's proposer.

Voting rules, quorum, and deadlines live in
[`outbe-vote`](../../system/vote/README.md).

## Vote payload

```json
{"kind":"oip","text":"..."}
{"kind":"gip","text":"..."}
```

`text` must be non-empty and within the governance max text size.

## outbe-cli

Proposer and voters must be active validators. A validator may have at most one
pending vote proposal (`MAX_PENDING_PROPOSALS_PER_VALIDATOR = 1`), so concurrent
OIP and GIP proposals need two different proposers (as in the e2e feature).

```bash
VOTE_ADDR=0x000000000000000000000000000000000000EE0C
GOV_ADDR=0x0000000000000000000000000000000000001018

# Propose an OIP (validator-0)
outbe-cli --private-key "$V0_KEY" --rpc-url "$RPC_URL" vote propose \
  --target-module "$GOV_ADDR" \
  --payload '{"kind":"oip","text":"e2e oip body"}'

# Propose a GIP from a second validator while the OIP vote is still pending
outbe-cli --private-key "$V1_KEY" --rpc-url "$RPC_URL" vote propose \
  --target-module "$GOV_ADDR" \
  --payload '{"kind":"gip","text":"e2e gip body"}'

# Cast yes votes (2/3 of active validators)
outbe-cli --private-key "$V0_KEY" --rpc-url "$RPC_URL" vote cast --proposal-id 1 --yes
outbe-cli --private-key "$V1_KEY" --rpc-url "$RPC_URL" vote cast --proposal-id 1 --yes
outbe-cli --private-key "$V2_KEY" --rpc-url "$RPC_URL" vote cast --proposal-id 1 --yes

outbe-cli --rpc-url "$RPC_URL" vote status --proposal-id 1
```

After the voting deadline and begin-block tally, an approved proposal creates
governance id `1` for that kind. Read it back with `cast call` (or equivalent
`eth_call`):

```bash
cast call "$GOV_ADDR" \
  'getOip(uint256)((uint256,uint8,address,uint64,uint64,bytes32,string))' 1 \
  --rpc-url "$RPC_URL"

cast call "$GOV_ADDR" \
  'getGip(uint256)((uint256,uint8,address,uint64,uint64,bytes32,string))' 1 \
  --rpc-url "$RPC_URL"
```

Status `1` is `Approved`.
