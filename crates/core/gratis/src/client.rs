//! Private wallet notes and witness builders for both pledge profiles.
use alloy_primitives::{Address, B256, U256};
use outbe_protocol::{codec, error::Error};
use outbe_zk_canonical::{
    noir::{pledgenote_issue, pledgenote_unpledge},
    pledgenote as hash,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Note {
    pub chain_id: u64,
    pub owner: Address,
    pub secret: B256,
    pub amount: U256,
    pub receipt_context: B256,
}
impl Note {
    pub fn initial(
        chain_id: u64,
        owner: Address,
        modify_key: &[u8; 32],
        amount: U256,
        nonce: u64,
    ) -> Result<Self, Error> {
        let entropy = outbe_tee::protocol::initial_pledge_secret(modify_key, amount, nonce);
        let secret = codec::field_to_b256(&codec::field_from_be_bytes(&entropy))?;
        let note = Self {
            chain_id,
            owner,
            secret,
            amount,
            receipt_context: B256::ZERO,
        };
        note.commitment()?;
        Ok(note)
    }
    pub fn serial(&self) -> Result<B256, Error> {
        if self.owner.is_zero() || self.secret.is_zero() {
            return Err(Error::NonCanonical("zero pledge owner or secret"));
        }
        codec::field_to_b256(&hash::note_sn(
            self.owner,
            codec::field_from_b256(&self.secret)?,
        )?)
    }
    pub fn commitment(&self) -> Result<B256, Error> {
        if self.amount.is_zero() {
            return Err(Error::NonCanonical("zero note amount"));
        }
        codec::field_to_b256(&hash::note_commitment(
            self.chain_id,
            codec::field_from_b256(&self.serial()?)?,
            self.amount,
            codec::field_from_b256(&self.receipt_context)?,
        )?)
    }
    pub fn nullifier(&self) -> Result<B256, Error> {
        codec::field_to_b256(&hash::note_nullifier(
            codec::field_from_b256(&self.commitment()?)?,
            codec::field_from_b256(&self.secret)?,
        )?)
    }
    pub fn change(&self, spend: U256) -> Result<Option<Self>, Error> {
        if spend.is_zero() || spend > self.amount {
            return Err(Error::NonCanonical("invalid pledge spend amount"));
        }
        if spend == self.amount {
            return Ok(None);
        }
        Ok(Some(Self {
            chain_id: self.chain_id,
            owner: self.owner,
            amount: self.amount - spend,
            secret: codec::field_to_b256(&hash::change_key(
                codec::field_from_b256(&self.secret)?,
                codec::field_from_b256(&self.nullifier()?)?,
            )?)?,
            receipt_context: B256::ZERO,
        }))
    }
    /// Store this private key with the position before submitting the issue proof.
    pub fn return_secret(&self, context: B256) -> Result<B256, Error> {
        if context.is_zero() {
            return Err(Error::NonCanonical("zero context"));
        }
        codec::field_to_b256(&hash::return_key(
            codec::field_from_b256(&self.secret)?,
            codec::field_from_b256(&self.nullifier()?)?,
            codec::field_from_b256(&context)?,
        )?)
    }
    pub fn returned(
        &self,
        issue_context: B256,
        position: U256,
        amount: U256,
        released_total: U256,
    ) -> Result<Self, Error> {
        let note = Self {
            chain_id: self.chain_id,
            owner: self.owner,
            secret: self.return_secret(issue_context)?,
            amount,
            receipt_context: codec::field_to_b256(&hash::receipt_context(
                position,
                released_total,
            )?)?,
        };
        note.commitment()?;
        Ok(note)
    }
}
pub fn new_tree(chain_id: u64) -> Result<hash::Tree, Error> {
    hash::Tree::new(
        hash::domain(),
        hash::empty_leaf(chain_id)?,
        crate::pledge::DEPTH,
    )
}
fn path(note: &Note, tree: &hash::Tree) -> Result<(u32, [B256; 32]), Error> {
    let leaf = codec::field_from_b256(&note.commitment()?)?;
    let index = tree
        .leaves()
        .iter()
        .position(|v| *v == leaf)
        .ok_or(Error::NonCanonical("note not in tree"))?;
    let index = u32::try_from(index).map_err(|_| Error::NonCanonical("pledge tree capacity"))?;
    let proof = tree.inclusion_path(u64::from(index))?;
    if proof.domain != hash::domain() || proof.root(leaf)? != tree.root() {
        return Err(Error::NonCanonical("pledge path root"));
    }
    let siblings: Vec<B256> = proof
        .siblings
        .iter()
        .map(codec::field_to_b256)
        .collect::<Result<_, _>>()?;
    Ok((
        index,
        siblings
            .try_into()
            .map_err(|_| Error::NonCanonical("pledge depth"))?,
    ))
}
pub fn issue_inputs(
    note: &Note,
    tree: &hash::Tree,
    amount: U256,
    context: B256,
) -> Result<
    (
        pledgenote_issue::alloy::Witness,
        pledgenote_issue::alloy::PublicInputs,
    ),
    Error,
> {
    let (leaf_index, auth_path) = path(note, tree)?;
    let change = note
        .change(amount)?
        .map(|n| n.commitment())
        .transpose()?
        .unwrap_or(B256::ZERO);
    let serial = hash::note_sn(
        note.owner,
        codec::field_from_b256(&note.return_secret(context)?)?,
    )?;
    Ok((
        pledgenote_issue::alloy::Witness {
            owner: note.owner,
            note_spend_key: note.secret,
            note_amount: note.amount,
            receipt_context: note.receipt_context,
            leaf_index,
            auth_path,
        },
        pledgenote_issue::alloy::PublicInputs {
            chain_id: note.chain_id,
            root: codec::field_to_b256(&tree.root())?,
            nullifier: note.nullifier()?,
            context,
            spend_amount: amount,
            change_commitment: change,
            return_note_serial: codec::field_to_b256(&serial)?,
        },
    ))
}
pub fn unpledge_inputs(
    note: &Note,
    tree: &hash::Tree,
    amount: U256,
    context: B256,
) -> Result<
    (
        pledgenote_unpledge::alloy::Witness,
        pledgenote_unpledge::alloy::PublicInputs,
    ),
    Error,
> {
    if context.is_zero() {
        return Err(Error::NonCanonical("zero context"));
    }
    let (leaf_index, auth_path) = path(note, tree)?;
    let change = note
        .change(amount)?
        .map(|n| n.commitment())
        .transpose()?
        .unwrap_or(B256::ZERO);
    Ok((
        pledgenote_unpledge::alloy::Witness {
            note_spend_key: note.secret,
            note_amount: note.amount,
            receipt_context: note.receipt_context,
            leaf_index,
            auth_path,
        },
        pledgenote_unpledge::alloy::PublicInputs {
            chain_id: note.chain_id,
            root: codec::field_to_b256(&tree.root())?,
            nullifier: note.nullifier()?,
            context,
            spend_amount: amount,
            change_commitment: change,
            owner: note.owner,
        },
    ))
}

pub fn prove_issue(
    note: &Note,
    tree: &hash::Tree,
    amount: U256,
    context: B256,
) -> Result<Vec<u8>, Error> {
    use outbe_protocol::protocol::zk::ProofGenerator;
    use outbe_zk_backend::barretenberg::Barretenberg;
    let (witness, public) = issue_inputs(note, tree, amount, context)?;
    let public = public.try_into()?;
    let proof = ProofGenerator::<pledgenote_issue::PledgenoteIssue>::generate(
        &Barretenberg::default(),
        &witness.try_into()?,
        &public,
    )?;
    pledgenote_issue::encode_combined_proof(public, proof.proof)
}
pub fn prove_unpledge(
    note: &Note,
    tree: &hash::Tree,
    amount: U256,
    context: B256,
) -> Result<Vec<u8>, Error> {
    use outbe_protocol::protocol::zk::ProofGenerator;
    use outbe_zk_backend::barretenberg::Barretenberg;
    let (witness, public) = unpledge_inputs(note, tree, amount, context)?;
    let public = public.try_into()?;
    let proof = ProofGenerator::<pledgenote_unpledge::PledgenoteUnpledge>::generate(
        &Barretenberg::default(),
        &witness.try_into()?,
        &public,
    )?;
    pledgenote_unpledge::encode_combined_proof(public, proof.proof)
}
