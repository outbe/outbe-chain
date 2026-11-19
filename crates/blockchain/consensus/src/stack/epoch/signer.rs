use super::super::*;

pub struct EpochValidationCommittee<'a> {
    pub epoch: Epoch,
    pub participants: &'a commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub validator_set: &'a validators::ValidatorSet,
    pub recovered_boundary: Option<&'a DkgBoundaryArtifact>,
}

pub struct EpochValidationProviders<'a> {
    pub vrf_materials: &'a VrfMaterialProvider<MinSig>,
    pub certificate_scheme: &'a HybridSchemeProvider<MinSig>,
    pub committee: &'a CommitteeProvider,
}

pub fn radicle_signer_enabled(gate: RadicleVotingGate, has_share: bool) -> Result<bool> {
    match gate {
        RadicleVotingGate::Verifier => Ok(false),
        RadicleVotingGate::SignerAllowed => Ok(has_share),
        RadicleVotingGate::Fatal(error) => {
            let reason = match error {
                RadicleVotingGateError::SidecarUnavailable => "sidecar unavailable",
                RadicleVotingGateError::LocalNodeIdUnavailable => "local NodeId unavailable",
                RadicleVotingGateError::ActiveBindingMissing => "active binding missing",
                RadicleVotingGateError::BindingMismatch => "binding mismatch",
            };
            Err(crate::errors::StackEpochError::RadicleVotingGateFatal { reason }.into())
        }
    }
}

pub async fn wait_for_radicle_role_change(
    updates: &mut tokio::sync::watch::Receiver<outbe_radicle::integration::RadicleStatusSnapshot>,
    current_signer: bool,
    has_share: bool,
) -> Result<bool> {
    loop {
        let desired_signer = radicle_signer_enabled(updates.borrow().voting_gate, has_share)?;
        if desired_signer != current_signer {
            return Ok(desired_signer);
        }
        if updates.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

pub fn epoch_validation_inputs(
    committee: EpochValidationCommittee<'_>,
    vrf_materials: &VrfMaterialProvider<MinSig>,
) -> Result<(HybridScheme<MinSig>, Vec<alloy_primitives::Address>)> {
    let EpochValidationCommittee {
        epoch,
        participants,
        validator_set,
        recovered_boundary,
    } = committee;
    let verifier_scheme = HybridScheme::<MinSig>::verifier_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants.clone(),
        vrf_materials.clone(),
    )
    .ok_or(crate::errors::StackEpochError::BuildVerifierSchemeFailed { epoch })?;
    // Simplex participant indices follow ordered::Set pubkey ordering, not the
    // original validator_set order. Certificate signer bitmaps use this order.
    // On restart/live-join with a recovered DKG boundary, provider-latest state
    // may include an unfinalized membership-changing head. Use the boundary's
    // own participant-index-aligned address vector for that recovered epoch.
    let ordered_addresses = match recovered_boundary {
        Some(boundary) => ordered_addresses_from_recovered_boundary(participants, boundary)?,
        None => ordered_validator_addresses(participants, validator_set)?,
    };
    Ok((verifier_scheme, ordered_addresses))
}

pub fn register_epoch_validation_providers(
    committee: EpochValidationCommittee<'_>,
    providers: EpochValidationProviders<'_>,
) -> Result<()> {
    let epoch = committee.epoch;
    let (verifier_scheme, ordered_addresses) =
        epoch_validation_inputs(committee, providers.vrf_materials)?;
    let _ = providers
        .certificate_scheme
        .register(epoch, verifier_scheme);
    let _ = providers.committee.register(epoch, ordered_addresses);
    Ok(())
}

pub struct ValidatorEvmIdentity<'a> {
    pub signing_key: &'a bls12381::PrivateKey,
    pub consensus_validator_set: &'a validators::ValidatorSet,
    pub reshare_target_validator_set: &'a validators::ValidatorSet,
    pub recovered_committee: Option<(
        &'a commonware_utils::ordered::Set<bls12381::PublicKey>,
        &'a DkgBoundaryArtifact,
    )>,
    pub shareless_verifier: bool,
}

