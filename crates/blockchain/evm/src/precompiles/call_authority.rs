//! Binds EVM command identity to existing domain mutation capabilities.
//! Economic transitions remain owned by Cycle and Metadosis.
use alloy_primitives::{Address, U256};
use outbe_metadosis::config::OcompForkInstallV1;
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    storage::{
        metadosis_cycle_allocation_binding, metadosis_init_genesis_binding,
        metadosis_late_settlement_binding, metadosis_ocomp_lifecycle_begin_binding,
        metadosis_ocomp_terminal_request_binding, metadosis_process_ready_binding,
        metadosis_verified_vote_binding, MetadosisCertifiedFinalityBinding,
        MetadosisMutationEntitlements, MetadosisMutationPurposeTag,
    },
};

/// Admission for the public result-vote command. The same decision must drive
/// dispatch and capabilities; selector-only routing could open an unauthorized
/// mutation frame before the command gets a chance to reject its call mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResultVoteCall {
    NotVote,
    Entitled,
    WrongMode,
}

impl ResultVoteCall {
    pub(super) fn classify(
        address: Address,
        data: &[u8],
        is_static: bool,
        value: U256,
        lifecycle_active: bool,
    ) -> Self {
        if !lifecycle_active
            || address != outbe_ocomp_protocol::abi::METADOSIS_ADDRESS
            || !data.get(..4).is_some_and(|selector| {
                selector == outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR
            })
        {
            return Self::NotVote;
        }
        if is_static || !value.is_zero() {
            Self::WrongMode
        } else {
            Self::Entitled
        }
    }
}

pub(super) struct MetadosisMutationCall<'a> {
    pub(super) address: Address,
    pub(super) data: &'a [u8],
    pub(super) caller: Address,
    pub(super) is_static: bool,
    pub(super) value: U256,
    pub(super) ocomp_lifecycle_active: bool,
    pub(super) result_vote: ResultVoteCall,
    pub(super) chain_id: u64,
    pub(super) block_number: u64,
    pub(super) timestamp: u64,
    pub(super) cycle_active_utc_day: Option<u32>,
    pub(super) preloaded_certified_state_root: Option<alloy_primitives::B256>,
    pub(super) ocomp_fork_install: Option<&'a OcompForkInstallV1>,
}

pub(super) fn metadosis_mutation_entitlements(
    call: MetadosisMutationCall<'_>,
) -> MetadosisMutationEntitlements {
    let MetadosisMutationCall {
        address,
        data,
        caller,
        is_static,
        value,
        ocomp_lifecycle_active,
        result_vote,
        chain_id,
        block_number,
        timestamp,
        preloaded_certified_state_root,
        ..
    } = call;
    use MetadosisMutationPurposeTag as Purpose;

    if is_static || !value.is_zero() {
        return MetadosisMutationEntitlements::NONE;
    }
    if result_vote == ResultVoteCall::Entitled {
        return MetadosisMutationEntitlements::exact(
            Purpose::VerifiedResultVote,
            metadosis_verified_vote_binding(data),
        );
    }
    if address != OUTBE_SYSTEM_TX_ADDRESS || caller != SYSTEM_ADDRESS {
        return MetadosisMutationEntitlements::NONE;
    }
    let Ok(input) = crate::system_tx::SystemTxInputV2::decode(data) else {
        return MetadosisMutationEntitlements::NONE;
    };
    match input {
        crate::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata }
            if metadata.proof_kind
                == outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization =>
        {
            let Some(finalized_state_root) = preloaded_certified_state_root else {
                return MetadosisMutationEntitlements::NONE;
            };
            let certified = MetadosisCertifiedFinalityBinding::new(
                chain_id,
                block_number,
                metadata.finalized_block_number,
                metadata.finalized_block_hash,
                finalized_state_root,
            );
            MetadosisMutationEntitlements::exact(Purpose::CertifiedFinality, certified.binding())
        }
        crate::system_tx::SystemTxInputV2::LateFinalizeCredits { .. } => {
            MetadosisMutationEntitlements::exact(
                Purpose::CertifiedFinality,
                metadosis_late_settlement_binding(chain_id, block_number, timestamp),
            )
        }
        // Exact command identities cover genesis, one contiguous daily
        // allocation when due, and the single hourly Metadosis pass. The cursor
        // is read from Cycle storage by the provider; it is never accepted from
        // calldata. Multi-day gaps grant no missed-day economic authority.
        crate::system_tx::SystemTxInputV2::CycleTick => cycle_tick_entitlements(&call),
        crate::system_tx::SystemTxInputV2::OcompLifecycleBegin if ocomp_lifecycle_active => {
            ocomp_lifecycle_entitlements(&call)
        }
        crate::system_tx::SystemTxInputV2::OcompTerminalRequest if ocomp_lifecycle_active => {
            MetadosisMutationEntitlements::exact(
                Purpose::OcompLifecycle,
                metadosis_ocomp_terminal_request_binding(chain_id, block_number, timestamp),
            )
        }
        _ => MetadosisMutationEntitlements::NONE,
    }
}

