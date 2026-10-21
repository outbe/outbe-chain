use outbe_primitives::{
    error::Result,
    radicle_publication::{players_commitment, PublicationAction, PublicationBody},
};
use outbe_validatorset::{contract::ValidatorSet, state::CommitteeSnapshot};

use crate::{
    errors::CodeRegistryError::InvalidPublication,
    payload::PatchVotePayloadV1,
    publication::PublicationReceipt,
    repo_id_storage_key,
    schema::{PATCH_EXECUTION_PENDING, PATCH_EXECUTION_REVIEWED},
    CodeRegistry,
};

impl CodeRegistry<'_> {
    pub(super) fn publication_committee(
        &self,
        body: &PublicationBody,
    ) -> Result<CommitteeSnapshot> {
        let validators = ValidatorSet::new(self.storage.clone());
        let epoch = validators.current_epoch_u64()?;
        if epoch != body.epoch {
            return Err(InvalidPublication("current epoch").into());
        }
        let committee = outbe_validatorset::state::read_committee_snapshot_for_epoch(
            self.storage.clone(),
            epoch,
        )?
        .ok_or(InvalidPublication("current committee missing"))?;
        let effective = validators.get_active_consensus_set()?;
        validate_committee_members(&committee, &effective)?;
        validate_committee_material(&committee, body)?;
        Ok(committee)
    }

    pub(super) fn validate_publication_body(&self, body: &PublicationBody) -> Result<()> {
        let sequence = self.publication_sequence.read()?;
        self.validate_body_chain_and_sequence(body, sequence)?;
        let previous = self.load_previous_receipt(body, sequence)?;
        let expected_anchor = self.expected_anchor_height(body, sequence, previous.as_ref())?;
        self.validate_canonical_anchor(body, expected_anchor)
    }

    fn validate_body_chain_and_sequence(
        &self,
        body: &PublicationBody,
        sequence: u64,
    ) -> Result<()> {
        let descriptor = self
            .governments_descriptor()?
            .ok_or(InvalidPublication("governments disabled"))?;
        let height = self.storage.block_number()?;
        if body.chain_id != self.storage.chain_id()?
            || body.genesis_hash != self.storage.genesis_hash()?
            || body.genesis_hash.is_zero()
        {
            return Err(InvalidPublication("chain/descriptor/sequence").into());
        }
        if body.descriptor_hash != descriptor.hash()?
            || body.repo_id.is_zero()
            || body.material_hash.is_zero()
        {
            return Err(InvalidPublication("chain/descriptor/sequence").into());
        }
        if body.package_manifest_hash.is_zero()
            || sequence.checked_add(1) != Some(body.sequence)
            || self.publication_receipt_hash.read()? != body.previous_receipt_hash
        {
            return Err(InvalidPublication("chain/descriptor/sequence").into());
        }
        if body.finalized_anchor_height >= height {
            return Err(InvalidPublication("chain/descriptor/sequence").into());
        }
        Ok(())
    }

    fn load_previous_receipt(
        &self,
        body: &PublicationBody,
        sequence: u64,
    ) -> Result<Option<PublicationReceipt>> {
        if sequence == 0 {
            return Ok(None);
        }
        let receipt = self
            .publication_receipt(sequence)?
            .ok_or(InvalidPublication("previous receipt missing"))?;
        if receipt.receipt_hash != body.previous_receipt_hash {
            return Err(InvalidPublication("previous receipt mismatch").into());
        }
        Ok(Some(receipt))
    }

    fn expected_anchor_height(
        &self,
        body: &PublicationBody,
        sequence: u64,
        previous: Option<&PublicationReceipt>,
    ) -> Result<u64> {
        if body.action == PublicationAction::Bootstrap {
            if sequence != 0
                || !body.previous_receipt_hash.is_zero()
                || !self.network_repository.read()?.is_zero()
            {
                return Err(InvalidPublication("already bootstrapped").into());
            }
            self.validate_non_patch_body(body)?;
            return Ok(0);
        }
        let bootstrap = self
            .publication_receipt(1)?
            .ok_or(InvalidPublication("bootstrap receipt missing"))?;
        if self.network_repository.read()? != repo_id_storage_key(body.repo_id)
            || bootstrap.body.owner_bls_public != body.owner_bls_public
        {
            return Err(InvalidPublication("immutable network owner/RID").into());
        }
        let material = self
            .publication_receipt(self.publication_material_sequence.read()?)?
            .ok_or(InvalidPublication("material receipt missing"))?;
        if body.action == PublicationAction::BindMaterial {
            return self.validate_bind_material_anchor(body, &material);
        }
        self.validate_patch_action_anchor(body, &material, previous)
    }

    fn validate_bind_material_anchor(
        &self,
        body: &PublicationBody,
        material: &PublicationReceipt,
    ) -> Result<u64> {
        self.validate_non_patch_body(body)?;
        if material.body.epoch == body.epoch
            && material.body.material_version == body.material_version
        {
            return Err(InvalidPublication("material already bound").into());
        }
        Ok(ValidatorSet::new(self.storage.clone())
            .epoch_snapshot()?
            .start_block)
    }

    fn validate_patch_action_anchor(
        &self,
        body: &PublicationBody,
        material: &PublicationReceipt,
        previous: Option<&PublicationReceipt>,
    ) -> Result<u64> {
        if material.body.epoch != body.epoch
            || material.body.material_version != body.material_version
        {
            return Err(InvalidPublication("current material is not bound").into());
        }
        if material.body.material_hash != body.material_hash
            || material.body.players_commitment != body.players_commitment
        {
            return Err(InvalidPublication("current material is not bound").into());
        }
        let approval = self
            .patch_approval(body.proposal_id)?
            .ok_or(InvalidPublication("terminal Approved patch required"))?;
        let subject = PatchVotePayloadV1::decode(approval.payload.as_bytes())?;
        self.validate_patch_subject_and_stage(body, &subject)?;
        let prev_height = previous
            .ok_or(InvalidPublication("previous receipt missing"))?
            .accepted_height;
        Ok(approval
            .approved_height
            .max(material.accepted_height)
            .max(prev_height))
    }

    fn validate_patch_subject_and_stage(
        &self,
        body: &PublicationBody,
        subject: &PatchVotePayloadV1,
    ) -> Result<()> {
        let expected_stage = if body.action == PublicationAction::Review {
            PATCH_EXECUTION_PENDING
        } else {
            PATCH_EXECUTION_REVIEWED
        };
        if subject.repo_id != body.repo_id || subject.patch_id != body.patch_id {
            return Err(InvalidPublication("exact patch subject/execution stage").into());
        }
        if subject.revision_id != body.revision_id
            || subject.expected_target_head != body.expected_target_head
            || self.patch_execution_stage.read(&body.proposal_id)? != expected_stage
        {
            return Err(InvalidPublication("exact patch subject/execution stage").into());
        }
        Ok(())
    }

    fn validate_canonical_anchor(
        &self,
        body: &PublicationBody,
        expected_anchor: u64,
    ) -> Result<()> {
        if body.finalized_anchor_height != expected_anchor {
            return Err(InvalidPublication("job anchor height").into());
        }
        let canonical = if expected_anchor == 0 {
            Some(self.storage.genesis_hash()?)
        } else {
            self.storage.canonical_block_hash(expected_anchor)?
        };
        if canonical != Some(body.finalized_anchor_hash) {
            return Err(InvalidPublication("canonical job anchor").into());
        }
        Ok(())
    }

    fn validate_non_patch_body(&self, body: &PublicationBody) -> Result<()> {
        if !body.proposal_id.is_zero()
            || !body.patch_id.is_zero()
            || !body.revision_id.is_zero()
            || !body.expected_target_head.is_zero()
        {
            return Err(InvalidPublication("non-patch action has patch fields").into());
        }
        Ok(())
    }
}

