use super::*;
use crate::stack::follower::local_anchor::LocalFinalizedHistory;
use commonware_consensus::marshal::store::{Blocks, Certificates};
use commonware_consensus::types::Epocher as _;
use outbe_consensus::bls::bootstrap_dkg_for_participants;
use outbe_consensus::dkg_manager::{build_boundary_artifact, BoundaryArtifactInput};
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
};

const RESTORED_EPOCH: u64 = 621;
const ACTIVATION: u64 = 745_205;
const FLOOR: u64 = ACTIVATION + 45;

struct LocalAnchorRecords {
    records: Vec<(ConsensusBlock, outbe_consensus::marshal_types::Finalization)>,
    canonical: BTreeMap<u64, B256>,
}

fn local_anchor_records() -> LocalAnchorRecords {
    let keys: Vec<_> = (1..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: Set<_> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg_for_participants(participants.clone()).unwrap();
    let epoch = Epoch::new(RESTORED_EPOCH);
    let validator_set = outbe_consensus::validators::ValidatorSet {
        public_keys: participants.iter().cloned().collect(),
        addresses: (1..=3).map(Address::repeat_byte).collect(),
        p2p_addresses: vec![outbe_primitives::validators::ValidatorP2pAddress::Missing; 3],
    };
    let boundary = build_boundary_artifact(BoundaryArtifactInput {
        epoch,
        validator_set: &validator_set,
        output: &dkg.output,
        is_full_dkg: false,
        dkg_cycle: RESTORED_EPOCH,
        freeze_height: ACTIVATION - 10,
        planned_activation_height: ACTIVATION,
        vrf_material_version: RESTORED_EPOCH,
        is_validator_set_change: false,
        tee_expired_target_exclusions: vec![],
    })
    .unwrap();
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
        ..Default::default()
    })
    .unwrap();
    let verifier = HybridScheme::verifier_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants.clone(),
        VrfMaterialProvider::new(RESTORED_EPOCH, dkg.polynomial.clone(), None),
    )
    .unwrap();
    let signers: Vec<_> = keys
        .iter()
        .map(|key| {
            let index = participants.index(&key.public_key()).unwrap().get() as usize;
            HybridScheme::signer_with_vrf_provider(
                &config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                VrfMaterialProvider::new(
                    RESTORED_EPOCH,
                    dkg.polynomial.clone(),
                    Some(dkg.shares[index].clone()),
                ),
            )
            .unwrap()
        })
        .collect();
    let records = [(ACTIVATION, extra_data), (FLOOR, Bytes::new())]
        .into_iter()
        .map(|(height, extra_data)| {
            let mut raw = Block::default();
            raw.header.number = height;
            raw.header.extra_data = extra_data;
            let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(OutbeHeader::new),
            ));
            let proposal = Proposal::new(
                Round::new(epoch, View::new(height - ACTIVATION + 17)),
                View::new(16),
                block.digest(),
            );
            let certificate = fixtures::certify_fixture_proposal(&verifier, &signers, proposal);
            (block, certificate)
        })
        .collect::<Vec<_>>();
    let canonical = records
        .iter()
        .map(|(block, _)| (block.number(), block.block_hash()))
        .collect();
    LocalAnchorRecords { records, canonical }
}

type LocalCertificates = immutable::Archive<
    commonware_tokio::Context,
    Digest,
    outbe_consensus::marshal_types::Finalization,
>;
type LocalBlocks = immutable::Archive<commonware_tokio::Context, Digest, ConsensusBlock>;

