use super::*;

#[cucumber::then("the snapshot FullNode preserves the copied Lysis result and canonical state")]
pub(super) fn snapshot_fullnode_preserves_copied_result(world: &mut crate::world::World) {
    snapshot_fullnode_preserves_copied_result_checked(world).expect("copied chain and OCOMP data");
}

pub(super) fn snapshot_fullnode_preserves_copied_result_checked(
    world: &crate::world::World,
) -> eyre::Result<()> {
    let evidence = world
        .state
        .offline_snapshot
        .as_ref()
        .ok_or_else(|| eyre!("snapshot evidence"))?;
    let slot = world.validators.joiner_index();
    let port = world.validators.http_port(slot);
    let cut = &evidence.cut_canonical;
    ensure!(
        world.rpc.wait_finalized_at_least(port, cut.number + 1, 180),
        "recipient did not catch advancing chain"
    );
    ensure!(
        world
            .rpc
            .block_hash(port, cut.number)
            .ok_or_else(|| eyre!("recipient cut block"))?
            .parse::<alloy_primitives::B256>()?
            == cut.hash.parse::<alloy_primitives::B256>()?,
        "recipient canonical cut changed"
    );
    let primary_root = world
        .rpc
        .state_root(world.validators.primary_port(), cut.number)
        .ok_or_else(|| eyre!("primary cut state root"))?;
    ensure!(
        world.rpc.state_root(port, cut.number) == Some(primary_root),
        "recipient cut EVM state differs"
    );
    let job: alloy_primitives::B256 = evidence.copied_result.job_id.parse()?;
    let bytes = std::fs::read(super::super::local_result_path(world, slot, job))?;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(&bytes, &limits)?;
    ensure!(
        result.job_id == job
            && hex::encode(result.result_digest(&limits)?) == evidence.copied_result.digest,
        "copied completed result changed"
    );
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .ok_or_else(|| eyre!("actual Lysis activation"))?;
    ensure!(
        world
            .rpc
            .finalized_ocomp_certified_generation_on(port, activation)
            == world.state.ocomp_certified_generation,
        "recipient certified generation differs"
    );
    let local_actions = super::super::result_nod_actions_on(world, slot, job);
    ensure!(
        local_actions == super::super::result_nod_actions_on(world, 0, job),
        "recipient saved NOD bodies differ"
    );
    ensure!(
        recipient_identity(world)? == evidence.identity_placed,
        "ordinary launch changed resident identity"
    );
    Ok(())
}

// Harness-only fragment, called after existing capacity completion and
// contributors_are_paid, BEFORE the next-day request changes current generation.
// Pass the copied generation/payout evidence and queue_sequence captured at E.
// Existing armProceedsForTest fixture remains part of this scenario's disclosure.
alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ISnapshotPayoutRead {
        struct ContributorLeaf { address owner; uint256 sourceTributeId; uint256 nominal; }
        struct ContributorRound { uint256 amount; uint32 contributorCount; uint256 paidSoFar; uint32 paidLeafCount; }
        function contributorPayoutRound(uint32 worldwideDay) external view returns (ContributorRound memory);
        function payContributorBatch(uint32 worldwideDay, uint32 startIndex, ContributorLeaf[] leaves, bytes32[] proof) external;
    }
}

pub(super) struct SnapshotOwnerSample<T> {
    pub(super) before: u64,
    pub(super) observation: Result<Option<T>, String>,
    pub(super) after: u64,
}

