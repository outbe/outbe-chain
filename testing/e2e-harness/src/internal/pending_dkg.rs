//! Read-only evidence for a completed, not-yet-activated DKG restart.
//!
//! This validates local persistence, not chain authority. The caller must bind
//! the returned public artifact to finalized chain evidence and the owned node
//! incarnation. No private material escapes the observation or enters errors.

use std::{fs, io::ErrorKind, num::NonZeroU32, path::Path};

use commonware_codec::{Encode as _, Read as _, ReadExt as _};
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::Output,
    primitives::{group::Share, sharing::ModeVersion, sharing::Sharing, variant::MinSig},
};
use eyre::{ensure, eyre, Result};
use outbe_consensus::{bls, dkg_manager};
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    reshare_artifact::{decode_boundary_artifact, encode_boundary_artifact},
    validators::{ValidatorP2pAddress, ValidatorSet},
};

const PENDING: [&str; 3] = [
    "dkg_pending_share.hex",
    "dkg_pending_polynomial.hex",
    "dkg_pending_output.hex",
];
const ACTIVE: [&str; 3] = ["dkg_share.hex", "dkg_polynomial.hex", "dkg_output.hex"];
const SNAPSHOT: &str = "dkg_pending_boundary.bin";
const RETIRED: [&str; 7] = [
    PENDING[0],
    PENDING[1],
    PENDING[2],
    SNAPSHOT,
    "dkg_pending_boundary.bin.tmp",
    "dkg_dealer_retry.hex",
    "dkg_player_retry.hex",
];

/// Public checkpoint retained across a same-identity restart. In particular,
/// neither the private signing share nor its encoding is retained here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingDkgCheckpoint {
    pub(crate) completed_at_height: u64,
    pub(crate) artifact: DkgBoundaryArtifact,
}

impl PendingDkgCheckpoint {
    /// Observe the exact configured plaintext E2E keys directory. All four
    /// pending files are mandatory; active files never substitute for them.
    pub(crate) fn observe(keys_dir: &Path, expected_consensus_pubkey: &[u8]) -> Result<Self> {
        let bytes = read_installed_file(keys_dir, SNAPSHOT)?;
        let checkpoint = decode_snapshot(&bytes)?;
        let output = validate_triplet(keys_dir, PENDING, expected_consensus_pubkey)?;
        checkpoint.validate_output(&output)?;
        Ok(checkpoint)
    }

