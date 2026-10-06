use super::*;

/// Local submission and the complete authority for block-1 coordination.
pub struct TeeBootstrapStartup<'a> {
    pub local_submission: TeeBootstrapParticipantSubmissionV2,
    pub authority: TeeBootstrapAuthorityV2,
    pub committee: BTreeSet<Address>,
    pub evm_signer: &'a OutbeEvmSigner,
}

/// Committee size and network namespace for the startup offer-key ceremony.
pub struct TeeDkgStartup {
    pub participant_count: usize,
    pub network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1,
    pub tribute_offer_epoch: u64,
}

/// Bind the generic OST3 coordinator to the dedicated bounded Commonware
/// bootstrap channel. The scope commits to the complete authority namespace so
/// delivery acknowledgements from another chain, policy or ceremony cannot be
/// replayed here.
pub async fn run_tee_bootstrap_v2_at_startup<S, R, C>(
    request: TeeBootstrapStartup<'_>,
    transport: StartupGossipTransport<S, R, C>,
) -> eyre::Result<TeeBootstrapV2>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    let TeeBootstrapStartup {
        local_submission,
        authority,
        committee,
        evm_signer,
    } = request;
    let policy_hash = authority
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("invalid OST3 startup policy: {error}"))?;
    let mut scope = Vec::with_capacity(OST3_SCOPE_DOMAIN.len() + 32 * 4 + 8);
    scope.extend_from_slice(OST3_SCOPE_DOMAIN);
    scope.extend_from_slice(&authority.policy.chain_id);
    scope.extend_from_slice(authority.policy.genesis_hash.as_slice());
    scope.extend_from_slice(policy_hash.as_slice());
    scope.extend_from_slice(authority.committee_snapshot_hash.as_slice());
    scope.extend_from_slice(&authority.committee_snapshot_block.to_be_bytes());
    scope.extend_from_slice(authority.tribute_offer_public_key.as_slice());
    let mut gossip = CommonwareBootstrapGossip {
        sender: transport.sender,
        receiver: transport.receiver,
        clock: transport.clock,
        delivery: new_delivery_tracker(keccak256(scope), transport.allowed_remote_peers),
    };
    coordinate_tee_bootstrap_v2(
        local_submission,
        TeeBootstrapCoordination {
            authority,
            committee: &committee,
            evm_signer,
        },
        &mut gossip,
    )
    .await
}

/// Run the one-time TEE DKG ceremony at startup and return the **shared offer
/// public key** derived from the group threshold signature (Seam F). This function:
///
/// 1. Exchanges enclave identities across the committee.
/// 2. Drives the dealer/player ceremony and the offer-key partial-signature round
///    entirely through the enclave seams.
/// 3. Returns the byte-identical `tribute_offer_public` that every honest node derives.
///
/// The caller connects the enclave and passes the connected `client`.
///
/// The offer *secret* never leaves the enclave. It is stored resident there and
/// used to decrypt offers.
///
/// The request's participant count is the committee size. Its network binding
/// and offer epoch bind the derived offer key. This function runs before
/// [`run_tee_bootstrap_v2_at_startup`], whose OST3 payload registers the
/// returned key on-chain at block 1.
pub async fn run_tee_dkg_at_startup<E, S, R, C>(
    client: &mut E,
    request: TeeDkgStartup,
    transport: StartupGossipTransport<S, R, C>,
) -> eyre::Result<([u8; 32], Vec<u8>)>
where
    E: outbe_tee::tee_dkg::EnclaveChannel,
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    let TeeDkgStartup {
        participant_count: n,
        network_binding,
        tribute_offer_epoch,
    } = request;
    let my_bls = match client
        .request(&EnclaveRequest::GetPublicKeys)
        .map_err(|e| eyre::eyre!("TEE DKG GetPublicKeys failed: {e}"))?
    {
        EnclaveResponse::PublicKeys { tee_bls_pub, .. } => tee_bls_pub,
        other => return Err(eyre::eyre!("unexpected GetPublicKeys response: {other:?}")),
    };

    let chain_id = B256::from(network_binding.chain_id);

    let mut dkg_scope = Vec::with_capacity(32 + 8 + 7);
    dkg_scope.extend_from_slice(chain_id.as_slice());
    dkg_scope.extend_from_slice(&tribute_offer_epoch.to_be_bytes());
    dkg_scope.extend_from_slice(b"tee-dkg");
    let mut gossip = CommonwareDkgGossip::new(transport, keccak256(dkg_scope));
    let PreparedDkg {
        ceremony_id,
        identities,
    } = prepare_dkg_identities(
        client,
        &mut gossip,
        DkgIdentityRequest {
            my_bls: &my_bls,
            n,
            network_binding: &network_binding,
        },
    )
    .await?;
    let coord = CeremonyCoordinator::new(ceremony_id, 0, my_bls, identities);

    let outcome = run_tee_dkg_ceremony(
        &coord,
        client,
        &mut gossip,
        n,
        chain_id,
        tribute_offer_epoch,
    )
    .await
    .map_err(|e| eyre::eyre!("TEE DKG ceremony failed: {e}"))?;

    Ok((
        outcome.tribute_offer_public,
        outcome.tribute_offer_group_public_key,
    ))
}