fn cycle_tick_entitlements(call: &MetadosisMutationCall<'_>) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    let chain_id = call.chain_id;
    let block_number = call.block_number;
    let timestamp = call.timestamp;
    let cycle_active_utc_day = call.cycle_active_utc_day;
    let ocomp_fork_install = call.ocomp_fork_install;
    let genesis_activation_height =
        ocomp_fork_install.map_or(1, |install| install.activation_height);
    let mut entitlements = MetadosisMutationEntitlements::NONE;
    if block_number == genesis_activation_height {
        entitlements = entitlements.union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_init_genesis_binding(chain_id, block_number, timestamp),
        ));
    }
    let Some(active_utc_day) = cycle_active_utc_day else {
        return entitlements;
    };
    let block_utc_day = outbe_primitives::time::timestamp_to_date_key(timestamp);
    let Ok(day_action) = outbe_cycle::handler::protocol_day_action(active_utc_day, block_utc_day)
    else {
        return entitlements;
    };
    if let outbe_cycle::handler::ProtocolDayAction::SettlePrevious { day } = day_action {
        entitlements = entitlements.union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_cycle_allocation_binding(
                chain_id,
                block_number,
                outbe_primitives::time::date_key_to_utc_timestamp(day),
            ),
        ));
    }
    entitlements.union(MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(chain_id, block_number, timestamp),
    ))
}

fn ocomp_lifecycle_entitlements(call: &MetadosisMutationCall<'_>) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    let chain_id = call.chain_id;
    let block_number = call.block_number;
    let timestamp = call.timestamp;
    let ocomp_fork_install = call.ocomp_fork_install;
    let lifecycle = MetadosisMutationEntitlements::exact(
        Purpose::OcompLifecycle,
        metadosis_ocomp_lifecycle_begin_binding(chain_id, block_number, timestamp),
    );
    let Some(install) =
        ocomp_fork_install.filter(|install| install.activation_height == block_number)
    else {
        return lifecycle;
    };
    let Ok(install_hash) = install.install_hash(&outbe_metadosis::config::poc_schema_limits())
    else {
        return lifecycle;
    };
    lifecycle.union(MetadosisMutationEntitlements::exact(
        Purpose::ForkProfile,
        install_hash,
    ))
}

/// The Cycle cursor prepares capabilities for a protocol command, not for any
/// user payload that happens to share its encoding. Check identity before decode.
pub(super) fn is_protocol_cycle_call(address: Address, caller: Address, data: &[u8]) -> bool {
    address == OUTBE_SYSTEM_TX_ADDRESS
        && caller == SYSTEM_ADDRESS
        && matches!(
            crate::system_tx::SystemTxInputV2::decode(data),
            Ok(crate::system_tx::SystemTxInputV2::CycleTick)
        )
}