// Recognize only an adjacent, unchanged-root CE marker at the observed RPC head.
// The RPC diagnostic is not a stable wire format: unknown spellings fail closed.
pub(super) fn snapshot_forward_ce_mismatch(error: &str, before: u64, after: u64) -> bool {
    fn fields<'a>(text: &'a str, names: &[&str]) -> Option<Vec<&'a str>> {
        let values: Vec<_> = text.split(", ").collect();
        if values.len() != names.len() {
            return None;
        }
        values
            .into_iter()
            .zip(names)
            .map(|(value, name)| value.strip_prefix(*name)?.strip_prefix(": "))
            .collect()
    }
    let parsed = (|| -> Option<()> {
        let detail = error.strip_prefix(
            "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required ExactParentIdentity { ",
        )?;
        let (required, marker) = detail.split_once(" }, marker FinalizedMarker { ")?;
        let required = fields(
            required,
            &[
                "commitment_scheme_version",
                "block_number",
                "block_hash",
                "root",
            ],
        )?;
        let marker = fields(
            marker.strip_suffix(" }")?,
            &[
                "commitment_scheme_version",
                "height",
                "block_hash",
                "parent_block_hash",
                "parent_root",
                "new_root",
            ],
        )?;
        let required_scheme = required[0].parse::<u32>().ok()?;
        let marker_scheme = marker[0].parse::<u32>().ok()?;
        let required_height = required[1].parse::<u64>().ok()?;
        let marker_height = marker[1].parse::<u64>().ok()?;
        let required_hash = required[2].parse::<alloy_primitives::B256>().ok()?;
        let required_root = required[3].parse::<alloy_primitives::B256>().ok()?;
        let marker_hash = marker[2].parse::<alloy_primitives::B256>().ok()?;
        let parent_hash = marker[3].parse::<alloy_primitives::B256>().ok()?;
        let parent_root = marker[4].parse::<alloy_primitives::B256>().ok()?;
        let new_root = marker[5].parse::<alloy_primitives::B256>().ok()?;
        let height_range =
            required_height <= before && before <= marker_height && marker_height == after;
        let same_scheme = required_scheme == 1 && required_scheme == marker_scheme;
        let next_marker = required_height.checked_add(1) == Some(marker_height);
        let same_parent = parent_hash == required_hash && marker_hash != required_hash;
        let unchanged_root = parent_root == required_root && new_root == required_root;
        if !(height_range && same_scheme && next_marker) {
            return None;
        }
        (same_parent && unchanged_root).then_some(())
    })();
    parsed.is_some()
}

