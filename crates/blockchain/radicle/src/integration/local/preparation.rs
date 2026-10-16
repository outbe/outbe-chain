//! Receipt-bound manifest delivery and pinned preparation refs from the local node.

use super::{material, ChainIdentity, EndpointNetworkResolver, Error, FinalizedPatchSigningState};
use crate::{
    integration::{
        publication::{
            store::ReviewPackageStore, CheckpointOwnerSnapshot, OwnerRef, PublicationManifest,
            MAX_MANIFEST_BYTES,
        },
        signing::RepositorySigningMaterial,
    },
    manager::{RepositoryAuthorityReader, RepositoryReceiptReader},
};
use alloy_primitives::{B256, U256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const CACHE: &[u8; 4] = b"OCW1";
pub(super) const MAGIC: &[u8; 4] = b"OCP1";
pub(super) const MAX_FRAME: usize = 5 + 4 + material::MAX_FRAME + 4 + MAX_MANIFEST_BYTES + 20;

pub fn publication_store_path(control: &Path) -> PathBuf {
    let mut path = control.as_os_str().to_os_string();
    path.push(".outbe.packages");
    path.into()
}

pub struct PatchPreparationContext {
    pub state: FinalizedPatchSigningState,
    pub material: RepositorySigningMaterial,
    pub snapshot: CheckpointOwnerSnapshot,
}

pub(super) async fn cache(
    reader: Arc<dyn RepositoryReceiptReader>,
    store: Option<Arc<Mutex<ReviewPackageStore>>>,
    permit: OwnedSemaphorePermit,
    request: Vec<u8>,
) -> Result<(), Error> {
    let store = store.ok_or(Error::Unavailable)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let sequence = u64::from_be_bytes(request[4..12].try_into().map_err(|_| Error::Protocol)?);
        let expected = B256::from_slice(&request[12..44]);
        let receipt = reader
            .read_current_publication_receipt(sequence)
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        if receipt.receipt().receipt_hash != expected || receipt.receipt().body.sequence != sequence
        {
            return Err(Error::Protocol);
        }
        let manifest =
            PublicationManifest::from_bytes(&request[44..]).map_err(|_| Error::Protocol)?;
        store
            .lock()
            .map_err(|_| Error::Unavailable)?
            .persist_checkpoint_manifest(&receipt, &manifest)
            .map_err(|_| Error::Unavailable)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

pub(super) async fn read(
    reader: Arc<dyn RepositoryReceiptReader>,
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    store: Option<Arc<Mutex<ReviewPackageStore>>>,
    readers: Arc<Semaphore>,
    permit: OwnedSemaphorePermit,
    proposal_id: U256,
) -> Result<Vec<u8>, Error> {
    let store = store.ok_or(Error::Unavailable)?;
    let (state, material) = material::current(authority, resolver, permit, proposal_id).await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let snapshot = store
            .lock()
            .map_err(|_| Error::Unavailable)?
            .replay_checkpoint_snapshot(reader.as_ref(), state.checkpoint())
            .map_err(|_| Error::Unavailable)?;
        encode(&state, &material, &snapshot)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

pub(super) fn encode(
    state: &FinalizedPatchSigningState,
    material: &RepositorySigningMaterial,
    snapshot: &CheckpointOwnerSnapshot,
) -> Result<Vec<u8>, Error> {
    if snapshot.checkpoint().receipt() != state.checkpoint().receipt() {
        return Err(Error::Protocol);
    }
    let context = material::encode(state, material)?;
    let sigrefs = snapshot.sigrefs().into_sha1().ok_or(Error::Protocol)?;
    let refs = PublicationManifest::new(
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
    )
    .map_err(|_| Error::Protocol)?
    .to_bytes()
    .map_err(|_| Error::Protocol)?;
    Ok([
        MAGIC.as_slice(),
        &[2],
        &(context.len() as u32).to_be_bytes(),
        &context,
        &(refs.len() as u32).to_be_bytes(),
        &refs,
        &sigrefs,
    ]
    .concat())
}

pub(super) fn decode(
    bytes: &[u8],
    proposal_id: U256,
    chain: ChainIdentity,
) -> Result<PatchPreparationContext, Error> {
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() > MAX_FRAME
        || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let mut input = &bytes[5..];
    let (state, material) = material::decode(
        material::take(&mut input, material::MAX_FRAME)?,
        proposal_id,
        chain,
    )?;
    let refs = PublicationManifest::from_bytes(material::take(&mut input, MAX_MANIFEST_BYTES)?)
        .map_err(|_| Error::Protocol)?;
    if !refs.objects().is_empty()
        || !refs.refs().is_empty()
        || !refs.subjects().is_empty()
        || input.len() != 20
    {
        return Err(Error::Protocol);
    }
    let sigrefs = radicle::git::Oid::from_sha1(input.try_into().map_err(|_| Error::Protocol)?);
    let snapshot = CheckpointOwnerSnapshot::from_local_refs(
        state.checkpoint().clone(),
        refs.owner_refs().map_err(|_| Error::Protocol)?,
        sigrefs,
    )
    .map_err(|_| Error::Protocol)?;
    Ok(PatchPreparationContext {
        state,
        material,
        snapshot,
    })
}
