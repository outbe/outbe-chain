//! Public DKG Output bound to the node's exact finalized patch context.

use super::{state, ChainIdentity, EndpointNetworkResolver, Error, FinalizedPatchSigningState};
use crate::{
    integration::signing::{RepositorySigningMaterial, MAX_PUBLIC_OUTPUT_BYTES},
    manager::RepositoryAuthorityReader,
};
use alloy_primitives::U256;
use radicle::crypto::BlsPublicKey;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

pub(super) const MAGIC: &[u8; 4] = b"OCM1";
pub(super) const MAX_FRAME: usize = 5 + 4 + state::MAX_FRAME + 4 + MAX_PUBLIC_OUTPUT_BYTES;

pub(super) async fn read(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
    proposal_id: U256,
) -> Result<Vec<u8>, Error> {
    let authority = authority.ok_or(Error::Unavailable)?;
    let resolver = resolver.ok_or(Error::Unavailable)?;
    let state = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        authority.read_current_patch_signing_state(proposal_id)
    })
    .await
    .map_err(|_| Error::Unavailable)?
    .map_err(|_| Error::Unavailable)?;
    let owner = BlsPublicKey::from_bytes(state.material().receipt().body.owner_bls_public.0)
        .map_err(|_| Error::Protocol)?;
    let material = resolver
        .repository_signing_material(owner)
        .await
        .map_err(|_| Error::Unavailable)?;
    // The actor samples its own current authority; a transition between the reads requires retry.
    encode(&state, &material)
}

fn bind(
    state: &FinalizedPatchSigningState,
    material: &RepositorySigningMaterial,
) -> Result<(), Error> {
    if material.authority() != state.authority()
        || material.material_hash() != state.material().receipt().body.material_hash.0
        || material.players_commitment() != state.material().receipt().body.players_commitment.0
    {
        return Err(Error::Protocol);
    }
    Ok(())
}

pub(super) fn encode(
    state: &FinalizedPatchSigningState,
    material: &RepositorySigningMaterial,
) -> Result<Vec<u8>, Error> {
    bind(state, material)?;
    let state = state::encode(state)?;
    let output = material.canonical_output();
    if output.len() > MAX_PUBLIC_OUTPUT_BYTES {
        return Err(Error::Protocol);
    }
    Ok([
        MAGIC.as_slice(),
        &[2],
        &(state.len() as u32).to_be_bytes(),
        &state,
        &(output.len() as u32).to_be_bytes(),
        &output,
    ]
    .concat())
}

pub(super) fn decode(
    bytes: &[u8],
    proposal_id: U256,
    chain: ChainIdentity,
) -> Result<(FinalizedPatchSigningState, RepositorySigningMaterial), Error> {
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() > MAX_FRAME
        || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let mut input = &bytes[5..];
    let state = state::decode(take(&mut input, state::MAX_FRAME)?, proposal_id, chain)?;
    let output = take(&mut input, MAX_PUBLIC_OUTPUT_BYTES)?;
    if !input.is_empty() {
        return Err(Error::Protocol);
    }
    let owner = BlsPublicKey::from_bytes(state.material().receipt().body.owner_bls_public.0)
        .map_err(|_| Error::Protocol)?;
    let material =
        RepositorySigningMaterial::from_canonical_output(state.authority().clone(), output, owner)
            .map_err(|_| Error::Protocol)?;
    bind(&state, &material)?;
    Ok((state, material))
}

fn take<'a>(input: &mut &'a [u8], maximum: usize) -> Result<&'a [u8], Error> {
    let (length, rest) = input.split_at_checked(4).ok_or(Error::Protocol)?;
    let length = u32::from_be_bytes(length.try_into().map_err(|_| Error::Protocol)?) as usize;
    if length > maximum {
        return Err(Error::Protocol);
    }
    let (field, rest) = rest.split_at_checked(length).ok_or(Error::Protocol)?;
    *input = rest;
    Ok(field)
}