pub fn validate_validator_evm_signer(
    args: &ConsensusArgs,
    identity: ValidatorEvmIdentity<'_>,
) -> Result<Option<EthAddress>> {
    let Some(evm_key_path) = args.effective_validator_evm_key()? else {
        return Ok(None);
    };
    let backend = args.evm_key_backend()?;
    let signer =
        crate::bls::load_validator_evm_signer(&evm_key_path, &backend).wrap_err_with(|| {
            format!(
                "failed to load validator EVM key from {}",
                evm_key_path.display()
            )
        })?;
    let signer_address = signer.address();

    match identity.recovered_committee {
        Some((participants, boundary)) => {
            authorize_recovered_signer(signer_address, &identity, participants, boundary)
        }
        None => authorize_current_signer(signer_address, &identity),
    }
}

fn authorize_recovered_signer(
    signer_address: EthAddress,
    identity: &ValidatorEvmIdentity<'_>,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    boundary: &DkgBoundaryArtifact,
) -> Result<Option<EthAddress>> {
    let signing_key = identity.signing_key;
    let reshare_target_validator_set = identity.reshare_target_validator_set;
    let shareless_verifier = identity.shareless_verifier;
    let ordered_addresses = ordered_addresses_from_recovered_boundary(participants, boundary)
        .wrap_err("failed to validate recovered DKG boundary committee for EVM signer")?;
    let local_public_key = signing_key.public_key();
    let Some(participant_index) = participants.position(&local_public_key) else {
        if shareless_verifier {
            let Some(target_index) = reshare_target_validator_set
                .addresses
                .iter()
                .position(|address| *address == signer_address)
            else {
                info!(
                target: "outbe_engine::stack",
                                       %signer_address,
                                       epoch = boundary.epoch,
                                       "shareless verifier is not yet in the canonical reshare target; retaining no proposer identity"
                                   );
                return Ok(None);
            };
            let target_public_key = reshare_target_validator_set
                .public_keys
                .get(target_index)
                .ok_or(
                    crate::errors::StackEpochError::ReshareTargetMissingBlsPublicKey {
                        address: signer_address,
                    },
                )?;
            if target_public_key != &local_public_key {
                return Err(
                    crate::errors::StackEpochError::EvmKeyBelongsToDifferentReshareBlsKey {
                        address: signer_address,
                    }
                    .into(),
                );
            }
            info!(
                %signer_address,
                epoch = boundary.epoch,
                "shareless validator identity matches the canonical reshare target; \
                 the node remains authority-free until DKG grants its threshold share"
            );
            return Ok(Some(signer_address));
        }
        return Err(
            crate::errors::StackEpochError::LocalBlsKeyNotInRecoveredBoundaryCommittee {
                epoch: boundary.epoch,
            }
            .into(),
        );
    };
    let expected_address = ordered_addresses.get(participant_index).ok_or(
        crate::errors::StackEpochError::RecoveredBoundaryAddressMappingMissingParticipant {
            index: participant_index,
        },
    )?;
    if *expected_address != signer_address {
        return Err(
            crate::errors::StackEpochError::EvmKeyMismatchWithRecoveredBoundaryAddress {
                signer_address,
                expected_address: *expected_address,
            }
            .into(),
        );
    }
    info!(
        address = %signer_address,
        epoch = boundary.epoch,
        "validated validator EVM signer against recovered DKG boundary"
    );
    Ok(Some(signer_address))
}