// None requests a fresh complete observation. It never means an absent owner.
pub(super) fn snapshot_owner_decision<T>(
    sample: SnapshotOwnerSample<T>,
) -> Result<Option<T>, String> {
    match sample.observation {
        Err(error) => {
            if snapshot_forward_ce_mismatch(&error, sample.before, sample.after) {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Ok(None) => Err("missing materialized owner".to_owned()),
        Ok(Some(value)) => {
            if sample.after < sample.before {
                Err("owner observation head regressed".to_owned())
            } else if sample.after > sample.before {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
    }
}

pub(super) fn snapshot_observe_owner<T>(
    deadline: std::time::Instant,
    mut sample: impl FnMut() -> eyre::Result<SnapshotOwnerSample<T>>,
    mut now: impl FnMut() -> std::time::Instant,
    mut wait: impl FnMut(),
) -> eyre::Result<T> {
    let mut last = "no owner observation".to_owned();
    loop {
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        let observed = sample()?;
        let outcome = match &observed.observation {
            Ok(Some(_)) => "present",
            Ok(None) => "missing",
            Err(error) => error.as_str(),
        };
        last = format!(
            "before={} after={} result={outcome}",
            observed.before, observed.after,
        );
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        if let Some(value) =
            snapshot_owner_decision(observed).map_err(|error| eyre!("{error}; {last}"))?
        {
            return Ok(value);
        }
        eprintln!("snapshot_owner_observation_retry {last}");
        wait();
    }
}

pub(super) fn snapshot_materialized_owner(
    world: &crate::world::World,
    port: u16,
    owner: alloy_primitives::Address,
    deadline: std::time::Instant,
) -> eyre::Result<(Vec<u8>, crate::internal::eth::INod::NodData)> {
    snapshot_observe_owner(
        deadline,
        || {
            let before = world
                .rpc
                .head(port)
                .ok_or_else(|| eyre!("head before owner read"))?;
            let observation = world.rpc.materialized_nod_for_owner(port, owner);
            // Preserve the whole Result until head movement has been observed.
            let after = world.rpc.head(port).ok_or_else(|| match &observation {
                Err(error) => eyre!("head after owner read unavailable; owner read error: {error}"),
                _ => eyre!("head after owner read unavailable"),
            })?;
            Ok(SnapshotOwnerSample {
                before,
                observation,
                after,
            })
        },
        std::time::Instant::now,
        || {
            std::thread::sleep(
                std::time::Duration::from_millis(250)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        },
    )
    .map_err(|error| eyre!("materialized owner port={port} owner={owner:#x}: {error}"))
}

pub(super) fn snapshot_public_nod_proofs(
    world: &crate::world::World,
    generation: &crate::world::rpc::OcompCertifiedGenerationV1,
) -> eyre::Result<Vec<serde_json::Value>> {
    use alloy_sol_types::SolValue;
    use outbe_ocomp_protocol::{
        list::streaming_ordered_list_membership_proof,
        profile::poc_schema_limits,
        result::{ActiveNodSetV1, NodMembershipProofV1},
        ListKind,
    };
    let primary = world.validators.primary_port();
    let slot = world.validators.joiner_index();
    let recipient = world.validators.http_port(slot);
    let actions = super::super::result_nod_actions_on(world, slot, generation.job_id);
    ensure!(
        actions == super::super::result_nod_actions_on(world, 0, generation.job_id),
        "copied local Nod actions differ"
    );
    ensure!(
        actions.len() == generation.nod_count as usize,
        "copied Nod population differs"
    );
    ensure!(
        crate::internal::nod_reference::nod_root(&actions) == generation.nod_root,
        "local actions do not bind canonical Nod root"
    );
    let limits = poc_schema_limits();
    let records = actions
        .iter()
        .map(|a| a.encode_canonical_record(&limits))
        .collect::<Result<Vec<_>, _>>()?;
    let authority = ActiveNodSetV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_root: generation.nod_root,
        nod_count: generation.nod_count,
    };
    let mut proofs = Vec::new();
    // One observer budget covers all owners and both nodes. Retries never extend it.
    let owner_deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    for (ordinal, action) in actions.iter().enumerate() {
        let proof = NodMembershipProofV1 {
            job_id: generation.job_id,
            program_semantics_hash: generation.program_semantics_hash,
            worldwide_day: generation.worldwide_day,
            generation: generation.generation,
            nod_ordinal: ordinal.try_into()?,
            action: action.clone(),
            membership_siblings: streaming_ordered_list_membership_proof(
                ListKind::NodActions,
                generation.nod_count,
                ordinal.try_into()?,
                &records,
                limits.max_bounded_bytes,
            )?,
        };
        proof.verify_against(&authority, &limits)?;
        let local = snapshot_materialized_owner(world, recipient, action.owner, owner_deadline)?;
        let canonical = snapshot_materialized_owner(world, primary, action.owner, owner_deadline)?;
        ensure!(
            local.0 == action.nod_id.as_slice()
                && local.0 == canonical.0
                && local.1.abi_encode() == canonical.1.abi_encode(),
            "FullNode Nod body differs"
        );
        proofs.push(serde_json::json!({"nod_id":hex::encode(action.nod_id),"proof":hex::encode(proof.encode_canonical_record(&limits)?),"body":hex::encode(local.1.abi_encode())}));
    }
    Ok(proofs)
}

#[cfg(test)]
mod snapshot_owner_observation_tests {
    use super::*;
    use std::{
        cell::Cell,
        collections::VecDeque,
        time::{Duration, Instant},
    };

    const CE_RACE: &str = "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required ExactParentIdentity { commitment_scheme_version: 1, block_number: 521, block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7, root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0 }, marker FinalizedMarker { commitment_scheme_version: 1, height: 522, block_hash: 0xb0d63f8c96229446dec5685a47eb1bd04a1299e86ca4ff11abc192fc7b445bb9, parent_block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7, parent_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0, new_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0 }";

    fn sample(
        before: u64,
        observation: Result<Option<u64>, String>,
        after: u64,
    ) -> SnapshotOwnerSample<u64> {
        SnapshotOwnerSample {
            before,
            observation,
            after,
        }
    }

    fn observe(samples: Vec<SnapshotOwnerSample<u64>>, seconds: u64) -> (eyre::Result<u64>, usize) {
        let started = Instant::now();
        let clock = Cell::new(started);
        let mut samples = VecDeque::from(samples);
        let mut calls = 0;
        let result = snapshot_observe_owner(
            started + Duration::from_secs(seconds),
            || {
                calls += 1;
                Ok(samples.pop_front().expect("unexpected observation retry"))
            },
            || clock.get(),
            || clock.set(clock.get() + Duration::from_secs(1)),
        );
        (result, calls)
    }

    #[test]
    fn crossed_forward_ce_race_restarts_the_whole_owner_observation() {
        let (result, calls) = observe(
            vec![
                sample(521, Err(CE_RACE.to_owned()), 522),
                sample(522, Ok(Some(7)), 522),
            ],
            5,
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[test]
    fn stable_rpc_head_with_adjacent_ce_race_restarts_the_whole_owner_observation() {
        let (result, calls) = observe(
            vec![
                sample(522, Err(CE_RACE.to_owned()), 522),
                sample(522, Ok(Some(7)), 522),
            ],
            5,
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[test]
    fn crossed_forward_success_is_discarded_before_accepting_a_stable_tuple() {
        let (result, calls) = observe(
            vec![
                sample(521, Ok(Some(99)), 522),
                sample(522, Ok(Some(7)), 522),
            ],
            5,
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[test]
    fn stale_or_regressing_head_ce_mismatch_is_an_error() {
        for (before, after) in [(521, 521), (522, 521)] {
            let (result, calls) = observe(vec![sample(before, Err(CE_RACE.to_owned()), after)], 5);
            assert!(result.unwrap_err().to_string().contains(CE_RACE));
            assert_eq!(calls, 1);
        }
        let (result, calls) = observe(vec![sample(522, Ok(Some(7)), 521)], 5);
        assert!(result.unwrap_err().to_string().contains("regressed"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unrelated_head_movement_does_not_authorize_retry_of_an_old_ce_error() {
        let (result, calls) = observe(vec![sample(600, Err(CE_RACE.to_owned()), 601)], 5);
        assert!(result.unwrap_err().to_string().contains(CE_RACE));
        assert_eq!(calls, 1);
    }

    #[test]
    fn ce_marker_behind_observed_head_does_not_authorize_retry() {
        let (result, calls) = observe(vec![sample(522, Err(CE_RACE.to_owned()), 523)], 5);
        assert!(result.unwrap_err().to_string().contains(CE_RACE));
        assert_eq!(calls, 1);
    }

    #[test]
    fn generic_rpc_decode_uniqueness_and_missing_owner_fail_even_during_progress() {
        for error in [
            "eth_call failed: connection reset",
            "ABI decode failed: invalid body",
            "balanceOf returned 2, expected exactly one",
            "owner has more than one materialized NOD",
            "execution reverted: index out of bounds",
            "compressed-entity tree unavailable: exact parent mismatch",
        ] {
            let (result, calls) = observe(vec![sample(521, Err(error.to_owned()), 522)], 5);
            assert!(result.unwrap_err().to_string().contains(error));
            assert_eq!(calls, 1);
        }
        let (result, calls) = observe(vec![sample(521, Ok(None), 522)], 5);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing materialized owner"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unrelated_or_malformed_ce_identities_are_not_retried() {
        let bad_hash = format!("0x{}", "11".repeat(32));
        let errors = [
            CE_RACE.replace("commitment_scheme_version: 1", "commitment_scheme_version: 2"),
            CE_RACE.replace("height: 522", "height: 521"),
            CE_RACE.replace("height: 522", "height: 523"),
            CE_RACE.replace("height: 522", "height: 520"),
            CE_RACE.replace("FinalizedMarker { commitment_scheme_version: 1", "FinalizedMarker { commitment_scheme_version: 2"),
            CE_RACE.replace("parent_block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7", &format!("parent_block_hash: {bad_hash}")),
            CE_RACE.replace("parent_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0", &format!("parent_root: {bad_hash}")),
            CE_RACE.replace("new_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0", &format!("new_root: {bad_hash}")),
            CE_RACE.replace("height: 522", "height: broken"),
        ];
        for error in errors {
            let (result, calls) = observe(vec![sample(521, Err(error.clone()), 522)], 5);
            assert!(result.unwrap_err().to_string().contains(&error));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn repeated_forward_errors_exhaust_one_deadline_with_last_heads_and_error() {
        let (result, calls) = observe(
            vec![
                sample(521, Err(CE_RACE.to_owned()), 522),
                sample(521, Err(CE_RACE.to_owned()), 522),
            ],
            2,
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains("deadline exhausted"));
        assert!(error.contains("before=521 after=522"));
        assert!(error.contains(CE_RACE));
        assert_eq!(calls, 2);
    }
}
