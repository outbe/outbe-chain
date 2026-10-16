//! Bind current public DKG material without rewriting accepted native history.
use super::{review::Error, CheckpointOwnerSnapshot, OwnerRef, PublicationManifest};
use crate::{
    integration::signing::{
        accepted::AcceptedPublicationVerifier, witness::RepositoryWitnessBundle,
        RepositorySigningMaterial,
    },
    manager::{FinalizedMaterialSigningState, FinalizedPublicationReceipt},
};
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};

pub struct VerifiedMaterialPublication {
    body: PublicationBody,
    manifest: PublicationManifest,
    witness_hash: alloy_primitives::B256,
}
impl VerifiedMaterialPublication {
    pub fn body(&self) -> &PublicationBody {
        &self.body
    }
    pub fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }
    pub(crate) fn witness_hash(&self) -> alloy_primitives::B256 {
        self.witness_hash
    }
    pub fn authorize_current(
        &self,
        state: &FinalizedMaterialSigningState,
        material: &RepositorySigningMaterial,
    ) -> Result<(), Error> {
        let mut expected = body(state, material)?;
        expected.package_manifest_hash = self.manifest.hash()?;
        if expected != self.body {
            return Err(Error::Context);
        }
        Ok(())
    }
}

fn body(
    state: &FinalizedMaterialSigningState,
    material: &RepositorySigningMaterial,
) -> Result<PublicationBody, Error> {
    let before = state.checkpoint().receipt();
    let bound = &state.material().receipt().body;
    let authority = state.authority();
    if material.authority() != authority
        || before.body.chain_id != authority.chain_id()
        || before.body.genesis_hash != authority.genesis_hash()
        || before.body.owner_bls_public.as_slice()
            != authority.committee().vrf_group_public_key_bytes.as_slice()
        || (bound.epoch == authority.epoch()
            && bound.material_version == authority.committee().vrf_material_version)
        || state.anchor().number > authority.block().number
    {
        return Err(Error::Context);
    }
    let mut body = before.body.clone();
    body.sequence = body.sequence.checked_add(1).ok_or(Error::Context)?;
    body.previous_receipt_hash = before.receipt_hash;
    body.action = PublicationAction::BindMaterial;
    body.epoch = authority.epoch();
    body.material_version = authority.committee().vrf_material_version;
    body.material_hash = material.material_hash().into();
    body.players_commitment = material.players_commitment().into();
    body.proposal_id = Default::default();
    body.patch_id = Default::default();
    body.revision_id = Default::default();
    body.expected_target_head = Default::default();
    body.finalized_anchor_height = state.anchor().number;
    body.finalized_anchor_hash = state.anchor().hash;
    body.package_manifest_hash = Default::default();
    Ok(body)
}