fn authorize_current_signer(
    signer_address: EthAddress,
    identity: &ValidatorEvmIdentity<'_>,
) -> Result<Option<EthAddress>> {
    let signing_key = identity.signing_key;
    let consensus_validator_set = identity.consensus_validator_set;
    let reshare_target_validator_set = identity.reshare_target_validator_set;
    let shareless_verifier = identity.shareless_verifier;
    let authorized = consensus_validator_set
        .addresses
        .iter()
        .position(|address| *address == signer_address)
        .map(|index| {
            (
                &consensus_validator_set.public_keys,
                index,
                "active consensus participant set",
            )
        })
        .or_else(|| {
            reshare_target_validator_set
                .addresses
                .iter()
                .position(|address| *address == signer_address)
                .map(|index| {
                    (
                        &reshare_target_validator_set.public_keys,
                        index,
                        "current reshare target",
                    )
                })
        });
    let Some((public_keys, index, source_set)) = authorized else {
        if shareless_verifier {
            info!(
                %signer_address,
                "verifier-join: EVM signer is not yet in the on-chain validator set; the node \
                 syncs as a verifier and resolves its proposer address once a reshare grants \
                 it a share"
            );
            return Ok(None);
        }
        return Err(crate::errors::StackEpochError::EvmKeyNotInActiveSets {
            address: signer_address,
        }
        .into());
    };

    let local_public_key = signing_key.public_key();
    let Some(registered_public_key) = public_keys.get(index) else {
        return Err(
            crate::errors::StackEpochError::ValidatorSetMissingBlsPublicKey {
                address: signer_address,
            }
            .into(),
        );
    };
    if registered_public_key != &local_public_key {
        return Err(
            crate::errors::StackEpochError::EvmKeyBelongsToDifferentBlsKey {
                address: signer_address,
            }
            .into(),
        );
    }
    let address = signer_address;
    info!(
        address = %address,
        source_set,
        "validated validator EVM signer"
    );
    Ok(Some(address))
}

/// TEE admission narrows the ordinary validator role; it never creates one.
pub fn authority_signer_enabled(
    authority: Option<&outbe_tee::TeeAuthorityWatch>,
    accountable_validator: bool,
    has_share: bool,
    gate: RadicleVotingGate,
) -> Result<bool> {
    let ordinary = radicle_signer_enabled(gate, has_share)?;
    Ok(ordinary
        && accountable_validator
        && authority.is_some_and(|watch| watch.ensure_current().is_authority_open()))
}

impl<E, N: StackNodeAccess> super::runtime::EpochSupervisor<E, N>
where
    E: Clock,
{
    pub(super) fn local_validator_accountable(&self) -> bool {
        local_validator_accountable(
            self.protocol.as_ref(),
            self.node.provider(),
            &self.signing_key.public_key(),
        )
    }
}

pub(super) fn local_validator_accountable(
    protocol: &dyn crate::protocol_state::EngineProtocolState,
    provider: &(impl reth_ethereum::storage::StateProviderFactory + ?Sized),
    local_key: &bls12381::PublicKey,
) -> bool {
    match validators::read_accountable_validators_at_latest(protocol, provider) {
        Ok(members) => members.public_keys.contains(local_key),
        Err(error) => {
            warn!(%error, "validator eligibility read failed; keeping local voting closed");
            false
        }
    }
}

