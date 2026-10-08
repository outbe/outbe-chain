use super::*;

pub(super) fn snapshot_public_kind(
    tx: &serde_json::Value,
    copied_queue_sequence: u64,
    worldwide_day: u32,
) -> eyre::Result<Option<bool>> {
    use crate::internal::addresses;
    use alloy_primitives::{address, Address};
    use alloy_sol_types::SolCall;
    use outbe_ocomp_protocol::{
        abi::{
            decode_protected_materialize_certified_nods_calldata,
            MATERIALIZE_CERTIFIED_NODS_SELECTOR,
        },
        profile::poc_schema_limits,
    };
    let factory = address!("0000000000000000000000000000000000001015");
    let limits = poc_schema_limits();
    let Some(to) = tx["to"].as_str().and_then(|s| s.parse::<Address>().ok()) else {
        return Ok(None);
    };
    if to != addresses::NOD_FACTORY_ADDR && to != factory {
        return Ok(None);
    }
    let input = hex::decode(
        tx["input"]
            .as_str()
            .ok_or_else(|| eyre!("missing calldata"))?
            .trim_start_matches("0x"),
    )?;
    let materialization = to == addresses::NOD_FACTORY_ADDR
        && input.get(..4) == Some(MATERIALIZE_CERTIFIED_NODS_SELECTOR.as_slice());
    let paying = to == factory
        && input.get(..4)
            == Some(ISnapshotPayoutRead::payContributorBatchCall::SELECTOR.as_slice());
    if !materialization && !paying {
        return Ok(None);
    }
    if materialization
        && decode_protected_materialize_certified_nods_calldata(&input, &limits)?.queue_sequence
            != copied_queue_sequence
    {
        return Ok(None);
    }
    if paying
        && ISnapshotPayoutRead::payContributorBatchCall::abi_decode(&input)?.worldwideDay
            != worldwide_day
    {
        return Ok(None);
    }
    Ok(Some(materialization))
}

pub(super) fn snapshot_public_transactions(
    world: &crate::world::World,
    cut_height: u64,
    through: u64,
    copied_queue_sequence: u64,
    worldwide_day: u32,
) -> eyre::Result<Vec<serde_json::Value>> {
    use crate::internal::eth;
    let slot = world.validators.joiner_index();
    let url = world.rpc.url(world.validators.primary_port());
    let recipient_signer = eth::address_of(&world.validators.get(slot).evm_key()?)
        .ok_or_else(|| eyre!("recipient EVM address"))?;
    let mut delegate_owners = std::collections::BTreeMap::new();
    for index in 0..world.validators.size() {
        let validator = eth::address_of(&world.validators.get(index).evm_key()?)
            .ok_or_else(|| eyre!("validator EVM address"))?;
        delegate_owners.insert(
            world.ocomp.ocomp_delegate_address(index.try_into()?)?,
            validator,
        );
    }
    let scan = SnapshotPublicScan {
        url: &url,
        recipient_signer,
        delegate_owners,
        copied_queue_sequence,
        worldwide_day,
    };
    let mut transactions = Vec::new();
    let mut materialization_count = 0;
    let mut payout_count = 0;
    let mut first = cut_height
        .checked_add(1)
        .ok_or_else(|| eyre!("cut overflow"))?;
    while first <= through {
        let last = first.saturating_add(63).min(through);
        let blocks = eth::blocks_with_transactions(&url, first, last, 64)
            .ok_or_else(|| eyre!("public transaction scan failed"))?;
        for (height, block) in (first..=last).zip(blocks) {
            for tx in block["transactions"]
                .as_array()
                .ok_or_else(|| eyre!("missing public transactions"))?
            {
                let Some((materialization, observed)) = scan.observe(tx, &block, height)? else {
                    continue;
                };
                if materialization {
                    materialization_count += 1;
                } else {
                    payout_count += 1;
                }
                transactions.push(observed);
            }
        }
        first = last.checked_add(1).ok_or_else(|| eyre!("scan overflow"))?;
    }
    ensure!(
        materialization_count > 0 && payout_count > 0,
        "missing actual post-cut materialization or payout transactions"
    );
    Ok(transactions)
}