pub fn prepare_material_publication(
    state: &FinalizedMaterialSigningState,
    snapshot: &CheckpointOwnerSnapshot,
    material: &RepositorySigningMaterial,
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedMaterialPublication, Error> {
    if snapshot.checkpoint().receipt() != state.checkpoint().receipt() {
        return Err(Error::Context);
    }
    witness.verify(material, &[])?;
    let manifest = PublicationManifest::new(
        vec![],
        vec![],
        vec![],
        snapshot
            .owner_refs()
            .iter()
            .map(|(name, oid)| OwnerRef {
                name: name.to_string(),
                oid: oid.to_string(),
            })
            .collect(),
    )?;
    let mut body = body(state, material)?;
    body.package_manifest_hash = manifest.hash()?;
    manifest.bind_receipt(&body)?;
    Ok(VerifiedMaterialPublication {
        body,
        manifest,
        witness_hash: Sha256::hash(&[&witness.to_bytes()?]).0.into(),
    })
}
pub fn verify_material_publication(
    state: &FinalizedMaterialSigningState,
    snapshot: &CheckpointOwnerSnapshot,
    material: &RepositorySigningMaterial,
    body: &PublicationBody,
    manifest: &PublicationManifest,
    witness: &RepositoryWitnessBundle,
) -> Result<VerifiedMaterialPublication, Error> {
    let expected = prepare_material_publication(state, snapshot, material, witness)?;
    if expected.body != *body || expected.manifest != *manifest {
        return Err(Error::Package);
    }
    Ok(expected)
}

pub struct AcceptedMaterialPublication {
    receipt: FinalizedPublicationReceipt,
}
impl AcceptedMaterialPublication {
    pub fn receipt(&self) -> &FinalizedPublicationReceipt {
        &self.receipt
    }
}
pub fn verify_accepted_material_publication(
    previous: &CheckpointOwnerSnapshot,
    verifier: &AcceptedPublicationVerifier,
) -> Result<AcceptedMaterialPublication, Error> {
    let receipt = verifier.receipt();
    let body = &receipt.receipt().body;
    let old = previous.checkpoint().receipt();
    if body.action != PublicationAction::BindMaterial
        || body.sequence != old.body.sequence.checked_add(1).ok_or(Error::Context)?
        || body.previous_receipt_hash != old.receipt_hash
        || body.chain_id != old.body.chain_id
        || body.genesis_hash != old.body.genesis_hash
        || body.repo_id != old.body.repo_id
        || body.owner_bls_public != old.body.owner_bls_public
        || body.descriptor_hash != old.body.descriptor_hash
        || (body.epoch == old.body.epoch && body.material_version == old.body.material_version)
        || verifier.manifest().owner_refs()? != *previous.owner_refs()
        || !body.proposal_id.is_zero()
        || !body.patch_id.is_zero()
        || !body.revision_id.is_zero()
        || !body.expected_target_head.is_zero()
    {
        return Err(Error::Context);
    }
    verifier.verify_native(&[])?;
    Ok(AcceptedMaterialPublication {
        receipt: receipt.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::FinalizedBlock;
    use alloy_primitives::B256;
    use radicle::{crypto::BlsPublicKey, git, storage::refs::Refs};

    #[test]
    fn binding_changes_only_public_material_and_rejects_changed_owner_snapshot() {
        // Public-material fixture, not evidence of an interactive reshare or native installation.
        let (patch, output, _, _) = crate::integration::local::tests::signing_fixture(4);
        let mut authority = patch.authority.clone();
        authority.committee.vrf_material_version += 1;
        let owner =
            BlsPublicKey::from_bytes(patch.checkpoint.receipt.body.owner_bls_public.0).unwrap();
        let material = RepositorySigningMaterial::new(authority.clone(), output, owner).unwrap();
        let state = FinalizedMaterialSigningState {
            authority,
            checkpoint: patch.checkpoint.clone(),
            material: patch.material,
            anchor: FinalizedBlock {
                number: 11,
                hash: B256::repeat_byte(11),
            },
        };
        let mut refs = Refs::new();
        refs.insert(
            radicle::storage::refs::IDENTITY_ROOT.to_ref_string(),
            git::Oid::from_sha1([2; 20]),
        );
        refs.insert(
            git::fmt::qualified!("refs/heads/main").to_ref_string(),
            git::Oid::from_sha1([3; 20]),
        );
        let snapshot = CheckpointOwnerSnapshot::from_local_refs(
            state.checkpoint.clone(),
            refs.clone(),
            git::Oid::from_sha1([4; 20]),
        )
        .unwrap();
        let witness = RepositoryWitnessBundle::new(material.canonical_output(), vec![]).unwrap();
        let package = prepare_material_publication(&state, &snapshot, &material, &witness).unwrap();
        assert_eq!(package.body.action, PublicationAction::BindMaterial);
        assert_ne!(
            package.body.material_hash,
            state.material.receipt().body.material_hash
        );
        assert!(package.manifest.objects().is_empty());
        assert!(package.manifest.refs().is_empty());
        assert!(package.manifest.subjects().is_empty());
        assert_eq!(package.manifest.owner_refs().unwrap(), refs);
        package.authorize_current(&state, &material).unwrap();
        let receipt = FinalizedPublicationReceipt {
            block: state.authority.block,
            receipt: outbe_radicleregistry::publication::PublicationReceipt {
                accepted_height: state.authority.block.number,
                receipt_hash: package.body.receipt_hash(),
                body: package.body.clone(),
            },
        };
        let verifier =
            AcceptedPublicationVerifier::from_receipt(receipt.clone(), package.manifest.clone())
                .unwrap();
        let proof = verify_accepted_material_publication(&snapshot, &verifier).unwrap();
        assert_eq!(proof.receipt().receipt(), receipt.receipt());
        let mut after = snapshot.clone();
        after.advance(receipt.clone(), &package.manifest).unwrap();
        assert_eq!(after.owner_refs(), snapshot.owner_refs());
        assert_eq!(after.sigrefs(), snapshot.sigrefs());
        let already_bound = FinalizedMaterialSigningState {
            checkpoint: receipt.clone(),
            material: receipt.clone(),
            ..state
        };
        assert!(package
            .authorize_current(&already_bound, &material)
            .is_err());
        let mut changed = package.manifest.owner_snapshot().to_vec();
        changed
            .iter_mut()
            .find(|r| r.name == "refs/heads/main")
            .unwrap()
            .oid = git::Oid::from_sha1([9; 20]).to_string();
        let changed = PublicationManifest::new(vec![], vec![], vec![], changed).unwrap();
        let mut wrong = receipt;
        wrong.receipt.body.package_manifest_hash = changed.hash().unwrap();
        wrong.receipt.receipt_hash = wrong.receipt.body.receipt_hash();
        let wrong = AcceptedPublicationVerifier::from_receipt(wrong, changed).unwrap();
        assert!(verify_accepted_material_publication(&snapshot, &wrong).is_err());
    }
}