pub(super) async fn wait_for_authority_role_change(
    updates: &mut tokio::sync::watch::Receiver<outbe_radicle::integration::RadicleStatusSnapshot>,
    authority: &mut Option<outbe_tee::TeeAuthorityWatch>,
    current_signer: bool,
    accountable_validator: bool,
    has_share: bool,
) -> Result<bool> {
    let mut authority_closed = false;
    let mut radicle_closed = false;
    loop {
        let desired = authority_signer_enabled(
            authority.as_ref(),
            accountable_validator,
            has_share,
            updates.borrow().voting_gate,
        )?;
        if desired != current_signer {
            return Ok(desired);
        }
        let authority_update = async {
            if authority_closed {
                std::future::pending::<()>().await;
            }
            match authority {
                Some(watch) => watch.changed().await,
                None => std::future::pending().await,
            }
        };
        let radicle_update = async {
            if radicle_closed {
                std::future::pending::<()>().await;
            }
            updates.changed().await
        };
        tokio::select! {
            result = authority_update => { authority_closed = result.is_err(); },
            result = radicle_update => { radicle_closed = result.is_err(); },
        }
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState as State};

    #[tokio::test]
    async fn live_authority_watch_demotes_and_reopens_without_terminating() {
        use futures::FutureExt as _;
        let channel = TeeAuthorityChannel::new(State::ArmedBootstrap {
            enclave_id: B256::repeat_byte(1),
        });
        let mut authority = Some(channel.subscribe());
        let (radicle, mut updates) = tokio::sync::watch::channel(
            outbe_radicle::integration::RadicleStatusSnapshot::default(),
        );
        let mut snapshot = updates.borrow().clone();
        snapshot.voting_gate = RadicleVotingGate::SignerAllowed;
        radicle.send_replace(snapshot);
        channel.publish(State::SuspendedJailed);
        assert!(
            !wait_for_authority_role_change(&mut updates, &mut authority, true, true, true)
                .await
                .unwrap()
        );
        channel.publish(State::Armed {
            enclave_id: B256::repeat_byte(1),
            valid_until: 100,
        });
        assert!(
            wait_for_authority_role_change(&mut updates, &mut authority, false, true, true)
                .await
                .unwrap()
        );
        // Reopening admission alone cannot restore a jailed validator's ordinary role.
        assert!(
            wait_for_authority_role_change(&mut updates, &mut authority, false, false, true)
                .now_or_never()
                .is_none()
        );
        assert!(
            wait_for_authority_role_change(&mut updates, &mut authority, false, true, false)
                .now_or_never()
                .is_none()
        );
        let waiting =
            wait_for_authority_role_change(&mut updates, &mut authority, true, true, true);
        tokio::pin!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(channel);
        assert!(!waiting
            .as_mut()
            .now_or_never()
            .expect("publisher shutdown must wake the live role monitor")
            .unwrap());
    }

    #[derive(Clone, Copy)]
    struct ProviderHead {
        height: u64,
        jailed: bool,
        timestamp: u64,
        valid_until: u64,
    }

    fn fresh_armed_then(next: ProviderHead, expected: State) {
        let head = std::sync::Arc::new(std::sync::Mutex::new(ProviderHead {
            height: 7,
            jailed: false,
            timestamp: 1_800_000_000,
            valid_until: 1_800_001_000,
        }));
        let channel = TeeAuthorityChannel::default();
        let watch = channel.subscribe();
        let published = std::sync::Arc::clone(&head);
        let publisher = channel.clone();
        channel.install_sync_refresh(move |generation| {
            let facts = *published.lock().expect("provider head");
            if facts.height < publisher.observed_finalized_height() {
                return;
            }
            let closed = match (facts.jailed, facts.timestamp >= facts.valid_until) {
                (true, _) => State::SuspendedJailed,
                (false, true) => State::SuspendedLeaseExpired,
                (false, false) => State::Armed {
                    enclave_id: B256::repeat_byte(1),
                    valid_until: facts.valid_until,
                },
            };
            assert!(publisher.publish_if_generation(generation, closed));
            publisher.note_applied_finalized_height(facts.height);
        });
        channel.note_observed_finalized_height(7);
        assert!(authority_signer_enabled(
            Some(&watch),
            true,
            true,
            RadicleVotingGate::SignerAllowed,
        )
        .unwrap());
        assert!(channel.snapshot().has_verified_resident_key());
        *head.lock().expect("provider head") = next;
        channel.note_observed_finalized_height(next.height);
        assert!(!authority_signer_enabled(
            Some(&watch),
            true,
            true,
            RadicleVotingGate::SignerAllowed,
        )
        .unwrap());
        let closed = channel.snapshot();
        assert_eq!(closed, expected);
        assert!(closed.has_verified_resident_key());
        assert!(!closed.is_authority_open());
    }

    #[test]
    fn signer_voting_gate_closes_from_fresh_armed_to_jailed() {
        fresh_armed_then(
            ProviderHead {
                height: 8,
                jailed: true,
                timestamp: 1_800_000_000,
                valid_until: 1_800_001_000,
            },
            State::SuspendedJailed,
        );
    }

    #[test]
    fn signer_voting_gate_closes_from_fresh_armed_to_lease_expiry() {
        fresh_armed_then(
            ProviderHead {
                height: 9,
                jailed: false,
                timestamp: 1_800_000_000,
                valid_until: 1_800_000_000,
            },
            State::SuspendedLeaseExpired,
        );
    }
}