/// Read the permanent offer public key only after the enclave marks it ready.
/// Before readiness the recipient field is exclusively an onboarding key and
/// must never be compared with canonical OST3 state.
pub fn query_enclave_offer_public<E>(client: &mut E) -> eyre::Result<B256>
where
    E: outbe_tee::tee_dkg::EnclaveChannel,
{
    match client
        .request(&EnclaveRequest::GetPublicKeys)
        .map_err(|e| eyre::eyre!("TEE GetPublicKeys failed: {e}"))?
    {
        EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        } => {
            if !offer_key_ready {
                return Err(eyre::eyre!(
                    "local enclave permanent offer key is not ready; no recovery or fallback exists"
                ));
            }
            let public = B256::from(recipient_x25519_pub);
            if public.is_zero() {
                return Err(eyre::eyre!(
                    "local enclave reports a ready but zero permanent offer key"
                ));
            }
            Ok(public)
        }
        other => Err(eyre::eyre!("unexpected GetPublicKeys response: {other:?}")),
    }
}

struct DkgIdentityRequest<'a> {
    my_bls: &'a Vec<u8>,
    n: usize,
    network_binding: &'a outbe_primitives::tee_attestation_v1::NetworkBindingV1,
}
struct PreparedDkg {
    ceremony_id: B256,
    identities: Vec<outbe_tee::protocol::ParticipantAnnounce>,
}

async fn prepare_dkg_identities<E, S, R, C>(
    client: &mut E,
    gossip: &mut CommonwareDkgGossip<S, R, C>,
    request: DkgIdentityRequest<'_>,
) -> eyre::Result<PreparedDkg>
where
    E: outbe_tee::tee_dkg::EnclaveChannel,
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    // First exchange only the public BLS identities. Then each enclave binds the
    // resulting exact set to the network before any share-recipient key is
    // trusted.
    let preliminary = gossip
        .exchange_identities(IdentityExchange {
            local: LocalIdentityAnnouncement {
                bls: request.my_bls.clone(),
                enc: [0; 32],
                signature: Vec::new(),
            },
            binding: CeremonyBinding {
                ceremony_id: B256::ZERO,
                round: 0,
                participant_set_hash: B256::ZERO,
            },
            participant_count: request.n,
        })
        .await?;
    let participant_bls = preliminary
        .iter()
        .map(|participant| participant.bls_pub.clone())
        .collect::<Vec<_>>();
    let participant_set_hash =
        outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
            .map_err(|error| eyre::eyre!("invalid TEE DKG participant set: {error}"))?;
    let ceremony_id = outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(
        request.network_binding,
        0,
        participant_set_hash,
    )
    .map_err(|error| eyre::eyre!("invalid TEE DKG ceremony binding: {error}"))?;
    let my_announcement = request_signed_announcement(client, ceremony_id, participant_bls)?;
    let identities = gossip
        .exchange_identities(IdentityExchange {
            local: LocalIdentityAnnouncement {
                bls: my_announcement.bls_pub.clone(),
                enc: my_announcement.enc_pub,
                signature: my_announcement.enc_sig,
            },
            binding: CeremonyBinding {
                ceremony_id,
                round: 0,
                participant_set_hash,
            },
            participant_count: request.n,
        })
        .await?;
    Ok(PreparedDkg {
        ceremony_id,
        identities,
    })
}

fn request_signed_announcement<E: outbe_tee::tee_dkg::EnclaveChannel>(
    client: &mut E,
    ceremony_id: B256,
    participant_bls: Vec<Vec<u8>>,
) -> eyre::Result<outbe_tee::protocol::ParticipantAnnounce> {
    Ok(
        match client
            .request(&EnclaveRequest::DkgParticipantAnnounceV1 {
                ceremony_id,
                round: 0,
                participant_bls,
            })
            .map_err(|error| eyre::eyre!("TEE DKG announcement failed: {error}"))?
        {
            EnclaveResponse::DkgParticipantAnnounceV1 { participant } => participant,
            other => {
                return Err(eyre::eyre!(
                    "unexpected DKG announcement response: {other:?}"
                ))
            }
        },
    )
}
