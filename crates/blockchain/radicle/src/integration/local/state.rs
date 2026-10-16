//! Bounded public signing-state wire data from the authenticated local node.

use super::{ChainIdentity, Error, FinalizedBlock, FinalizedPatchSigningState};
use crate::manager::{
    FinalizedPatchApproval, FinalizedPublicationReceipt, FinalizedRepositoryAuthority,
};
use alloy_primitives::{Address, B256, U256};
use commonware_codec::DecodeExt as _;
use commonware_cryptography::bls12381;
use outbe_primitives::radicle_publication::{
    players_commitment, PublicationAction, PublicationBody, PUBLICATION_BODY_LEN,
};
use outbe_radicleregistry::{
    payload::{PatchVotePayloadV1, MAX_PATCH_VOTE_PAYLOAD_BYTES},
    publication::PublicationReceipt,
    schema::{PATCH_EXECUTION_MERGED, PATCH_EXECUTION_PENDING, PATCH_EXECUTION_REVIEWED},
    PatchApprovalRecord,
};
use outbe_validatorset::state::{CommitteeEntry, CommitteeSnapshot};

pub(super) const MAGIC: &[u8; 4] = b"OCS1";
// Fixed fields + existing protocol maxima for committee and canonical Vote payload.
pub(super) const MAX_FRAME: usize = 5
    + 40
    + 8
    + 32
    + 8
    + 8
    + 32
    + 96
    + 2
    + 68 * outbe_consensus::bls::MAX_VALIDATORS as usize
    + 32
    + 8
    + 8
    + 2
    + MAX_PATCH_VOTE_PAYLOAD_BYTES
    + 2 * (8 + 32 + PUBLICATION_BODY_LEN)
    + 1
    + 40;

pub(super) fn encode(state: &FinalizedPatchSigningState) -> Result<Vec<u8>, Error> {
    let authority = state.authority();
    let committee = authority.committee();
    let approval = &state.approval().approval;
    if committee.committee.is_empty()
        || committee.committee.len() > outbe_consensus::bls::MAX_VALIDATORS as usize
        || committee.vrf_group_public_key_bytes.len() != 96
        || approval.payload.len() > MAX_PATCH_VOTE_PAYLOAD_BYTES
    {
        return Err(Error::Protocol);
    }
    let mut bytes = [MAGIC.as_slice(), &[2]].concat();
    bytes.extend(encode_authority(authority)?);
    bytes.extend(approval.proposal_id.to_be_bytes::<32>());
    bytes.extend(approval.approved_height.to_be_bytes());
    bytes.extend(approval.approved_timestamp.to_be_bytes());
    bytes.extend((approval.payload.len() as u16).to_be_bytes());
    bytes.extend(approval.payload.as_bytes());
    for receipt in [state.checkpoint().receipt(), state.material().receipt()] {
        bytes.extend(receipt.accepted_height.to_be_bytes());
        bytes.extend(receipt.receipt_hash.0);
        bytes.extend(receipt.body.to_bytes());
    }
    bytes.push(state.stage());
    bytes.extend(state.anchor().number.to_be_bytes());
    bytes.extend(state.anchor().hash.0);
    Ok(bytes)
}

pub(super) const MAX_AUTHORITY_BYTES: usize =
    40 + 8 + 32 + 8 + 8 + 32 + 96 + 2 + 68 * outbe_consensus::bls::MAX_VALIDATORS as usize;

pub(super) fn encode_authority(authority: &FinalizedRepositoryAuthority) -> Result<Vec<u8>, Error> {
    let committee = authority.committee();
    if committee.committee.is_empty()
        || committee.committee.len() > outbe_consensus::bls::MAX_VALIDATORS as usize
        || committee.vrf_group_public_key_bytes.len() != 96
    {
        return Err(Error::Protocol);
    }
    let mut bytes = Vec::new();
    bytes.extend(authority.block().number.to_be_bytes());
    bytes.extend(authority.block().hash.0);
    bytes.extend(authority.chain_id().to_be_bytes());
    bytes.extend(authority.genesis_hash().0);
    bytes.extend(authority.epoch().to_be_bytes());
    bytes.extend(committee.vrf_material_version.to_be_bytes());
    bytes.extend(committee.vrf_public_polynomial_hash.0);
    bytes.extend(&committee.vrf_group_public_key_bytes);
    bytes.extend((committee.committee.len() as u16).to_be_bytes());
    for member in &committee.committee {
        bytes.extend(member.address.as_slice());
        bytes.extend(member.consensus_pubkey);
    }
    Ok(bytes)
}