fn validate_committee_members(
    committee: &CommitteeSnapshot,
    effective: &[outbe_validatorset::ValidatorRecord],
) -> Result<()> {
    let n = committee.committee.len();
    let unique: std::collections::BTreeSet<_> = committee
        .committee
        .iter()
        .map(|member| member.address)
        .collect();
    if n == 0
        || n > outbe_consensus::bls::MAX_VALIDATORS as usize
        || n != effective.len()
        || unique.len() != n
    {
        return Err(InvalidPublication("effective committee/material").into());
    }
    if committee
        .committee
        .windows(2)
        .any(|pair| pair[0].consensus_pubkey >= pair[1].consensus_pubkey)
    {
        return Err(InvalidPublication("effective committee/material").into());
    }
    if committee.committee.iter().any(|member| {
        !effective.iter().any(|active| {
            active.validator_address == member.address
                && active.consensus_pubkey.as_slice() == member.consensus_pubkey
        })
    }) {
        return Err(InvalidPublication("effective committee/material").into());
    }
    Ok(())
}

fn validate_committee_material(
    committee: &CommitteeSnapshot,
    body: &PublicationBody,
) -> Result<()> {
    if committee.vrf_material_version != body.material_version
        || committee.vrf_public_polynomial_hash.is_zero()
        || committee.vrf_group_public_key_bytes.as_slice() != body.owner_bls_public.as_slice()
    {
        return Err(InvalidPublication("effective committee/material").into());
    }
    let commitment = players_commitment(
        committee
            .committee
            .iter()
            .map(|member| (member.address, &member.consensus_pubkey)),
    )?;
    if commitment != body.players_commitment {
        return Err(InvalidPublication("effective committee/material").into());
    }
    Ok(())
}