async fn write_local_anchor_archives(
    ctx: commonware_tokio::Context,
    fixture: &LocalAnchorRecords,
) -> (LocalCertificates, LocalBlocks) {
    let page_cache = CacheRef::from_pooler(
        &ctx,
        nonzero_u16(4096, "page").unwrap(),
        nonzero_usize(64, "capacity").unwrap(),
    );
    let mut certificates = immutable::Archive::init(
        ctx.child("certificates"),
        marshal_archive::archive_config(
            "local-anchor",
            marshal_archive::ArchiveKind::Finalizations,
            &page_cache,
            HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let mut blocks = immutable::Archive::init(
        ctx.child("blocks"),
        marshal_archive::archive_config(
            "local-anchor",
            marshal_archive::ArchiveKind::Blocks,
            &page_cache,
            (),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    for (block, certificate) in &fixture.records {
        certificates = Certificates::put(
            certificates,
            Height::new(block.number()),
            block.digest(),
            certificate.clone(),
        )
        .await
        .unwrap();
        blocks = Blocks::put(blocks, block.clone()).await.unwrap();
    }
    certificates = Certificates::sync(certificates).await.unwrap();
    blocks = Blocks::sync(blocks).await.unwrap();
    (certificates, blocks)
}

fn assert_restored_committee(
    restored: &crate::stack::follower::local_anchor::RestoredCommittee,
    fixture: &LocalAnchorRecords,
) {
    assert_eq!(restored.chain.anchor_epoch(), RESTORED_EPOCH);
    assert_eq!(
        restored
            .epocher
            .activation_height(Epoch::new(RESTORED_EPOCH)),
        Some(Height::new(ACTIVATION))
    );
    restored
        .chain
        .verify_finalization(Epoch::new(RESTORED_EPOCH), &fixture.records[1].1)
        .unwrap();
    assert!(restored
        .epocher
        .containing(Height::new(ACTIVATION - 1))
        .is_none());
}

#[test]
fn local_snapshot_anchor_restores_archived_committee_and_rejects_conflicting_canonical_hashes() {
    commonware_tokio::Runner::new(commonware_tokio::Config::default().with_worker_threads(2))
        .start(|ctx| async move {
            let fixture = local_anchor_records();
            let (certificates, blocks) = write_local_anchor_archives(ctx, &fixture).await;
            for mismatch in [None, Some(FLOOR), Some(ACTIVATION)] {
                let result = LocalFinalizedHistory {
                    certificates: &certificates,
                    blocks: &blocks,
                    canonical_hash: |height| {
                        Ok(if mismatch == Some(height) {
                            Some(B256::ZERO)
                        } else {
                            fixture.canonical.get(&height).copied()
                        })
                    },
                    floor: FLOOR,
                    epoch_length: 1_200,
                    activation_grace: 300,
                }
                .restore()
                .await;
                if mismatch.is_some() {
                    assert!(result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("disagrees with canonical execution state"));
                } else {
                    let restored = result.unwrap().unwrap();
                    assert_restored_committee(&restored, &fixture);
                }
            }
        });
}

#[test]
fn local_snapshot_anchor_rejects_modified_signed_certificates() {
    for record_index in [0, 1] {
        commonware_tokio::Runner::new(commonware_tokio::Config::default().with_worker_threads(2))
            .start(move |ctx| async move {
                let mut fixture = local_anchor_records();
                // Keep the payload canonical but invalidate the signed proposal.
                fixture.records[record_index].1.proposal.parent = View::new(15);
                let (certificates, blocks) = write_local_anchor_archives(ctx, &fixture).await;
                assert!(LocalFinalizedHistory {
                    certificates: &certificates,
                    blocks: &blocks,
                    canonical_hash: |height| Ok(fixture.canonical.get(&height).copied()),
                    floor: FLOOR,
                    epoch_length: 1_200,
                    activation_grace: 300,
                }
                .restore()
                .await
                .is_err());
            });
    }
}

#[test]
fn local_snapshot_anchor_missing_floor_or_boundary_uses_genesis_fallback() {
    commonware_tokio::Runner::new(commonware_tokio::Config::default().with_worker_threads(2))
        .start(|ctx| async move {
            let fixture = local_anchor_records();
            let (certificates, blocks) = write_local_anchor_archives(ctx, &fixture).await;
            // The final case places the boundary outside the discovery window.
            for (floor, epoch_length, activation_grace) in
                [(0, 1_200, 300), (FLOOR + 1, 1_200, 300), (FLOOR, 10, 0)]
            {
                assert!(LocalFinalizedHistory {
                    certificates: &certificates,
                    blocks: &blocks,
                    canonical_hash: |height| Ok(fixture.canonical.get(&height).copied()),
                    floor,
                    epoch_length,
                    activation_grace,
                }
                .restore()
                .await
                .unwrap()
                .is_none());
            }
        });
}
