//! Pure block-acceptance rules for the application handler.
//!
//! These are the deterministic "what makes a proposed block invalid?" checks,
//! lifted out of `handler.rs`'s propose/verify event loop. They take an
//! immutable block plus the scheme/committee providers and return a `Result` -
//! no clock, no marshal, no runtime state - so they read and test as a
//! standalone validation layer. `handler` calls them; the tests below exercise
//! them directly.

use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;
use commonware_consensus::types::Round;
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PublicKey};
use outbe_primitives::{addresses::REWARDS_ADDRESS, system_tx::OcompLifecycleActivation};

use crate::block::ConsensusBlock;
use crate::committee_provider::CommitteeProvider;
use crate::digest::Digest;
use crate::hybrid::HybridSchemeProvider;

/// Chain policy and leader lookup dependencies for one immutable validation request.
pub(crate) struct SystemTxLeaderValidationContext<'a> {
    pub(crate) round: Round,
    pub(crate) proposer: &'a PublicKey,
    pub(crate) chain_id: u64,
    pub(crate) ocomp_lifecycle_activation: OcompLifecycleActivation,
    pub(crate) certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub(crate) committee_provider: &'a CommitteeProvider,
}

mod leader;

/// A non-genesis block's beneficiary must be the protocol `REWARDS_ADDRESS`.
pub(crate) fn validate_rewards_beneficiary(block: &ConsensusBlock) -> Result<(), String> {
    if block.number() > 0 && block.header().beneficiary() != REWARDS_ADDRESS {
        return Err(format!(
            "non-genesis block beneficiary must be REWARDS_ADDRESS {}: got {}",
            REWARDS_ADDRESS,
            block.header().beneficiary()
        ));
    }
    Ok(())
}

/// The proposed block must extend the Simplex context parent: matching parent
/// digest and height (`parent.number() + 1`, or `1` at genesis).
pub(crate) fn validate_context_parent_binding(
    block: &ConsensusBlock,
    parent_block: Option<&ConsensusBlock>,
    context_parent_digest: Digest,
    genesis_hash: B256,
) -> Result<(), String> {
    if block.parent_digest() != context_parent_digest {
        return Err(format!(
            "proposed block parent digest {} does not match Simplex context parent {}",
            block.parent_digest().0,
            context_parent_digest.0
        ));
    }

    let expected_number = if context_parent_digest.0 == genesis_hash {
        1
    } else {
        let parent = parent_block.ok_or_else(|| {
            "non-genesis Simplex context parent was not resolved for height validation".to_string()
        })?;
        if parent.digest() != context_parent_digest {
            return Err(format!(
                "resolved parent digest {} does not match Simplex context parent {}",
                parent.digest().0,
                context_parent_digest.0
            ));
        }
        parent.number().checked_add(1).ok_or_else(|| {
            "parent block number overflow while validating proposal height".to_string()
        })?
    };

    if block.number() != expected_number {
        return Err(format!(
            "proposed block number {} does not extend Simplex parent height {}",
            block.number(),
            expected_number.saturating_sub(1)
        ));
    }

    Ok(())
}

/// Validate the begin/end system-transaction set: layout, the mandatory
/// CertifiedParentAccounting parent-hash binding, BoundaryOutcome consistency
/// with the header artifact, per-tx signature-hash binding, and that every
/// system tx is signed by the consensus leader's EVM address.
pub(crate) fn validate_system_tx_leader_binding_for_activation(
    block: &ConsensusBlock,
    context: SystemTxLeaderValidationContext<'_>,
) -> Result<(), String> {
    let raw_block = block.clone().into_inner().into_block();
    leader::validate_gas_limit(&raw_block.header)?;
    let (layout, artifacts) = leader::validate_layout(
        &raw_block.body,
        &raw_block.header,
        context.ocomp_lifecycle_activation,
    )?;
    if layout.system_tx_count() == 0 {
        return Ok(());
    }
    leader::validate_parent_accounting(&layout, &raw_block.header)?;
    leader::validate_boundary_outcome(&layout, &artifacts)?;
    leader::validate_envelopes(&layout, &raw_block.header, context.chain_id)?;
    let expected = leader::consensus_leader_evm_address(&context)?;
    leader::validate_signers(&layout, expected)
}