    /// Verify promotion of this exact output and retirement of this ceremony's
    /// pending/retry state. Call at the captured activation, before another DKG
    /// cycle starts; a later cycle is not evidence for this checkpoint.
    pub(crate) fn verify_active(
        &self,
        keys_dir: &Path,
        expected_consensus_pubkey: &[u8],
    ) -> Result<()> {
        let output = validate_triplet(keys_dir, ACTIVE, expected_consensus_pubkey)?;
        self.validate_output(&output)?;
        for name in RETIRED {
            match fs::symlink_metadata(keys_dir.join(name)) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                _ => return Err(eyre!("DKG retirement not proven for {name}")),
            }
        }
        Ok(())
    }

    fn validate_output(&self, output: &Output<MinSig, bls12381::PublicKey>) -> Result<()> {
        let artifact = &self.artifact;
        let boundary_output = dkg_manager::decode_boundary_outcome(&artifact.outcome)
            .ok_or_else(|| eyre!("invalid pending DKG boundary outcome"))?;
        ensure!(
            output == &boundary_output,
            "DKG output differs from checkpoint"
        );
        // The public output decoder validates the ODKO envelope, but does not
        // bind its epoch/full-DKG byte to the containing BoundaryOutcome.
        let outcome_epoch = u64::from_be_bytes(
            artifact.outcome[5..13]
                .try_into()
                .map_err(|_| eyre!("invalid DKG outcome epoch"))?,
        );
        ensure!(
            outcome_epoch == artifact.epoch,
            "DKG outcome epoch mismatch"
        );
        ensure!(
            artifact.outcome[13] == u8::from(artifact.is_full_dkg),
            "DKG outcome full-DKG flag mismatch"
        );
        ensure!(!artifact.is_full_dkg, "pending checkpoint is not a reshare");
        ensure!(
            artifact.freeze_height <= self.completed_at_height
                && self.completed_at_height < artifact.planned_activation_height,
            "DKG checkpoint is not completed before planned activation"
        );
        ensure!(
            output.players().len() == artifact.reshare.new_active_set.len(),
            "DKG participant/address count mismatch"
        );
        // Reuse the production builder for ALL public commitments, including
        // target/set hashes, group key bytes/hash, committee hash and exclusions.
        // This establishes internal consistency, not the historical registry's
        // authority for the supplied address-to-key mapping (the caller's job).
        let validators = ValidatorSet {
            public_keys: output.players().iter().cloned().collect(),
            addresses: artifact.reshare.new_active_set.clone(),
            p2p_addresses: vec![ValidatorP2pAddress::Missing; output.players().len()],
        };
        let rebuilt = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: Epoch::new(artifact.epoch),
            validator_set: &validators,
            output,
            is_full_dkg: artifact.is_full_dkg,
            dkg_cycle: artifact.dkg_cycle,
            freeze_height: artifact.freeze_height,
            planned_activation_height: artifact.planned_activation_height,
            vrf_material_version: artifact.vrf_material_version,
            is_validator_set_change: artifact.is_validator_set_change,
            tee_expired_target_exclusions: artifact.tee_expired_target_exclusions.clone(),
        })
        .map_err(|_| eyre!("invalid DKG boundary commitments"))?;
        ensure!(rebuilt == *artifact, "DKG boundary commitments mismatch");
        Ok(())
    }
}

fn read_installed_file(keys_dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = keys_dir.join(name);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| eyre!("missing or unreadable DKG artifact {name}"))?;
    ensure!(
        metadata.is_file(),
        "DKG artifact is not a regular file: {name}"
    );
    fs::read(path).map_err(|_| eyre!("unreadable DKG artifact {name}"))
}

fn decode_snapshot(bytes: &[u8]) -> Result<PendingDkgCheckpoint> {
    const HEADER_LEN: usize = 20;
    ensure!(bytes.len() >= HEADER_LEN, "truncated pending DKG snapshot");
    ensure!(
        &bytes[..8] == b"ODKGPB02",
        "unsupported pending DKG snapshot"
    );
    let completed_at_height = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| eyre!("invalid DKG completion height"))?,
    );
    let len = u32::from_be_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| eyre!("invalid DKG snapshot length"))?,
    );
    let end = HEADER_LEN
        .checked_add(usize::try_from(len)?)
        .ok_or_else(|| eyre!("DKG snapshot length overflow"))?;
    ensure!(bytes.len() == end, "pending DKG snapshot length mismatch");
    let encoded = &bytes[HEADER_LEN..];
    let artifact = decode_boundary_artifact(encoded)
        .map_err(|_| eyre!("invalid pending DKG boundary encoding"))?
        .ok_or_else(|| eyre!("pending DKG snapshot lacks BoundaryOutcome"))?;
    // Production writes a boundary-only envelope, not an arbitrary header with
    // extra records that decode_boundary_artifact would otherwise disregard.
    let canonical = encode_boundary_artifact(&artifact)
        .map_err(|_| eyre!("invalid pending DKG boundary encoding"))?;
    ensure!(
        canonical.as_ref() == encoded,
        "noncanonical pending DKG snapshot"
    );
    Ok(PendingDkgCheckpoint {
        completed_at_height,
        artifact,
    })
}