pub(super) fn decode(
    bytes: &[u8],
    proposal_id: U256,
    chain: ChainIdentity,
) -> Result<FinalizedPatchSigningState, Error> {
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() > MAX_FRAME
        || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let mut input = Input(&bytes[5..]);
    let authority = decode_authority(&mut input, chain)?;
    let block = authority.block();
    let chain_id = authority.chain_id();
    let genesis_hash = authority.genesis_hash();
    let epoch = authority.epoch();
    let vrf_material_version = authority.committee().vrf_material_version;
    let vrf_public_polynomial_hash = authority.committee().vrf_public_polynomial_hash;
    let group = authority.committee().vrf_group_public_key_bytes.as_slice();
    let committee = &authority.committee().committee;
    let response_proposal = U256::from_be_bytes(input.array::<32>()?);
    let approved_height = input.u64()?;
    let approved_timestamp = input.u64()?;
    let length = u16::from_be_bytes(input.array()?) as usize;
    if length > MAX_PATCH_VOTE_PAYLOAD_BYTES {
        return Err(Error::Protocol);
    }
    let payload = input.take(length)?;
    let subject = PatchVotePayloadV1::decode(payload).map_err(|_| Error::Protocol)?;
    let payload = String::from_utf8(payload.to_vec()).map_err(|_| Error::Protocol)?;
    let checkpoint = input.receipt(block, chain)?;
    let material = input.receipt(block, chain)?;
    let stage = input.array::<1>()?[0];
    let anchor = FinalizedBlock {
        number: input.u64()?,
        hash: input.array::<32>()?.into(),
    };
    let current = &checkpoint.receipt.body;
    let bound = &material.receipt.body;
    let players = players_commitment(
        committee
            .iter()
            .map(|member| (member.address, &member.consensus_pubkey)),
    )
    .map_err(|_| Error::Protocol)?;
    if !input.0.is_empty()
        || chain_id != chain.chain_id
        || genesis_hash != chain.genesis_hash
        || block.hash.is_zero()
        || vrf_public_polynomial_hash.is_zero()
        || response_proposal != proposal_id
        || approved_height == 0
        || approved_height > block.number
        || !matches!(
            stage,
            PATCH_EXECUTION_PENDING | PATCH_EXECUTION_REVIEWED | PATCH_EXECUTION_MERGED
        )
        || anchor.hash.is_zero()
        || anchor.number
            != approved_height
                .max(checkpoint.receipt.accepted_height)
                .max(material.receipt.accepted_height)
        || (anchor.number == block.number && anchor.hash != block.hash)
        || bound.sequence > current.sequence
        || !matches!(
            bound.action,
            PublicationAction::Bootstrap | PublicationAction::BindMaterial
        )
        || current.repo_id != subject.repo_id
        || bound.repo_id != subject.repo_id
        || current.descriptor_hash.is_zero()
        || current.descriptor_hash != bound.descriptor_hash
        || current.owner_bls_public != bound.owner_bls_public
        || bound.owner_bls_public.as_slice() != group
        || current.epoch != epoch
        || bound.epoch != epoch
        || current.material_version != vrf_material_version
        || bound.material_version != vrf_material_version
        || current.material_hash.is_zero()
        || current.material_hash != bound.material_hash
        || current.players_commitment != players
        || bound.players_commitment != players
    {
        return Err(Error::Protocol);
    }
    Ok(FinalizedPatchSigningState {
        authority,
        approval: FinalizedPatchApproval {
            block,
            approval: PatchApprovalRecord {
                proposal_id,
                payload,
                approved_height,
                approved_timestamp,
            },
        },
        checkpoint,
        material,
        stage,
        anchor,
    })
}

pub(super) fn decode_authority(
    input: &mut Input<'_>,
    chain: ChainIdentity,
) -> Result<FinalizedRepositoryAuthority, Error> {
    let block = FinalizedBlock {
        number: input.u64()?,
        hash: input.array::<32>()?.into(),
    };
    let chain_id = input.u64()?;
    let genesis_hash = B256::from(input.array::<32>()?);
    let epoch = input.u64()?;
    let vrf_material_version = input.u64()?;
    let vrf_public_polynomial_hash = B256::from(input.array::<32>()?);
    let group = input.array::<96>()?;
    radicle::crypto::repository::BlsPublicKey::from_bytes(group).map_err(|_| Error::Protocol)?;
    let count = u16::from_be_bytes(input.array()?) as usize;
    if count == 0 || count > outbe_consensus::bls::MAX_VALIDATORS as usize {
        return Err(Error::Protocol);
    }
    let mut committee = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let address = Address::from(input.array::<20>()?);
        let consensus_pubkey = input.array::<48>()?;
        let key = bls12381::PublicKey::decode(consensus_pubkey.as_slice())
            .map_err(|_| Error::Protocol)?;
        if previous.as_ref().is_some_and(|old| old >= &key)
            || committee
                .iter()
                .any(|member: &CommitteeEntry| member.address == address)
        {
            return Err(Error::Protocol);
        }
        previous = Some(key);
        committee.push(CommitteeEntry {
            address,
            consensus_pubkey,
        });
    }
    if chain_id != chain.chain_id
        || genesis_hash != chain.genesis_hash
        || block.hash.is_zero()
        || vrf_public_polynomial_hash.is_zero()
    {
        return Err(Error::Protocol);
    }
    Ok(FinalizedRepositoryAuthority {
        block,
        chain_id,
        genesis_hash,
        epoch,
        committee: CommitteeSnapshot {
            committee,
            vrf_material_version,
            vrf_group_public_key_bytes: group.to_vec(),
            vrf_public_polynomial_hash,
        },
    })
}

pub(super) struct Input<'a>(pub(super) &'a [u8]);

impl<'a> Input<'a> {
    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let (head, tail) = self.0.split_at_checked(length).ok_or(Error::Protocol)?;
        self.0 = tail;
        Ok(head)
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        self.take(N)?.try_into().map_err(|_| Error::Protocol)
    }

    pub(super) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn receipt(
        &mut self,
        block: FinalizedBlock,
        chain: ChainIdentity,
    ) -> Result<FinalizedPublicationReceipt, Error> {
        let accepted_height = self.u64()?;
        let receipt_hash = B256::from(self.array::<32>()?);
        let body = PublicationBody::from_bytes(self.take(PUBLICATION_BODY_LEN)?)
            .map_err(|_| Error::Protocol)?;
        if accepted_height == 0
            || accepted_height > block.number
            || receipt_hash != body.receipt_hash()
            || body.chain_id != chain.chain_id
            || body.genesis_hash != chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        Ok(FinalizedPublicationReceipt {
            block,
            receipt: PublicationReceipt {
                accepted_height,
                receipt_hash,
                body,
            },
        })
    }
}