pub(super) fn assert_snapshot_delegate(
    url: &str,
    signer: alloy_primitives::Address,
    validator: alloy_primitives::Address,
    height: u64,
) -> eyre::Result<()> {
    use crate::internal::{addresses, eth};
    // Same existing OCOMP role value as verify_ocomp_delegate_bindings.
    let parent = height
        .checked_sub(1)
        .ok_or_else(|| eyre!("genesis transaction"))?;
    let active = eth::read_call_at_result(
        url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getActiveValidatorsCall {},
        parent,
    )
    .map_err(|e| eyre!(e))?;
    let declared = eth::read_call_at_result(
        url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::getDelegateCall { validator, role: 2 },
        parent,
    )
    .map_err(|e| eyre!(e))?;
    let resolved = eth::read_call_at_result(
        url,
        addresses::VS_ADDR,
        &eth::IValidatorSet::resolveValidatorCall { role: 2, signer },
        parent,
    )
    .map_err(|e| eyre!(e))?;
    ensure!(
        active.contains(&validator) && declared == signer && resolved == validator,
        "sender lacks existing active-validator delegate binding at transaction parent"
    );
    Ok(())
}

pub(super) fn snapshot_public_effects(
    world: &crate::world::World,
    cut_height: u64,
    copied_queue_sequence: u64,
    generation: &crate::world::rpc::OcompCertifiedGenerationV1,
    payout: &crate::world::state::ContributorPayoutEvidenceV1,
) -> eyre::Result<serde_json::Value> {
    use crate::internal::eth;
    use alloy_primitives::{address, Address, U256};
    use alloy_sol_types::SolValue;
    use eyre::{ensure, eyre};
    let primary = world.validators.primary_port();
    let slot = world.validators.joiner_index();
    let recipient = world.validators.http_port(slot);
    let url = world.rpc.url(primary);
    let recipient_url = world.rpc.url(recipient);
    let after = payout
        .after
        .as_ref()
        .ok_or_else(|| eyre!("contributors_are_paid has not completed"))?;
    ensure!(
        payout.worldwide_day == generation.worldwide_day,
        "wrong copied payout day"
    );
    let completed = world
        .rpc
        .completed_nod_materialization(primary, generation)
        .ok_or_else(|| eyre!("copied generation is not fully materialized"))?;
    let through = after.height.max(completed.completion_block_number);
    ensure!(
        world.rpc.wait_finalized_at_least(recipient, through, 180),
        "FullNode has not reached public completion"
    );
    ensure!(
        world
            .rpc
            .completed_nod_materialization(recipient, generation)
            == Some(completed.clone()),
        "FullNode materialization completion differs"
    );
    ensure!(
        world.rpc.state_root(recipient, after.height) == Some(format!("{:#x}", after.state_root)),
        "FullNode payout state root differs"
    );
    ensure!(
        world.rpc.block_hash(recipient, after.height) == Some(format!("{:#x}", after.block_hash)),
        "FullNode payout checkpoint differs"
    );

    let proofs = snapshot_public_nod_proofs(world, generation)?;
    let factory = address!("0000000000000000000000000000000000001015");
    let round_call = ISnapshotPayoutRead::contributorPayoutRoundCall {
        worldwideDay: payout.worldwide_day,
    };
    let round = eth::read_call_at_result(&recipient_url, factory, &round_call, after.height)
        .map_err(|e| eyre!(e))?;
    let primary_round =
        eth::read_call_at_result(&url, factory, &round_call, after.height).map_err(|e| eyre!(e))?;
    ensure!(
        round.abi_encode() == primary_round.abi_encode()
            && round.amount == payout.amount
            && round.paidSoFar == payout.expected_paid
            && round.contributorCount as usize == payout.contributors.len()
            && round.paidLeafCount == round.contributorCount,
        "FullNode payout round differs or remains pending"
    );
    let balance_at = |account: Address| -> eyre::Result<U256> {
        let raw = eth::raw_json_with_params(
            &recipient_url,
            "eth_getBalance",
            serde_json::json!([format!("{account:#x}"), format!("0x{:x}", after.height)]),
        )
        .ok_or_else(|| eyre!("FullNode checkpoint balance unavailable"))?;
        Ok(U256::from_str_radix(
            raw.as_str()
                .ok_or_else(|| eyre!("balance encoding"))?
                .trim_start_matches("0x"),
            16,
        )?)
    };
    let balances = payout
        .contributors
        .iter()
        .map(|c| balance_at(c.owner))
        .collect::<eyre::Result<Vec<_>>>()?;
    ensure!(
        balances == after.owner_balances && balance_at(factory)? == after.factory_balance,
        "FullNode public payout balances differ"
    );

    let transactions = snapshot_public_transactions(
        world,
        cut_height,
        through,
        copied_queue_sequence,
        payout.worldwide_day,
    )?;
    Ok(
        serde_json::json!({"generation":generation,"completion":completed,"payout_checkpoint":after,
        "payout_round":hex::encode(round.abi_encode()),"recipient_balances":balances,
        "nod_proofs":proofs,"post_cut_transactions":transactions,
        "fixture":"Existing armProceedsForTest/distribute fixture; public payout execution and balances are observed, not injected."}),
    )
}