fn validate_triplet(
    keys_dir: &Path,
    names: [&str; 3],
    expected_consensus_pubkey: &[u8],
) -> Result<Output<MinSig, bls12381::PublicKey>> {
    let mut decoded = Vec::with_capacity(3);
    for name in names {
        let raw = read_installed_file(keys_dir, name)?;
        let text = std::str::from_utf8(&raw)
            .map_err(|_| eyre!("invalid plaintext DKG artifact {name}"))?;
        decoded.push(
            hex::decode(text.trim()).map_err(|_| eyre!("invalid plaintext DKG artifact {name}"))?,
        );
    }
    // Use the same Commonware codecs as the BLS file loaders, but insist on
    // complete consumption. The file loaders alone accept trailing bytes.
    let cfg = (
        NonZeroU32::new(bls::MAX_VALIDATORS).ok_or_else(|| eyre!("invalid validator limit"))?,
        ModeVersion::v0(),
    );
    let mut share_bytes = decoded[0].as_slice();
    let share = Share::read(&mut share_bytes).map_err(|_| eyre!("invalid DKG share"))?;
    let mut polynomial_bytes = decoded[1].as_slice();
    let polynomial = Sharing::<MinSig>::read_cfg(&mut polynomial_bytes, &cfg)
        .map_err(|_| eyre!("invalid DKG polynomial"))?;
    let mut output_bytes = decoded[2].as_slice();
    let output = Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut output_bytes, &cfg)
        .map_err(|_| eyre!("invalid DKG output"))?;
    ensure!(
        share_bytes.is_empty() && polynomial_bytes.is_empty() && output_bytes.is_empty(),
        "trailing bytes in DKG triplet"
    );
    bls::validate_dkg_triplet(&share, &polynomial, &output)
        .map_err(|_| eyre!("inconsistent DKG triplet"))?;
    let owner = output
        .players()
        .iter()
        .position(|key| key.encode().as_ref() == expected_consensus_pubkey)
        .ok_or_else(|| eyre!("expected consensus owner absent from DKG output"))?;
    ensure!(
        usize::from(share.index) == owner,
        "DKG share belongs to another participant"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use commonware_cryptography::Signer as _;
    use commonware_utils::{ordered::Set, TryCollect as _};
    use outbe_primitives::reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    };

    // Same participant-bound bootstrap and public boundary builder used by
    // production fixtures. All private material is newly generated test data.
    struct Fixture {
        dir: tempfile::TempDir,
        dkg: bls::ParticipantDkgBootstrapResult,
        owner: Vec<u8>,
        artifact: DkgBoundaryArtifact,
    }

    impl Fixture {
        fn new(seed: u64) -> Self {
            let mut keys = (1..=4)
                .map(|n| bls12381::PrivateKey::from_seed(seed + n).public_key())
                .collect::<Vec<_>>();
            keys.sort();
            let players: Set<_> = keys.into_iter().try_collect().unwrap();
            let owner = players.iter().next().unwrap().encode().to_vec();
            let dkg = bls::bootstrap_dkg_for_participants(players.clone()).unwrap();
            let validators = ValidatorSet {
                public_keys: players.iter().cloned().collect(),
                addresses: (1..=4).map(Address::repeat_byte).collect(),
                p2p_addresses: vec![ValidatorP2pAddress::Missing; 4],
            };
            let artifact =
                dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                    epoch: Epoch::new(1),
                    validator_set: &validators,
                    output: &dkg.output,
                    is_full_dkg: false,
                    dkg_cycle: 1,
                    freeze_height: 60,
                    planned_activation_height: 120,
                    vrf_material_version: 1,
                    is_validator_set_change: true,
                    tee_expired_target_exclusions: Vec::new(),
                })
                .unwrap();
            let fixture = Self {
                dir: tempfile::tempdir().unwrap(),
                dkg,
                owner,
                artifact,
            };
            fixture.install(PENDING);
            fixture.write_snapshot(&fixture.artifact);
            fixture
        }

        fn install(&self, names: [&str; 3]) {
            let backend = bls::KeyBackend::Plaintext;
            bls::save_signing_share(
                &self.dir.path().join(names[0]),
                &self.dkg.shares[0],
                &backend,
            )
            .unwrap();
            bls::save_public_polynomial(
                &self.dir.path().join(names[1]),
                &self.dkg.polynomial,
                &backend,
            )
            .unwrap();
            bls::save_dkg_output(&self.dir.path().join(names[2]), &self.dkg.output, &backend)
                .unwrap();
        }

        fn write_snapshot(&self, artifact: &DkgBoundaryArtifact) {
            let encoded = encode_boundary_artifact(artifact).unwrap();
            self.write_envelope(&encoded);
        }

        fn write_envelope(&self, encoded: &[u8]) {
            let mut bytes = b"ODKGPB02".to_vec();
            bytes.extend_from_slice(&64_u64.to_be_bytes());
            bytes.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(encoded);
            fs::write(self.dir.path().join(SNAPSHOT), bytes).unwrap();
        }

        fn observe(&self) -> Result<PendingDkgCheckpoint> {
            PendingDkgCheckpoint::observe(self.dir.path(), &self.owner)
        }

        fn retire(&self) {
            for name in RETIRED {
                let path = self.dir.path().join(name);
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
        }
    }

    #[test]
    fn pending_only_bundle_is_a_checkpoint_without_an_active_share() {
        let fixture = Fixture::new(10);
        let checkpoint = fixture.observe().unwrap();
        assert_eq!(checkpoint.completed_at_height, 64);
        assert_eq!(checkpoint.artifact, fixture.artifact);
        assert!(!fixture.dir.path().join(ACTIVE[0]).exists());
        assert!(checkpoint
            .verify_active(fixture.dir.path(), &fixture.owner)
            .is_err());
        // Debug contains only the public record, never the signing-share bytes.
        let private_encoding = hex::encode(fixture.dkg.shares[0].encode());
        assert!(!format!("{checkpoint:?}").contains(&private_encoding));
    }

    #[test]
    fn active_material_and_temporary_snapshot_cannot_replace_pending_bundle() {
        let fixture = Fixture::new(20);
        fixture.install(ACTIVE);
        let snapshot = fs::read(fixture.dir.path().join(SNAPSHOT)).unwrap();
        fixture.retire();
        fs::write(
            fixture.dir.path().join("dkg_pending_boundary.bin.tmp"),
            snapshot,
        )
        .unwrap();
        assert!(fixture.observe().is_err());
        fixture.write_snapshot(&fixture.artifact);
        assert!(fixture.observe().is_err());
    }

    #[test]
    fn every_missing_corrupt_or_nonregular_pending_artifact_fails_closed() {
        let fixture = Fixture::new(30);
        for name in [PENDING[0], PENDING[1], PENDING[2], SNAPSHOT] {
            let path = fixture.dir.path().join(name);
            let saved = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            assert!(fixture.observe().is_err(), "missing {name}");
            fs::create_dir(&path).unwrap();
            assert!(fixture.observe().is_err(), "directory {name}");
            fs::remove_dir(&path).unwrap();
            fs::write(&path, b"secret-sentinel-never-include-in-error").unwrap();
            let error = fixture.observe().unwrap_err();
            assert!(!format!("{error:#}").contains("secret-sentinel"));
            fs::write(&path, &saved).unwrap();
            assert!(fixture.observe().is_ok());
        }
    }

    #[test]
    fn mixed_triplets_and_a_consistent_but_different_output_are_rejected() {
        let fixture = Fixture::new(40);
        let other = Fixture::new(40); // Same owners, independently dealt material.
        for name in PENDING {
            let path = fixture.dir.path().join(name);
            let saved = fs::read(&path).unwrap();
            fs::copy(other.dir.path().join(name), &path).unwrap();
            assert!(fixture.observe().is_err(), "mixed {name}");
            fs::write(&path, saved).unwrap();
        }
        for name in PENDING {
            fs::copy(other.dir.path().join(name), fixture.dir.path().join(name)).unwrap();
        }
        assert!(
            fixture.observe().is_err(),
            "different output with valid triplet"
        );
    }

    #[test]
    fn a_valid_share_must_belong_to_the_expected_consensus_participant() {
        let fixture = Fixture::new(50);
        let wrong_owner = fixture.dkg.output.players().iter().nth(1).unwrap().encode();
        assert!(PendingDkgCheckpoint::observe(fixture.dir.path(), &wrong_owner).is_err());
        assert!(PendingDkgCheckpoint::observe(fixture.dir.path(), &[]).is_err());
        let absent = bls12381::PrivateKey::from_seed(900).public_key().encode();
        assert!(PendingDkgCheckpoint::observe(fixture.dir.path(), &absent).is_err());
        bls::save_signing_share(
            &fixture.dir.path().join(PENDING[0]),
            &fixture.dkg.shares[1],
            &bls::KeyBackend::Plaintext,
        )
        .unwrap();
        assert!(fixture.observe().is_err());
    }

    #[test]
    fn triplet_decoders_reject_trailing_bytes() {
        let fixture = Fixture::new(60);
        for name in PENDING {
            let path = fixture.dir.path().join(name);
            let saved = fs::read_to_string(&path).unwrap();
            fs::write(&path, format!("{}00", saved.trim())).unwrap();
            assert!(fixture.observe().is_err(), "trailing {name}");
            fs::write(&path, saved).unwrap();
        }
    }

    #[test]
    fn snapshot_requires_exact_installed_version_length_and_boundary_only_envelope() {
        let fixture = Fixture::new(70);
        let path = fixture.dir.path().join(SNAPSHOT);
        let saved = fs::read(&path).unwrap();
        let mut variants = vec![
            Vec::new(),
            saved[..19].to_vec(),
            saved[..saved.len() - 1].to_vec(),
        ];
        let mut legacy = saved.clone();
        legacy[..8].copy_from_slice(b"ODKGPB01");
        variants.push(legacy);
        let mut bad_magic = saved.clone();
        bad_magic[0] = b'X';
        variants.push(bad_magic);
        let mut trailing = saved.clone();
        trailing.push(0);
        variants.push(trailing);
        let mut bad_len = saved.clone();
        bad_len[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        variants.push(bad_len);
        for bytes in variants {
            fs::write(&path, bytes).unwrap();
            assert!(fixture.observe().is_err());
        }
        fixture.write_envelope(&[]);
        assert!(fixture.observe().is_err());
        let extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                fixture.artifact.clone(),
            )),
            timestamp_millis_part: 1,
            ..Default::default()
        })
        .unwrap();
        fixture.write_envelope(&extra);
        assert!(fixture.observe().is_err());
        let preannounce = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                epoch: 1,
                outcome: fixture.artifact.outcome.clone(),
            }),
            ..Default::default()
        })
        .unwrap();
        fixture.write_envelope(&preannounce);
        assert!(fixture.observe().is_err());
    }

    #[test]
    fn boundary_epoch_flags_and_every_public_commitment_are_bound() {
        let fixture = Fixture::new(80);
        let changes: &[fn(&mut DkgBoundaryArtifact)] = &[
            |a| a.epoch += 1,
            |a| a.is_full_dkg = true,
            |a| a.target_set_hash = B256::ZERO,
            |a| a.reshare.active_set_hash = B256::ZERO,
            |a| a.committee_set_hash = B256::ZERO,
            |a| a.vrf_group_public_key = B256::ZERO,
            |a| a.vrf_group_public_key_bytes = vec![0; 96].into(),
            |a| a.vrf_material_version += 1,
            |a| a.reshare.new_active_set.swap(0, 1),
            |a| {
                a.reshare.new_active_set.pop();
            },
            |a| {
                a.tee_recipient_pubkeys
                    .push((Address::repeat_byte(1), B256::ZERO))
            },
            |a| {
                let mut bytes = a.outcome.to_vec();
                bytes[12] += 1;
                a.outcome = bytes.into();
            },
            |a| {
                let mut bytes = a.outcome.to_vec();
                bytes[13] = 1;
                a.outcome = bytes.into();
            },
            |a| {
                let mut bytes = a.outcome.to_vec();
                bytes[13] = 2;
                a.outcome = bytes.into();
            },
            |a| {
                let mut bytes = a.outcome.to_vec();
                bytes.push(0);
                a.outcome = bytes.into();
            },
        ];
        for (index, change) in changes.iter().enumerate() {
            let mut artifact = fixture.artifact.clone();
            change(&mut artifact);
            fixture.write_snapshot(&artifact);
            assert!(fixture.observe().is_err(), "boundary mutation {index}");
        }
        // The production encoder itself rejects inconsistent exclusions, so
        // corrupt the encoded commitment to test the observer's decode path.
        let mut encoded = encode_boundary_artifact(&fixture.artifact)
            .unwrap()
            .to_vec();
        let hash_start = encoded.len() - 34; // final hash(32) + empty count(2)
        encoded[hash_start..hash_start + 32].fill(0x11);
        fixture.write_envelope(&encoded);
        assert!(fixture.observe().is_err());
        // An internally consistent full-DKG outcome still is not this reshare seam.
        let mut full = fixture.artifact.clone();
        full.is_full_dkg = true;
        let mut bytes = full.outcome.to_vec();
        bytes[13] = 1;
        full.outcome = bytes.into();
        fixture.write_snapshot(&full);
        assert!(fixture.observe().is_err());
    }

    #[test]
    fn completion_must_be_between_freeze_and_planned_activation() {
        let fixture = Fixture::new(90);
        let path = fixture.dir.path().join(SNAPSHOT);
        let saved = fs::read(&path).unwrap();
        for height in [59_u64, 120, 121] {
            let mut bytes = saved.clone();
            bytes[8..16].copy_from_slice(&height.to_be_bytes());
            fs::write(&path, bytes).unwrap();
            assert!(fixture.observe().is_err());
        }
    }

    #[test]
    fn promotion_requires_matching_active_material_and_complete_retirement() {
        let fixture = Fixture::new(100);
        let checkpoint = fixture.observe().unwrap();
        fixture.install(ACTIVE);
        assert!(checkpoint
            .verify_active(fixture.dir.path(), &fixture.owner)
            .is_err());
        fixture.retire();
        checkpoint
            .verify_active(fixture.dir.path(), &fixture.owner)
            .unwrap();
        let other_owner = fixture.dkg.output.players().iter().nth(1).unwrap().encode();
        assert!(checkpoint
            .verify_active(fixture.dir.path(), &other_owner)
            .is_err());
        for name in RETIRED {
            let path = fixture.dir.path().join(name);
            fs::write(&path, b"retained synthetic artifact").unwrap();
            assert!(
                checkpoint
                    .verify_active(fixture.dir.path(), &fixture.owner)
                    .is_err(),
                "retained {name}"
            );
            fs::remove_file(path).unwrap();
        }
        for name in ACTIVE {
            let path = fixture.dir.path().join(name);
            let saved = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            assert!(checkpoint
                .verify_active(fixture.dir.path(), &fixture.owner)
                .is_err());
            fs::write(&path, b"invalid synthetic active material").unwrap();
            assert!(checkpoint
                .verify_active(fixture.dir.path(), &fixture.owner)
                .is_err());
            fs::write(path, saved).unwrap();
        }
        let other = Fixture::new(100);
        other.install(ACTIVE);
        for name in ACTIVE {
            fs::copy(other.dir.path().join(name), fixture.dir.path().join(name)).unwrap();
        }
        assert!(checkpoint
            .verify_active(fixture.dir.path(), &fixture.owner)
            .is_err());
    }
}
