//! Node-level handler registries for vote target dispatch and upgrade migrations.
//!
//! Handler implementations live in their owning crates; this module wires them
//! into Vote and Update lifecycle at block processing time.

pub mod vote {
    use outbe_governance::vote_target::GovernanceVoteTarget;
    use outbe_update::vote_target::UpdateVoteTarget;
    use outbe_vote::handlers::{VoteTarget, VoteTargetRegistry};

    static UPDATE_VOTE_TARGET: UpdateVoteTarget = UpdateVoteTarget;
    static GOVERNANCE_VOTE_TARGET: GovernanceVoteTarget = GovernanceVoteTarget;
    static ACTIVE_VOTE_TARGETS: &[&dyn VoteTarget] =
        &[&UPDATE_VOTE_TARGET, &GOVERNANCE_VOTE_TARGET];
    static REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(ACTIVE_VOTE_TARGETS);

    /// Returns the compile-time vote target registry for executor wiring.
    pub fn registry() -> &'static VoteTargetRegistry {
        &REGISTRY
    }
}

pub mod update {
    use outbe_common::WorldwideDay;
    use outbe_primitives::block::BlockRuntimeContext;
    use outbe_primitives::error::Result;
    use outbe_update::handlers::{UpgradeHandler, UpgradeHandlerRegistry, UpgradeHandlers};
    use outbe_update::{ProtocolVersion, ScheduledUpdateInfo};

    /// Frozen protocol version of the bounded Lysis V1 PoC. Registering the
    /// migration does not arm a network schedule; OCM-26 owns the canonical
    /// fresh-devnet activation record.
    const OCOMP_POC_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::from_raw(1);

    struct OcompPocFreshDevnetHandler;

    impl UpgradeHandler for OcompPocFreshDevnetHandler {
        fn version(&self) -> ProtocolVersion {
            OCOMP_POC_PROTOCOL_VERSION
        }

        fn label(&self) -> &'static str {
            "ocomp-poc-fresh-devnet-profile"
        }

        fn handle(
            &self,
            ctx: &BlockRuntimeContext,
            _scheduled: &ScheduledUpdateInfo,
        ) -> Result<()> {
            ctx.with_checkpoint(|| {
                let mut tribute = ctx.contract::<outbe_tribute::TributeContract>();
                tribute.initialize_fresh_ocomp_profile()?;

                let mut fidelity = ctx.contract::<outbe_fidelity::FidelityContract>();
                fidelity.initialize_fresh_ocomp_profile()?;

                outbe_oracle::api::initialize_fresh_ocomp_profile(ctx.storage.clone())?;

                let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
                outbe_metadosis::initialize_fresh_ocomp_profile(ctx.storage.clone(), wwd)?;
                Ok(())
            })
        }
    }

    static OCOMP_POC_FRESH_DEVNET_HANDLER: OcompPocFreshDevnetHandler = OcompPocFreshDevnetHandler;

    /// Active upgrade handlers for this node binary.
    ///
    /// Append entries when a protocol version requires deterministic storage
    /// migration at activation height. Versions without a handler activate as
    /// version-only switches.
    static ACTIVE_UPGRADE_HANDLERS: UpgradeHandlers = &[&OCOMP_POC_FRESH_DEVNET_HANDLER];
    static REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(ACTIVE_UPGRADE_HANDLERS);

    /// Returns the compile-time upgrade handler registry for executor wiring.
    pub fn registry() -> &'static UpgradeHandlerRegistry {
        &REGISTRY
    }

    #[cfg(test)]
    mod tests {
        use alloy_primitives::{Address, U256};
        use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        use outbe_primitives::storage::StorageHandle;
        use outbe_update::handlers::UpgradeHandler;
        use outbe_update::schema::ScheduledUpdateStatus;
        use outbe_update::ScheduledUpdateInfo;

        use super::{registry, OcompPocFreshDevnetHandler, OCOMP_POC_PROTOCOL_VERSION};

        const ACTIVATION_HEIGHT: u64 = 32;
        const ACTIVATION_TIMESTAMP: u64 = 1_753_315_200;

        fn scheduled() -> ScheduledUpdateInfo {
            ScheduledUpdateInfo {
                proposal_id: U256::from(1),
                version: OCOMP_POC_PROTOCOL_VERSION,
                activation_height: ACTIVATION_HEIGHT,
                info: "OCOMP PoC fixture".into(),
                status: ScheduledUpdateStatus::Scheduled,
            }
        }

        fn context(storage: StorageHandle<'_>) -> BlockRuntimeContext<'_> {
            BlockRuntimeContext::new(
                BlockContext::new(
                    ACTIVATION_HEIGHT,
                    ACTIVATION_TIMESTAMP,
                    1,
                    Address::ZERO,
                    Vec::new(),
                ),
                storage,
            )
        }

        #[test]
        fn registered_ocomp_handler_initializes_all_owner_profiles_atomically() {
            let handlers = registry()
                .lookup(OCOMP_POC_PROTOCOL_VERSION)
                .collect::<Vec<_>>();
            assert_eq!(handlers.len(), 1);
            assert_eq!(handlers[0].label(), "ocomp-poc-fresh-devnet-profile");

            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                outbe_oracle::contract::OracleContract::new(storage.clone())
                    .register_pair("COEN", "0xUSD")
                    .unwrap();
                OcompPocFreshDevnetHandler
                    .handle(&context(storage.clone()), &scheduled())
                    .unwrap();

                assert!(
                    outbe_tribute::TributeContract::new(storage.clone())
                        .pre_admission_projection(outbe_common::WorldwideDay::from_timestamp(
                            ACTIVATION_TIMESTAMP
                        ),)
                        .unwrap()
                        .profile_ready
                );
                assert!(
                    outbe_fidelity::FidelityContract::new(storage.clone())
                        .ocomp_projection()
                        .unwrap()
                        .profile_ready
                );
                assert!(
                    outbe_oracle::api::ocomp_pre_admission_projection(
                        storage.clone(),
                        outbe_common::WorldwideDay::from_timestamp(ACTIVATION_TIMESTAMP),
                        U256::ZERO,
                        ACTIVATION_TIMESTAMP,
                    )
                    .unwrap()
                    .profile_ready
                );
                assert!(
                    outbe_metadosis::schema::MetadosisContract::new(storage)
                        .ocomp_pre_admission_projection(outbe_common::WorldwideDay::from_timestamp(
                            ACTIVATION_TIMESTAMP
                        ),)
                        .unwrap()
                        .initialized
                );
            });
        }

        #[test]
        fn handler_failure_rolls_back_profiles_initialized_before_oracle_validation() {
            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                assert!(OcompPocFreshDevnetHandler
                    .handle(&context(storage.clone()), &scheduled())
                    .is_err());

                let wwd = outbe_common::WorldwideDay::from_timestamp(ACTIVATION_TIMESTAMP);
                assert!(
                    !outbe_tribute::TributeContract::new(storage.clone())
                        .pre_admission_projection(wwd)
                        .unwrap()
                        .profile_ready
                );
                assert!(
                    !outbe_fidelity::FidelityContract::new(storage.clone())
                        .ocomp_projection()
                        .unwrap()
                        .profile_ready
                );
                assert!(
                    !outbe_metadosis::schema::MetadosisContract::new(storage)
                        .ocomp_pre_admission_projection(wwd)
                        .unwrap()
                        .initialized
                );
            });
        }
    }
}
