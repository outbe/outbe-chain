//! Node-level handler registries for vote target dispatch and upgrade migrations.
//!
//! Handler implementations live in their owning crates; this module wires them
//! into Vote and Update lifecycle at block processing time.

pub mod vote {
    use alloy_primitives::{Address, U256};
    use outbe_governance::vote_target::GovernanceVoteTarget;
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
    static STABLECOIN_FACTORY_VOTE_TARGET: StablecoinFactoryVoteTarget =
        StablecoinFactoryVoteTarget;
    static ACTIVE_VOTE_TARGETS: &[&dyn VoteTarget] = &[
        &UPDATE_VOTE_TARGET,
        &GOVERNANCE_VOTE_TARGET,
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
            let active = crate::protocol_version::resolve(&storage)?;
            if !outbe_primitives::stablecoin_fork::stablecoin_v1_is_active(active.raw()) {
                return Err(PrecompileError::Revert(
                    "Stablecoin V1 is not active".into(),
                ));
            }
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
            let active = crate::protocol_version::resolve(&ctx.storage)?;
            match StablecoinFactoryApi::execute_approved(
                ctx.storage.clone(),
                proposal_id,
                context.proposer,
                payload,
                u64::from(active.raw()),
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
    mod tests {
        use super::*;
        use outbe_primitives::addresses::UPDATE_ADDRESS;
        use outbe_primitives::block::BlockContext;
        use outbe_primitives::stablecoin::{
            encode_canonical_stablecoin_create, StablecoinCreatePayload,
        };
        use outbe_primitives::stablecoin_fork::STABLECOIN_V1_PROTOCOL_VERSION_RAW;
        use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
        use outbe_stablecoinfactory::StablecoinFactoryContract;

        fn payload(issuer: Address) -> Vec<u8> {
            encode_canonical_stablecoin_create(&StablecoinCreatePayload {
                issuer,
                name: "Example Dollar".into(),
                ticker: "EXUSD".into(),
                iso4217: 840,
                decimals: 6,
                supply_cap: U256::from(1_000_000u64),
                policy_id: U256::from(1u64),
            })
            .unwrap()
        }

        fn context(issuer: Address) -> VoteTargetContext {
            VoteTargetContext {
                proposer: issuer,
                attached_value: STABLECOIN_CREATE_BOND,
                block_number: 7,
                chain_id: 1,
            }
        }

        fn activate(storage: &StorageHandle<'_>) {
            storage
                .sstore(
                    UPDATE_ADDRESS,
                    U256::ZERO,
                    U256::from(STABLECOIN_V1_PROTOCOL_VERSION_RAW),
                )
                .unwrap();
        }

        #[test]
        fn factory_target_has_exact_bond_and_reserves_only_after_activation() {
            let issuer = Address::repeat_byte(0x11);
            let raw = payload(issuer);
            let target = registry().lookup(STABLECOIN_FACTORY_ADDRESS).unwrap();
            assert_eq!(
                target.admission(),
                TargetAdmission::PublicBonded {
                    amount: STABLECOIN_CREATE_BOND
                }
            );
            target.validate(&raw, context(issuer)).unwrap();
            assert!(target
                .validate(&raw, context(Address::repeat_byte(0x22)))
                .is_err());

            let mut provider = HashMapStorageProvider::new(1);
            let storage = StorageHandle::new(&mut provider);
            assert!(target
                .reserve(storage.clone(), U256::from(1u64), &raw, context(issuer))
                .is_err());
            activate(&storage);
            target
                .reserve(storage.clone(), U256::from(1u64), &raw, context(issuer))
                .unwrap();
            let factory = StablecoinFactoryContract::new(storage);
            assert!(factory.reservations.exists(U256::from(1u64)).unwrap());
        }

        #[test]
        fn factory_target_approved_expired_error_and_fatal_paths_are_distinct() {
            let issuer = Address::repeat_byte(0x11);
            let raw = payload(issuer);
            let target = registry().lookup(STABLECOIN_FACTORY_ADDRESS).unwrap();

            let mut approved_provider = HashMapStorageProvider::new(1);
            let approved_storage = StorageHandle::new(&mut approved_provider);
            activate(&approved_storage);
            target
                .reserve(
                    approved_storage.clone(),
                    U256::from(1u64),
                    &raw,
                    context(issuer),
                )
                .unwrap();
            let approved_ctx = BlockRuntimeContext::new(
                BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
                approved_storage.clone(),
            );
            assert_eq!(
                target
                    .handle_tally(
                        &approved_ctx,
                        U256::from(1u64),
                        &raw,
                        context(issuer),
                        ProposalStatus::Approved,
                    )
                    .unwrap(),
                TargetExecutionOutcome::Applied
            );
            assert_eq!(
                StablecoinFactoryContract::new(approved_storage)
                    .token_count()
                    .unwrap(),
                U256::from(1u64)
            );

            let mut expired_provider = HashMapStorageProvider::new(1);
            let expired_storage = StorageHandle::new(&mut expired_provider);
            activate(&expired_storage);
            target
                .reserve(
                    expired_storage.clone(),
                    U256::from(2u64),
                    &raw,
                    context(issuer),
                )
                .unwrap();
            let expired_ctx = BlockRuntimeContext::new(
                BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
                expired_storage.clone(),
            );
            assert_eq!(
                target
                    .handle_tally(
                        &expired_ctx,
                        U256::from(2u64),
                        &raw,
                        context(issuer),
                        ProposalStatus::Expired,
                    )
                    .unwrap(),
                TargetExecutionOutcome::Applied
            );
            assert!(!StablecoinFactoryContract::new(expired_storage)
                .reservations
                .exists(U256::from(2u64))
                .unwrap());

            let mut error_provider = HashMapStorageProvider::new(1);
            let error_storage = StorageHandle::new(&mut error_provider);
            activate(&error_storage);
            target
                .reserve(
                    error_storage.clone(),
                    U256::from(3u64),
                    &raw,
                    context(issuer),
                )
                .unwrap();
            let error_ctx = BlockRuntimeContext::new(
                BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
                error_storage.clone(),
            );
            assert!(matches!(
                target
                    .handle_tally(
                        &error_ctx,
                        U256::from(3u64),
                        b"{",
                        context(issuer),
                        ProposalStatus::Approved,
                    )
                    .unwrap(),
                TargetExecutionOutcome::Error { .. }
            ));
            assert!(StablecoinFactoryContract::new(error_storage)
                .reservations
                .exists(U256::from(3u64))
                .unwrap());

            let mut fatal_provider = HashMapStorageProvider::new(1);
            let fatal_storage = StorageHandle::new(&mut fatal_provider);
            activate(&fatal_storage);
            target
                .reserve(
                    fatal_storage.clone(),
                    U256::from(4u64),
                    &raw,
                    context(issuer),
                )
                .unwrap();
            let fatal_factory = StablecoinFactoryContract::new(fatal_storage.clone());
            let fatal_reservation = fatal_factory
                .reservations
                .get(U256::from(4u64))
                .unwrap()
                .unwrap();
            fatal_factory
                .pending_address
                .clear(&fatal_reservation.token)
                .unwrap();
            let fatal_ctx = BlockRuntimeContext::new(
                BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
                fatal_storage,
            );
            assert!(matches!(
                target.handle_tally(
                    &fatal_ctx,
                    U256::from(4u64),
                    &raw,
                    context(issuer),
                    ProposalStatus::Approved,
                ),
                Err(PrecompileError::Fatal(_))
            ));
        }
    }
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
}
