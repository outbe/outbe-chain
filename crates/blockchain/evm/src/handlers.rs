//! Node-level handler registries for vote target dispatch and upgrade migrations.
//!
//! Handler implementations live in their owning crates. This module wires them
//! into Vote and Update lifecycle at block processing time.

pub mod vote {
    use alloy_primitives::{Address, U256};
    use outbe_governance::vote_target::GovernanceVoteTarget;
    use outbe_l2registry::L2RegistryVoteTarget;
    use outbe_primitives::addresses::STABLECOIN_FACTORY_ADDRESS;
    use outbe_primitives::block::BlockRuntimeContext;
    use outbe_primitives::error::{PrecompileError, Result};
    use outbe_primitives::stablecoin::decode_canonical_stablecoin_create;
    use outbe_primitives::stablecoin_fork::STABLECOIN_CREATE_BOND;
    use outbe_primitives::storage::StorageHandle;
    use outbe_stablecoinfactory::{
        FactoryReservation, StablecoinFactoryApi, ValidatedStablecoinCreate,
    };
    use outbe_update::vote_target::UpdateVoteTarget;
    use outbe_vote::handlers::{
        TargetAdmission, TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry,
    };
    use outbe_vote::schema::ProposalStatus;

    static UPDATE_VOTE_TARGET: UpdateVoteTarget = UpdateVoteTarget;
    static GOVERNANCE_VOTE_TARGET: GovernanceVoteTarget = GovernanceVoteTarget;
    static L2_REGISTRY_VOTE_TARGET: L2RegistryVoteTarget = L2RegistryVoteTarget;
    static STABLECOIN_FACTORY_VOTE_TARGET: StablecoinFactoryVoteTarget =
        StablecoinFactoryVoteTarget;
    static ACTIVE_VOTE_TARGETS: &[&dyn VoteTarget] = &[
        &UPDATE_VOTE_TARGET,
        &GOVERNANCE_VOTE_TARGET,
        &L2_REGISTRY_VOTE_TARGET,
        &STABLECOIN_FACTORY_VOTE_TARGET,
    ];
    static REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(ACTIVE_VOTE_TARGETS);

    struct StablecoinFactoryVoteTarget;

    impl StablecoinFactoryVoteTarget {
        fn validate_payload(
            payload: &[u8],
            context: VoteTargetContext,
        ) -> Result<ValidatedStablecoinCreate> {
            let decoded = decode_canonical_stablecoin_create(payload)
                .map_err(|_| PrecompileError::Revert("non-canonical stablecoin payload".into()))?;
            if decoded.issuer != context.proposer {
                return Err(PrecompileError::Revert(
                    "stablecoin proposer must equal issuer".into(),
                ));
            }
            let (token_id, token) = outbe_primitives::stablecoin::predict_stablecoin(
                context.chain_id,
                STABLECOIN_FACTORY_ADDRESS,
                decoded.issuer,
                &decoded.ticker,
                outbe_primitives::addresses::STABLECOIN_ADDRESS_PREFIX,
            )
            .map_err(|_| PrecompileError::Revert("invalid stablecoin identity".into()))?;
            Ok(ValidatedStablecoinCreate {
                payload: decoded,
                token_id,
                token,
            })
        }
    }

    impl VoteTarget for StablecoinFactoryVoteTarget {
        fn target_module(&self) -> Address {
            STABLECOIN_FACTORY_ADDRESS
        }

        fn admission(&self) -> TargetAdmission {
            TargetAdmission::PublicBonded {
                amount: STABLECOIN_CREATE_BOND,
            }
        }

        fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
            Self::validate_payload(payload, context).map(|_| ())
        }

        fn reserve(
            &self,
            storage: StorageHandle<'_>,
            proposal_id: U256,
            payload: &[u8],
            context: VoteTargetContext,
        ) -> Result<()> {
            let validated =
                StablecoinFactoryApi::validate_create(storage.clone(), context.proposer, payload)?;
            StablecoinFactoryApi::reserve(
                storage,
                &FactoryReservation {
                    proposal_id,
                    token_id: validated.token_id,
                    ticker: validated.payload.ticker,
                    token: validated.token,
                },
            )
        }

        fn handle_approved(
            &self,
            ctx: &BlockRuntimeContext,
            proposal_id: U256,
            payload: &[u8],
            context: VoteTargetContext,
        ) -> Result<TargetExecutionOutcome> {
            match StablecoinFactoryApi::execute_approved(
                ctx.storage.clone(),
                proposal_id,
                context.proposer,
                payload,
                0,
            ) {
                Ok(_) => Ok(TargetExecutionOutcome::Applied),
                Err(error @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))) => {
                    Ok(TargetExecutionOutcome::Error {
                        reason: error.to_string(),
                    })
                }
                Err(error) => Err(error),
            }
        }

        fn handle_tally(
            &self,
            ctx: &BlockRuntimeContext,
            proposal_id: U256,
            payload: &[u8],
            context: VoteTargetContext,
            status: ProposalStatus,
        ) -> Result<TargetExecutionOutcome> {
            match status {
                ProposalStatus::Approved => {
                    self.handle_approved(ctx, proposal_id, payload, context)
                }
                ProposalStatus::Expired => {
                    StablecoinFactoryApi::release(ctx.storage.clone(), proposal_id)?;
                    Ok(TargetExecutionOutcome::Applied)
                }
                ProposalStatus::Pending | ProposalStatus::Rejected | ProposalStatus::Error => {
                    Ok(TargetExecutionOutcome::Applied)
                }
            }
        }
    }

    /// Returns the compile-time vote target registry for executor wiring.
    pub fn registry() -> &'static VoteTargetRegistry {
        &REGISTRY
    }

    #[cfg(test)]
    mod tests;
}

pub mod update {
    use outbe_update::handlers::{UpgradeHandlerRegistry, UpgradeHandlers};

    /// Active upgrade handlers for this node binary.
    ///
    /// Append entries when a protocol version requires deterministic storage
    /// migration at activation height. Versions without a handler activate as
    /// version-only switches.
    static ACTIVE_UPGRADE_HANDLERS: UpgradeHandlers = &[];
    static REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(ACTIVE_UPGRADE_HANDLERS);

    /// Returns the compile-time upgrade handler registry for executor wiring.
    pub fn registry() -> &'static UpgradeHandlerRegistry {
        &REGISTRY
    }

    #[cfg(test)]
    mod tests {
        use outbe_update::ProtocolVersion;

        #[test]
        fn ocomp_genesis_activation_registers_no_generic_update_handler() {
            assert!(super::registry()
                .lookup(ProtocolVersion::from_raw(1))
                .next()
                .is_none());
        }
    }
}
