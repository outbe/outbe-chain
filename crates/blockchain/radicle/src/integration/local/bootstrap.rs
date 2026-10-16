//! Public unbound-bootstrap context from the authenticated local node.

use super::{state, ChainIdentity, EndpointNetworkResolver, Error};
use crate::{
    integration::signing::{RepositorySigningMaterial, MAX_PUBLIC_OUTPUT_BYTES},
    manager::{FinalizedBootstrapSigningState, RepositoryAuthorityReader},
};
use outbe_primitives::radicle_genesis::{GovernmentsDescriptor, MAX_GOVERNMENTS_DESCRIPTOR_BYTES};
use radicle::crypto::BlsPublicKey;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

pub(super) const SIGN_DOCUMENT: &[u8; 4] = b"OCJ1";
pub(super) const SIGN_FRAME: usize = 5 + 268;
pub(super) const MAGIC: &[u8; 4] = b"OCB1";
pub(super) const MAX_FRAME: usize = 5
    + state::MAX_AUTHORITY_BYTES
    + 2
    + MAX_GOVERNMENTS_DESCRIPTOR_BYTES
    + 4
    + MAX_PUBLIC_OUTPUT_BYTES;

pub struct BootstrapSigningContext {
    pub state: FinalizedBootstrapSigningState,
    pub material: RepositorySigningMaterial,
}

pub(super) async fn read(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    let context = current(authority, resolver, permit).await?;
    encode(&context.state, &context.material)
}

async fn current(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
) -> Result<BootstrapSigningContext, Error> {
    let authority = authority.ok_or(Error::Unavailable)?;
    let resolver = resolver.ok_or(Error::Unavailable)?;
    let state = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        authority.read_current_bootstrap_signing_state()
    })
    .await
    .map_err(|_| Error::Unavailable)?
    .map_err(|_| Error::Unavailable)?;
    let owner = BlsPublicKey::from_bytes(
        state
            .authority()
            .committee()
            .vrf_group_public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::Protocol)?,
    )
    .map_err(|_| Error::Protocol)?;
    let material = resolver
        .repository_signing_material(owner)
        .await
        .map_err(|_| Error::Unavailable)?;
    if material.authority() != state.authority() {
        return Err(Error::Unavailable);
    }
    Ok(BootstrapSigningContext { state, material })
}

pub(super) fn encode(
    state: &FinalizedBootstrapSigningState,
    material: &RepositorySigningMaterial,
) -> Result<Vec<u8>, Error> {
    if material.authority() != state.authority() {
        return Err(Error::Unavailable);
    }
    let authority = state::encode_authority(state.authority())?;
    let descriptor = state.descriptor().to_bytes().map_err(|_| Error::Protocol)?;
    let output = material.canonical_output();
    if output.len() > MAX_PUBLIC_OUTPUT_BYTES {
        return Err(Error::Protocol);
    }
    Ok([
        MAGIC.as_slice(),
        &[2],
        &authority,
        &(descriptor.len() as u16).to_be_bytes(),
        &descriptor,
        &(output.len() as u32).to_be_bytes(),
        &output,
    ]
    .concat())
}

pub(super) fn decode(bytes: &[u8], chain: ChainIdentity) -> Result<BootstrapSigningContext, Error> {
    if bytes == [MAGIC.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() > MAX_FRAME
        || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let mut input = state::Input(&bytes[5..]);
    let authority = state::decode_authority(&mut input, chain)?;
    let length = usize::from(u16::from_be_bytes(input.array()?));
    if length > MAX_GOVERNMENTS_DESCRIPTOR_BYTES {
        return Err(Error::Protocol);
    }
    let descriptor =
        GovernmentsDescriptor::from_bytes(input.take(length)?).map_err(|_| Error::Protocol)?;
    descriptor
        .validate(chain.chain_id, descriptor.genesis_timestamp)
        .map_err(|_| Error::Protocol)?;
    let output = super::material::take(&mut input.0, MAX_PUBLIC_OUTPUT_BYTES)?;
    if !input.0.is_empty() {
        return Err(Error::Protocol);
    }
    let owner = BlsPublicKey::from_bytes(
        authority
            .committee()
            .vrf_group_public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::Protocol)?,
    )
    .map_err(|_| Error::Protocol)?;
    let material =
        RepositorySigningMaterial::from_canonical_output(authority.clone(), output, owner)
            .map_err(|_| Error::Protocol)?;
    Ok(BootstrapSigningContext {
        state: FinalizedBootstrapSigningState {
            authority,
            descriptor,
        },
        material,
    })
}

pub(super) async fn sign_document(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
    expected_job: alloy_primitives::B256,
) -> Result<Vec<u8>, Error> {
    let signer = resolver.clone().ok_or(Error::Unavailable)?;
    let context = current(authority, resolver, permit).await?;
    let job = crate::integration::bootstrap::prepare_bootstrap_document(
        context.state.descriptor(),
        &context.material,
    )
    .map_err(|_| Error::Unavailable)?;
    if job.body().receipt_hash() != expected_job {
        return Err(Error::Unavailable);
    }
    let partial = signer
        .sign_bootstrap_document(Arc::new(job))
        .await
        .map_err(|_| Error::Unavailable)?;
    Ok([SIGN_DOCUMENT.as_slice(), &[2], &partial.to_bytes()].concat())
}
