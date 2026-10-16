//! Own-finalized delivery observations for advancing bounded publication batches.

use super::{
    ChainIdentity, Error, FinalizedBlock, LocalReceiptClient, PublicationBody,
    RepositoryReceiptReader,
};
use crate::manager::FinalizedPublicationProgress;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

pub(super) const MAGIC: &[u8; 4] = b"OCL1";
pub(super) const MAX_FRAME: usize =
    5 + 40 + 32 + 2 + 4 * outbe_consensus::bls::MAX_VALIDATORS as usize;

impl LocalReceiptClient {
    pub async fn read_publication_progress(
        &self,
        body: &PublicationBody,
    ) -> Result<FinalizedPublicationProgress, Error> {
        if body.chain_id != self.chain.chain_id || body.genesis_hash != self.chain.genesis_hash {
            return Err(Error::Protocol);
        }
        let request = [MAGIC.as_slice(), &body.to_bytes()].concat();
        decode(&self.exchange(&request, MAX_FRAME).await?, body, self.chain)
    }
}

pub(super) async fn read(
    reader: Arc<dyn RepositoryReceiptReader>,
    permit: OwnedSemaphorePermit,
    body: PublicationBody,
) -> Result<Vec<u8>, Error> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let progress = reader
            .read_current_publication_progress(&body)
            .map_err(|_| Error::Unavailable)?;
        encode(&progress, &body)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

pub(super) fn encode(
    progress: &FinalizedPublicationProgress,
    body: &PublicationBody,
) -> Result<Vec<u8>, Error> {
    if progress.body_hash() != body.receipt_hash() {
        return Err(Error::Protocol);
    }
    if let Some(receipt) = progress.receipt() {
        if receipt.receipt().body != *body
            || receipt.block() != progress.block()
            || !progress.attesters().is_empty()
        {
            return Err(Error::Protocol);
        }
        return Ok([
            MAGIC.as_slice(),
            &[2],
            &super::encode_receipt(Some(receipt)),
        ]
        .concat());
    }
    let indices = progress.attesters();
    validate_indices(indices)?;
    let mut bytes = [
        MAGIC.as_slice(),
        &[1],
        &progress.block().number.to_be_bytes(),
        progress.block().hash.as_slice(),
        progress.body_hash().as_slice(),
        &(indices.len() as u16).to_be_bytes(),
    ]
    .concat();
    for index in indices {
        bytes.extend(index.to_be_bytes());
    }
    Ok(bytes)
}

pub(super) fn decode(
    bytes: &[u8],
    body: &PublicationBody,
    chain: ChainIdentity,
) -> Result<FinalizedPublicationProgress, Error> {
    if body.chain_id != chain.chain_id || body.genesis_hash != chain.genesis_hash {
        return Err(Error::Protocol);
    }
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() > MAX_FRAME || bytes.get(..4) != Some(MAGIC.as_slice()) {
        return Err(Error::Protocol);
    }
    if bytes.get(4) == Some(&2) {
        let receipt =
            super::decode_response(&bytes[5..], body.sequence, chain)?.ok_or(Error::Protocol)?;
        if receipt.receipt().body != *body {
            return Err(Error::Protocol);
        }
        return Ok(FinalizedPublicationProgress {
            block: receipt.block(),
            body_hash: body.receipt_hash(),
            attesters: vec![],
            receipt: Some(receipt),
        });
    }
    if bytes.get(4) != Some(&1) {
        return Err(Error::Protocol);
    }
    let mut input = super::state::Input(&bytes[5..]);
    let block = FinalizedBlock {
        number: u64::from_be_bytes(input.array()?),
        hash: input.array::<32>()?.into(),
    };
    let body_hash = alloy_primitives::B256::from(input.array::<32>()?);
    let count = usize::from(u16::from_be_bytes(input.array()?));
    if count > outbe_consensus::bls::MAX_VALIDATORS as usize
        || body_hash != body.receipt_hash()
        || block.hash.is_zero()
    {
        return Err(Error::Protocol);
    }
    let indices = (0..count)
        .map(|_| input.array().map(u32::from_be_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    if !input.0.is_empty() {
        return Err(Error::Protocol);
    }
    validate_indices(&indices)?;
    Ok(FinalizedPublicationProgress {
        block,
        body_hash,
        attesters: indices,
        receipt: None,
    })
}

fn validate_indices(indices: &[u32]) -> Result<(), Error> {
    if indices.len() > outbe_consensus::bls::MAX_VALIDATORS as usize
        || indices
            .iter()
            .any(|&i| i >= outbe_consensus::bls::MAX_VALIDATORS)
        || indices.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(Error::Protocol);
    }
    Ok(())
}
