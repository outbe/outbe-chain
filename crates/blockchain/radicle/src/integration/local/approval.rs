//! Immutable accepted Vote data for verifying historical native entries.
use super::*;
use crate::manager::FinalizedPatchApproval;
use outbe_radicleregistry::{
    payload::{PatchVotePayloadV1, MAX_PATCH_VOTE_PAYLOAD_BYTES},
    PatchApprovalRecord,
};
use tokio::sync::OwnedSemaphorePermit;

pub(super) const MAGIC: &[u8; 4] = b"OVA1";
const MAX_FRAME: usize = 5 + 8 + 32 + 40 + 32 + 8 + 8 + 2 + MAX_PATCH_VOTE_PAYLOAD_BYTES;

impl LocalReceiptClient {
    pub async fn read_patch_approval(
        &self,
        proposal: U256,
    ) -> Result<Option<FinalizedPatchApproval>, Error> {
        let bytes = self
            .exchange(
                &[MAGIC.as_slice(), &proposal.to_be_bytes::<32>()].concat(),
                MAX_FRAME,
            )
            .await?;
        if bytes == [MAGIC.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if bytes == [MAGIC.as_slice(), &[1]].concat() {
            return Ok(None);
        }
        if bytes.len() > MAX_FRAME
            || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
        {
            return Err(Error::Protocol);
        }
        let mut input = state::Input(&bytes[5..]);
        let chain_id = u64::from_be_bytes(input.array()?);
        let genesis_hash = B256::from(input.array::<32>()?);
        let block = FinalizedBlock {
            number: u64::from_be_bytes(input.array()?),
            hash: B256::from(input.array::<32>()?),
        };
        let proposal_id = U256::from_be_bytes(input.array::<32>()?);
        let approved_height = u64::from_be_bytes(input.array()?);
        let approved_timestamp = u64::from_be_bytes(input.array()?);
        let size = u16::from_be_bytes(input.array()?) as usize;
        if size > MAX_PATCH_VOTE_PAYLOAD_BYTES {
            return Err(Error::Protocol);
        }
        let payload = input.take(size)?;
        PatchVotePayloadV1::decode(payload).map_err(|_| Error::Protocol)?;
        let payload = String::from_utf8(payload.to_vec()).map_err(|_| Error::Protocol)?;
        if !input.0.is_empty()
            || chain_id != self.chain.chain_id
            || genesis_hash != self.chain.genesis_hash
            || proposal_id != proposal
            || block.hash.is_zero()
            || approved_height == 0
            || approved_height > block.number
        {
            return Err(Error::Protocol);
        }
        Ok(Some(FinalizedPatchApproval {
            block,
            approval: PatchApprovalRecord {
                proposal_id,
                payload,
                approved_height,
                approved_timestamp,
            },
        }))
    }
}

pub(super) async fn read(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
    proposal: U256,
) -> Result<Vec<u8>, Error> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let Some(approval) = reader
            .read_current_patch_approval(proposal)
            .map_err(|_| Error::Unavailable)?
        else {
            return Ok([MAGIC.as_slice(), &[1]].concat());
        };
        let root = reader
            .read_current_publication_receipt(1)
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        let record = &approval.approval;
        if record.proposal_id != proposal
            || record.payload.len() > MAX_PATCH_VOTE_PAYLOAD_BYTES
            || record.approved_height == 0
            || record.approved_height > approval.block.number
            || approval.block.hash.is_zero()
        {
            return Err(Error::Protocol);
        }
        PatchVotePayloadV1::decode(record.payload.as_bytes()).map_err(|_| Error::Protocol)?;
        Ok([
            MAGIC.as_slice(),
            &[2],
            &root.receipt().body.chain_id.to_be_bytes(),
            root.receipt().body.genesis_hash.as_slice(),
            &approval.block.number.to_be_bytes(),
            approval.block.hash.as_slice(),
            &record.proposal_id.to_be_bytes::<32>(),
            &record.approved_height.to_be_bytes(),
            &record.approved_timestamp.to_be_bytes(),
            &(record.payload.len() as u16).to_be_bytes(),
            record.payload.as_bytes(),
        ]
        .concat())
    })
    .await
    .map_err(|_| Error::Unavailable)?
}
