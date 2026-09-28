use super::super::*;

pub(in crate::lifecycle) struct Dkg {
    pub(in crate::lifecycle) keys: Vec<PrivateKey>,
    pub(in crate::lifecycle) public_keys: Vec<PublicKey>,
    pub(in crate::lifecycle) vrf_group_public_key: <MinSig as Variant>::Public,
    pub(in crate::lifecycle) vrf_threshold_private:
        commonware_cryptography::bls12381::primitives::group::Private,
}

pub(in crate::lifecycle) fn build_dkg() -> Dkg {
    let keys = (0..4)
        .map(|index| PrivateKey::from_seed(index + 1))
        .collect::<Vec<_>>();
    let public_keys = keys
        .iter()
        .cloned()
        .map(PublicKey::from)
        .collect::<Vec<_>>();
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    let (vrf_threshold_private, vrf_group_public_key) = keypair::<_, MinSig>(&mut rng);
    Dkg {
        keys,
        public_keys,
        vrf_group_public_key,
        vrf_threshold_private,
    }
}

pub(in crate::lifecycle) fn build_snapshot(dkg: &Dkg) -> CommitteeSnapshot {
    let mut public_keys = dkg.public_keys.iter().collect::<Vec<_>>();
    public_keys.sort_by_key(|public_key| public_key.encode().to_vec());
    CommitteeSnapshot {
        committee: public_keys
            .into_iter()
            .enumerate()
            .map(|(index, public_key)| {
                let mut consensus_pubkey = [0u8; 48];
                consensus_pubkey.copy_from_slice(public_key.encode().as_ref());
                CommitteeEntry {
                    address: validator_sender(
                        u8::try_from(index).expect("fixture validator index fits u8"),
                    ),
                    consensus_pubkey,
                }
            })
            .collect(),
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_bytes: dkg.vrf_group_public_key.encode().to_vec(),
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

pub(in crate::lifecycle) fn finalized_parent_metadata(
    dkg: &Dkg,
    snapshot: &CommitteeSnapshot,
    finalized_block_number: u64,
    parent_hash: B256,
) -> CertifiedParentAccountingMetadata {
    let round = Round::new(Epoch::new(FINALIZED_EPOCH), View::new(FINALIZED_VIEW));
    let proposal =
        Proposal::<Sha256Digest>::new(round, View::new(PARENT_VIEW), Sha256Digest(parent_hash.0));
    let vote_message = proposal.encode().to_vec();
    let seed_message = round.encode().to_vec();
    let committee_set = commonware_utils::ordered::Set::from_iter_dedup(
        dkg.keys.iter().map(|key| key.public_key()),
    );
    let namespace = finalize_namespace(&committee_set);
    let signatures = dkg
        .keys
        .iter()
        .map(|key| key.sign(&namespace, &vote_message))
        .collect::<Vec<_>>();
    let certificate = HybridCertificate::<MinSig> {
        signers: Signers::new(
            dkg.keys.len() as u32,
            (0..u32::try_from(dkg.keys.len()).unwrap()).map(Participant::new),
        )
        .unwrap(),
        bls_aggregated_vote: aggregate::combine_signatures::<MinPk, _>(
            commonware_utils::iter::NonEmpty::try_new(
                signatures.iter().map(|signature| signature.as_ref()),
            )
            .unwrap(),
        ),
        vrf_proof: VrfProof::<MinSig> {
            material_version: VRF_MATERIAL_VERSION,
            threshold_signature: sign_message::<MinSig>(
                &dkg.vrf_threshold_private,
                &hybrid_seed_namespace(),
                &seed_message,
            ),
        },
    };
    let proof = Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal,
        certificate,
    }
    .encode()
    .to_vec();
    let committee_set_hash =
        outbe_consensus::proof::committee_set_hash_v2(FINALIZED_EPOCH, snapshot);
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number,
        finalized_block_hash: parent_hash,
        finalized_epoch: FINALIZED_EPOCH,
        finalized_view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        ordered_committee: snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .collect(),
        signer_bitmap: vec![1; snapshot.committee.len()],
        proof: Bytes::from(proof),
        committee_set_hash,
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_hash: keccak256(&snapshot.vrf_group_public_key_bytes),
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: Vec::new(),
    };
    outbe_consensus::proof::verify_v2_proof(
        &metadata,
        snapshot,
        metadata.proof.as_ref(),
        parent_hash,
    )
    .expect("real finalized-parent proof fixture verifies before execution");
    metadata
}