#[cfg(test)]
mod tests {
    use super::{
        validate_context_parent_binding, validate_rewards_beneficiary,
        validate_system_tx_leader_binding_for_activation, SystemTxLeaderValidationContext,
    };
    use crate::digest::Digest;
    use crate::dkg_manager;
    use crate::test_fixtures::*;
    use alloy_primitives::{Bytes, B256};
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::Signer as _;
    use outbe_primitives::reshare_artifact::{
        encode_consensus_header_artifact, ConsensusHeaderArtifact,
    };
    use outbe_primitives::signer::OutbeEvmSigner;
    use outbe_primitives::system_tx::{OcompLifecycleActivation, SystemTxInputV2};

    fn steady_system_inputs(parent_hash: B256) -> Vec<SystemTxInputV2> {
        vec![
            SystemTxInputV2::CertifiedParentAccounting {
                metadata: finalized_metadata(parent_hash),
            },
            SystemTxInputV2::LateFinalizeCredits {
                artifact: Default::default(),
            },
            SystemTxInputV2::CycleTick,
            SystemTxInputV2::RewardsGemDelivery,
            SystemTxInputV2::OracleSlashWindow,
            SystemTxInputV2::HookEvents,
        ]
    }

    fn leader_context<'a>(
        round: Round,
        proposer: &'a commonware_cryptography::bls12381::PublicKey,
        providers: (
            &'a crate::hybrid::HybridSchemeProvider<
                commonware_cryptography::bls12381::primitives::variant::MinSig,
            >,
            &'a crate::committee_provider::CommitteeProvider,
        ),
    ) -> SystemTxLeaderValidationContext<'a> {
        SystemTxLeaderValidationContext {
            round,
            proposer,
            chain_id: outbe_primitives::chain::CHAIN_ID,
            ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
            certificate_scheme_provider: providers.0,
            committee_provider: providers.1,
        }
    }

    #[test]
    fn rewards_beneficiary_rejects_non_genesis_mismatch() {
        let block = block_with_number_and_parent(1, B256::ZERO);
        let error = validate_rewards_beneficiary(&block)
            .expect_err("non-genesis beneficiary must be rewards escrow");
        assert!(error.contains("beneficiary must be REWARDS_ADDRESS"));
    }

    #[test]
    fn context_parent_binding_accepts_direct_child() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(8, parent.block_hash());

        validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
            .expect("direct child extends Simplex context parent");
    }

    #[test]
    fn context_parent_binding_rejects_wrong_parent_digest() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(8, B256::from([0x44; 32]));

        let error =
            validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
                .expect_err("child must bind header parent to Simplex context parent");
        assert!(error.contains("does not match Simplex context parent"));
    }

    #[test]
    fn context_parent_binding_rejects_height_gap() {
        let parent = block_with_number(7);
        let child = block_with_number_and_parent(9, parent.block_hash());

        let error =
            validate_context_parent_binding(&child, Some(&parent), parent.digest(), B256::ZERO)
                .expect_err("child height must be parent height plus one");
        assert!(error.contains("does not extend Simplex parent height"));
    }

    #[test]
    fn context_parent_binding_accepts_genesis_parent_for_block_one() {
        let genesis_hash = B256::from([0x55; 32]);
        let child = block_with_number_and_parent(1, genesis_hash);

        validate_context_parent_binding(&child, None, Digest(genesis_hash), genesis_hash)
            .expect("block 1 extends genesis parent");
    }

    #[test]
    fn system_tx_validation_rejects_missing_mandatory_kind_before_engine_status() {
        let (keys, _) = participants();
        let validator_set = validator_set_from_keys(&keys);
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_number(1);

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(Epoch::new(0), View::new(1)),
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect_err("block 1 must carry mandatory CycleTick system tx");

        assert!(error.contains("invalid system tx set"));
    }

    fn assert_fixture_leader_binding(
        secret_bytes: [u8; 32],
        leader_index: usize,
        epoch: Epoch,
        expected_message: &str,
    ) {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes(secret_bytes).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[leader_index] = signer.address();
        let (scheme_provider, committee_provider) = leader_binding_providers(epoch, &validator_set);
        let block = block_with_system_tx(&signer);

        validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(epoch, View::new(1)),
                &keys[leader_index].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect(expected_message);
    }

    #[test]
    fn system_tx_leader_binding_accepts_consensus_leader_address() {
        assert_fixture_leader_binding(
            [7u8; 32],
            0,
            Epoch::new(0),
            "system tx signer matches consensus leader EVM address",
        );
    }

    #[test]
    fn system_tx_leader_binding_accepts_payload_builder_visible_gas_plan() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::ZERO;
        let block = block_with_gas_planned_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: Bytes::new(),
                inputs: steady_system_inputs(parent_hash),
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
            30_000_000,
        );

        validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(Epoch::new(0), View::new(1)),
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect("validator must accept the payload builder's visible gas plan");
    }

    #[test]
    fn system_tx_leader_binding_rejects_bootstrap_gas_limit_after_block_one() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::ZERO;
        let block = block_with_gas_planned_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: Bytes::new(),
                inputs: steady_system_inputs(parent_hash),
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
            outbe_primitives::system_tx::BOOTSTRAP_BLOCK_GAS_LIMIT,
        );

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(Epoch::new(0), View::new(1)),
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect_err("500M is valid only at block 1");
        assert!(error.contains("protocol gas limit"));
    }

    #[test]
    fn system_tx_leader_binding_uses_the_manifest_activation_height() {
        const ACTIVATION_HEIGHT: u64 = 32;

        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::ZERO;
        let block = block_with_gas_planned_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: ACTIVATION_HEIGHT,
                parent_hash,
                extra_data: Bytes::new(),
                inputs: vec![
                    SystemTxInputV2::CertifiedParentAccounting {
                        metadata: finalized_metadata(parent_hash),
                    },
                    SystemTxInputV2::LateFinalizeCredits {
                        artifact: Default::default(),
                    },
                    SystemTxInputV2::OcompLifecycleBegin,
                    SystemTxInputV2::CycleTick,
                    SystemTxInputV2::RewardsGemDelivery,
                    SystemTxInputV2::OracleSlashWindow,
                    SystemTxInputV2::HookEvents,
                    SystemTxInputV2::OcompTerminalRequest,
                ],
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
            30_000_000,
        );
        let round = Round::new(Epoch::new(0), View::new(1));

        validate_system_tx_leader_binding_for_activation(
            &block,
            SystemTxLeaderValidationContext {
                ocomp_lifecycle_activation: OcompLifecycleActivation::at_block(ACTIVATION_HEIGHT),
                ..leader_context(
                    round,
                    &keys[0].public_key(),
                    (&scheme_provider, &committee_provider),
                )
            },
        )
        .expect("consensus verifier must accept the active payload layout at H");

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                round,
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect_err("the same payload must be invalid when OCOMP is not armed");
        assert!(error.contains("active system tx set mismatch"));
    }

    #[test]
    fn system_tx_leader_binding_uses_epoch_registered_committee() {
        assert_fixture_leader_binding(
            [9u8; 32],
            1,
            Epoch::new(1),
            "epoch-scoped committee maps current leader to EVM signer",
        );
    }

    #[test]
    fn system_tx_leader_binding_rejects_non_leader_signer() {
        let (keys, _) = participants();
        let leader_signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let non_leader_signer = OutbeEvmSigner::from_secret_bytes([8u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = leader_signer.address();
        validator_set.addresses[1] = non_leader_signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_system_tx(&non_leader_signer);

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(Epoch::new(0), View::new(1)),
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect_err("non-leader system tx signer must be rejected");
        assert!(error.contains("does not match consensus leader EVM address"));
    }

    #[test]
    fn system_tx_validation_rejects_wrong_chain_id_before_engine_status() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let block = block_with_system_tx(&signer);

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            SystemTxLeaderValidationContext {
                chain_id: outbe_primitives::chain::CHAIN_ID + 1,
                ..leader_context(
                    Round::new(Epoch::new(0), View::new(1)),
                    &keys[0].public_key(),
                    (&scheme_provider, &committee_provider),
                )
            },
        )
        .expect_err("wrong active chain id must be rejected before Engine status");
        assert!(error.contains("system tx signature_hash mismatch"));
    }

    #[test]
    fn system_tx_validation_rejects_finalization_parent_hash_mismatch_before_engine_status() {
        let (keys, _) = participants();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::from([0x11; 32]);
        let wrong_hash = B256::from([0x22; 32]);
        let block = block_with_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: Bytes::new(),
                inputs: steady_system_inputs(wrong_hash),
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
        );

        let error = validate_system_tx_leader_binding_for_activation(&block, leader_context(Round::new(Epoch::new(0), View::new(1)), &keys[0].public_key(), (&scheme_provider, &committee_provider)))
        .expect_err(
            "CertifiedParentAccounting metadata must bind to header parent hash before Engine status",
        );
        assert!(error.contains("CertifiedParentAccounting metadata hash must match block parent"));
    }

    #[test]
    fn system_tx_validation_rejects_boundary_calldata_mismatch_before_engine_status() {
        let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
        let signer = OutbeEvmSigner::from_secret_bytes([7u8; 32]).unwrap();
        let mut validator_set = validator_set_from_keys(&keys);
        validator_set.addresses[0] = signer.address();
        let header_artifact =
            dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                epoch: Epoch::new(0),
                validator_set: &validator_set,
                output: &output,
                is_full_dkg: true,
                dkg_cycle: 0,
                freeze_height: 0,
                planned_activation_height: 0,
                vrf_material_version: 0,
                is_validator_set_change: true,
                tee_expired_target_exclusions: Vec::new(),
            })
            .unwrap();
        let mut tx_artifact = header_artifact.clone();
        tx_artifact.planned_activation_height =
            tx_artifact.planned_activation_height.saturating_add(1);

        let (scheme_provider, committee_provider) =
            leader_binding_providers(Epoch::new(0), &validator_set);
        let parent_hash = B256::from([0x33; 32]);
        let block = block_with_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: encode_consensus_header_artifact(
                    &ConsensusHeaderArtifact::BoundaryOutcome(header_artifact),
                )
                .expect("header artifact encodes"),
                inputs: vec![
                    SystemTxInputV2::CertifiedParentAccounting {
                        metadata: finalized_metadata(parent_hash),
                    },
                    SystemTxInputV2::LateFinalizeCredits {
                        artifact: Default::default(),
                    },
                    SystemTxInputV2::CycleTick,
                    SystemTxInputV2::RewardsGemDelivery,
                    SystemTxInputV2::BoundaryOutcome {
                        artifact: tx_artifact,
                    },
                    SystemTxInputV2::OracleSlashWindow,
                    SystemTxInputV2::HookEvents,
                ],
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
        );

        let error = validate_system_tx_leader_binding_for_activation(
            &block,
            leader_context(
                Round::new(Epoch::new(0), View::new(1)),
                &keys[0].public_key(),
                (&scheme_provider, &committee_provider),
            ),
        )
        .expect_err("BoundaryOutcome calldata must bind to header artifact before Engine status");
        assert!(error.contains("BoundaryOutcome system tx artifact mismatch"));
    }
}
