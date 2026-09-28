use super::*;

/// Run a minimal 3-node DKG to get a valid (Output, Share) for testing.
#[allow(clippy::type_complexity)]
pub(super) fn run_test_dkg_complete() -> (
    Vec<bls12381::PrivateKey>,
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Share,
    Sharing<MinSig>,
) {
    use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Info, Player};
    use commonware_cryptography::bls12381::primitives::sharing::Mode;
    use commonware_parallel::Sequential;
    use commonware_utils::N3f1;

    let mut keys: Vec<bls12381::PrivateKey> = (0..3)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    keys.sort_by(|a, b| {
        commonware_codec::Encode::encode(&a.public_key())
            .cmp(&commonware_codec::Encode::encode(&b.public_key()))
    });

    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();

    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        b"test",
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    // Each validator deals and acks.
    let mut dealers = Vec::new();
    let mut pub_msgs = Vec::new();
    let mut all_priv_msgs = Vec::new();

    for key in &keys {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers.push(dealer);
        pub_msgs.push(pub_msg);
        all_priv_msgs.push(priv_msgs);
    }

    // Each player receives from all dealers.
    let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|k| Player::new(info.clone(), k.clone()).unwrap())
        .collect();

    for (dealer_idx, (pub_msg, priv_msgs)) in pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
    {
        let dealer_pk = keys[dealer_idx].public_key();
        for (player_pk, priv_msg) in priv_msgs {
            let player_idx = keys
                .iter()
                .position(|k| &k.public_key() == player_pk)
                .unwrap();
            if let Some(ack) = players[player_idx]
                .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg.clone())
                .expect("fixture dealing must be valid")
            {
                dealers[dealer_idx]
                    .receive_player_ack(player_pk.clone(), ack)
                    .unwrap();
            }
        }
    }

    // Finalize all dealers.
    let mut logs = std::collections::BTreeMap::new();
    for dealer in dealers {
        let signed_log = dealer.finalize::<N3f1>();
        if let Some((pk, log)) = signed_log.check(&info) {
            logs.insert(pk, log);
        }
    }

    // Player 0 finalizes.
    let mut dkg_logs = commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs::<
        MinSig,
        bls12381::PublicKey,
        N3f1,
    >::new(info.clone());
    for (dealer_pk, log) in logs {
        dkg_logs.record(dealer_pk, log);
    }
    let (output, share) = players
        .remove(0)
        .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            dkg_logs,
            &Sequential,
        )
        .unwrap();
    let polynomial = output.public().clone();

    (keys, participants, output, share, polynomial)
}

pub(super) fn run_test_dkg() -> (
    Vec<bls12381::PrivateKey>,
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Sharing<MinSig>,
) {
    let (keys, participants, _output, _share, polynomial) = run_test_dkg_complete();
    (keys, participants, _output, polynomial)
}

pub(super) fn test_boundary_with_vrf_hash(
    vrf_group_public_key: B256,
    dkg_cycle: u64,
) -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: dkg_cycle,
        dkg_cycle,
        freeze_height: 10,
        planned_activation_height: 20,
        target_set_hash: B256::with_last_byte(0xA1),
        vrf_material_version: dkg_cycle,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::new(),
        committee_set_hash: B256::ZERO,
        is_validator_set_change: true,
        outcome: Bytes::new(),
        is_full_dkg: false,
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: outbe_primitives::consensus::ReshareResult {
            new_active_set: Vec::new(),
            active_set_hash: B256::with_last_byte(0xA2),
        },
    }
}

pub(super) fn recovery_block(number: u64) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.extra_data = Bytes::from(vec![number as u8]);
    let block = block.map_header(OutbeHeader::new);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

pub(super) fn recovery_finalization_fixture(
    block: &ConsensusBlock,
    round: Round,
) -> (
    HybridSchemeProvider<MinSig>,
    Finalization<HybridScheme<MinSig>, outbe_consensus::digest::Digest>,
) {
    let keys: Vec<bls12381::PrivateKey> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg(3).unwrap();
    let signers: Vec<HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let pk = key.public_key();
            let idx = participants.index(&pk).unwrap();
            HybridScheme::signer(
                &config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[idx.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();
    let verifier = HybridScheme::<MinSig>::verifier(
        &config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .unwrap();

    let proposal = Proposal::new(
        round,
        round.view().previous().unwrap_or(View::zero()),
        block.digest(),
    );
    let finalizes: Vec<_> = signers
        .iter()
        .map(|scheme| Finalize::sign(scheme, proposal.clone()).unwrap())
        .collect();
    let finalization = Finalization::from_finalizes(
        &verifier,
        commonware_utils::iter::NonEmpty::try_new(finalizes.iter()).unwrap(),
        &Sequential,
    )
    .unwrap();
    let provider = HybridSchemeProvider::new();
    let _ = provider.register(round.epoch(), verifier);
    (provider, finalization)
}

/// Finalized-header provider stub: serves sealed headers carrying chosen
/// consensus header artifacts, optionally blocking one height on a barrier.
#[derive(Clone, Default)]
pub(super) struct MockFinalizedHeaderProvider {
    headers: BTreeMap<u64, SealedHeader<OutbeHeader>>,
    sealed_header_barrier: Option<(u64, Arc<Barrier>, Arc<Barrier>)>,
}

impl MockFinalizedHeaderProvider {
    pub(super) fn insert(&mut self, number: u64, artifact: Option<ConsensusHeaderArtifact>) {
        let extra_data = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                consensus_header_artifact: artifact,
                ..Default::default()
            },
        )
        .unwrap();
        self.headers.insert(
            number,
            SealedHeader::seal_slow(OutbeHeader::new(Header {
                number,
                extra_data,
                ..Default::default()
            })),
        );
    }

    pub(super) fn block_sealed_header_at(
        &mut self,
        number: u64,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    ) {
        self.sealed_header_barrier = Some((number, entered, release));
    }

    pub(super) fn without_sealed_header_barrier(&self) -> Self {
        Self {
            headers: self.headers.clone(),
            sealed_header_barrier: None,
        }
    }
}

impl BlockHashReader for MockFinalizedHeaderProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.headers.get(&number).map(SealedHeader::hash))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|height| self.headers.get(&height).map(SealedHeader::hash))
            .collect())
    }
}

impl HeaderProvider for MockFinalizedHeaderProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok(self
            .headers
            .values()
            .find(|header| header.hash() == block_hash)
            .map(|header| header.header().clone()))
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<Self::Header>> {
        Ok(self.headers.get(&num).map(|header| header.header().clone()))
    }

    fn headers_range(
        &self,
        _range: impl std::ops::RangeBounds<u64>,
    ) -> ProviderResult<Vec<Self::Header>> {
        Ok(Vec::new())
    }

    fn sealed_header(&self, number: u64) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        if let Some((blocked_number, entered, release)) = &self.sealed_header_barrier {
            if number == *blocked_number {
                entered.wait();
                release.wait();
            }
        }
        Ok(self.headers.get(&number).cloned())
    }

    fn sealed_headers_while(
        &self,
        _range: impl std::ops::RangeBounds<u64>,
        _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        Ok(Vec::new())
    }
}