#[cucumber::then(
    "the snapshot FullNode observes the same completed public actions without submitting them"
)]
pub(super) fn snapshot_observes_public_actions(world: &mut crate::world::World) {
    snapshot_observes_public_actions_checked(world).expect("copied public effects on FullNode");
}

pub(super) fn snapshot_observes_public_actions_checked(
    world: &crate::world::World,
) -> eyre::Result<()> {
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    let pending: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("pending-native-cut.json"))?)?;
    let queue = pending["execution"]["queue_sequence"]
        .as_u64()
        .ok_or_else(|| eyre!("native cut queue sequence"))?;
    let snapshot = world
        .state
        .offline_snapshot
        .as_ref()
        .ok_or_else(|| eyre!("snapshot evidence"))?;
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("certified generation"))?;
    let payout = world
        .state
        .ocomp_contributor_payout
        .as_ref()
        .ok_or_else(|| eyre!("completed payout"))?;
    let observed = snapshot_public_effects(
        world,
        snapshot.cut_canonical.number,
        queue,
        generation,
        payout,
    )?;
    std::fs::write(
        root.join("public-effects.json"),
        serde_json::to_vec_pretty(&observed)?,
    )?;
    Ok(())
}

struct SnapshotPublicScan<'a> {
    url: &'a str,
    recipient_signer: alloy_primitives::Address,
    delegate_owners: BTreeMap<alloy_primitives::Address, alloy_primitives::Address>,
    copied_queue_sequence: u64,
    worldwide_day: u32,
}

impl SnapshotPublicScan<'_> {
    fn observe(
        &self,
        tx: &serde_json::Value,
        block: &serde_json::Value,
        height: u64,
    ) -> eyre::Result<Option<(bool, serde_json::Value)>> {
        use crate::internal::eth;
        use alloy_primitives::Address;
        let Some(materialization) =
            snapshot_public_kind(tx, self.copied_queue_sequence, self.worldwide_day)?
        else {
            return Ok(None);
        };
        let signer: Address = tx["from"]
            .as_str()
            .ok_or_else(|| eyre!("missing sender"))?
            .parse()?;
        ensure!(
            signer != self.recipient_signer,
            "recipient submitted copied public effects"
        );
        let hash = tx["hash"]
            .as_str()
            .ok_or_else(|| eyre!("missing transaction hash"))?;
        let receipt = eth::receipt_json(self.url, hash)
            .ok_or_else(|| eyre!("missing public effect receipt"))?;
        if receipt["status"].as_str() != Some("0x1") {
            return Ok(None);
        }
        ensure!(
            receipt["blockHash"] == block["hash"] && receipt["transactionHash"] == tx["hash"],
            "receipt is not this canonical transaction"
        );
        let validator = *self
            .delegate_owners
            .get(&signer)
            .ok_or_else(|| eyre!("successful sender is not an existing OCOMP delegate"))?;
        assert_snapshot_delegate(self.url, signer, validator, height)?;
        Ok(Some((
            materialization,
            serde_json::json!({"block_number":height,"transaction":tx,"receipt":receipt,"validator":validator,"delegate":signer}),
        )))
    }
}
