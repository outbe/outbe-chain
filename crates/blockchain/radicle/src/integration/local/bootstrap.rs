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

pub(super) const SIGN_IDENTITY: &[u8; 4] = b"OCI1";
pub(super) const SIGN_REFS: &[u8; 4] = b"OCF1";
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

pub(super) async fn current(
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

pub(super) fn native_request(
    magic: &[u8; 4],
    body: &outbe_primitives::radicle_publication::PublicationBody,
    repo: &radicle::storage::git::Repository,
    manifest: radicle::git::Oid,
    signatures: &[&radicle::crypto::BlsEnvelope],
) -> Result<Vec<u8>, Error> {
    use radicle::storage::{ReadRepository as _, WriteRepository as _};
    use std::os::unix::ffi::OsStrExt as _;
    if repo.id().into_sha1() != Some(body.repo_id.0) || !repo.raw().path().is_absolute() {
        return Err(Error::Protocol);
    }
    let path = repo.raw().path().as_os_str().as_bytes();
    let length = u16::try_from(path.len()).map_err(|_| Error::Protocol)?;
    let mut request = [
        magic.as_slice(),
        body.receipt_hash().as_slice(),
        &length.to_be_bytes(),
        path,
        &manifest.into_sha1().ok_or(Error::Protocol)?,
    ]
    .concat();
    for signature in signatures {
        request.extend(signature.to_bytes());
    }
    Ok(request)
}

pub(super) async fn sign_native(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    readers: Arc<tokio::sync::Semaphore>,
    permit: OwnedSemaphorePermit,
    request: Vec<u8>,
) -> Result<Vec<u8>, Error> {
    use radicle::storage::WriteRepository as _;
    use std::os::unix::ffi::OsStringExt as _;
    let refs_step = request.get(..4) == Some(SIGN_REFS.as_slice());
    let magic = if refs_step { SIGN_REFS } else { SIGN_IDENTITY };
    let mut input = state::Input(&request[4..]);
    let expected = alloy_primitives::B256::from(input.array::<32>()?);
    let length = usize::from(u16::from_be_bytes(input.array()?));
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(input.take(length)?.to_vec()));
    if !path.is_absolute() {
        return Err(Error::Protocol);
    }
    let manifest = radicle::git::Oid::from_sha1(input.array()?);
    let document =
        radicle::crypto::BlsEnvelope::from_bytes(input.take(269)?).map_err(|_| Error::Protocol)?;
    let identity = if refs_step {
        Some(
            radicle::crypto::BlsEnvelope::from_bytes(input.take(269)?)
                .map_err(|_| Error::Protocol)?,
        )
    } else {
        None
    };
    if !input.0.is_empty() {
        return Err(Error::Protocol);
    }
    let signer = resolver.clone().ok_or(Error::Unavailable)?;
    let context = current(authority, resolver, permit).await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    enum Job {
        Identity(crate::integration::bootstrap::PreparedBootstrapIdentity),
        Refs(crate::integration::bootstrap::PreparedBootstrapRefs),
    }
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let job = crate::integration::bootstrap::prepare_bootstrap_document(
            context.state.descriptor(),
            &context.material,
        )
        .map_err(|_| Error::Unavailable)?;
        if job.body().receipt_hash() != expected {
            return Err(Error::Unavailable);
        }
        let repo = radicle::storage::git::Repository::open(path, job.repository_id())
            .map_err(|_| Error::Unavailable)?;
        let manifest = repo
            .raw()
            .find_blob(manifest.into())
            .map_err(|_| Error::Unavailable)?;
        let identity_job = job
            .attach(&repo, &document, manifest.content())
            .map_err(|_| Error::Unavailable)?;
        match identity {
            Some(signature) => Ok(Job::Refs(
                identity_job
                    .attach(&repo, &signature)
                    .map_err(|_| Error::Unavailable)?,
            )),
            None => Ok(Job::Identity(identity_job)),
        }
    })
    .await
    .map_err(|_| Error::Unavailable)??;
    let partial = match job {
        Job::Identity(job) => signer.sign_bootstrap_identity(Arc::new(job)).await,
        Job::Refs(job) => signer.sign_bootstrap_refs(Arc::new(job)).await,
    }
    .map_err(|_| Error::Unavailable)?;
    Ok([magic.as_slice(), &[2], &partial.to_bytes()].concat())
}

pub(super) fn decode_partial(
    bytes: &[u8],
    magic: &[u8; 4],
    context: &BootstrapSigningContext,
) -> Result<
    (
        crate::integration::signing::RepositoryPartial,
        commonware_cryptography::bls12381::PublicKey,
    ),
    Error,
> {
    use commonware_codec::DecodeExt as _;
    if bytes == [magic.as_slice(), &[0]].concat() {
        return Err(Error::Unavailable);
    }
    if bytes.len() != SIGN_FRAME
        || bytes.get(..5) != Some([magic.as_slice(), &[2]].concat().as_slice())
    {
        return Err(Error::Protocol);
    }
    let partial = crate::integration::signing::RepositoryPartial::from_bytes(&bytes[5..])
        .map_err(|_| Error::Protocol)?;
    let member = context
        .state
        .authority()
        .committee()
        .committee
        .get(partial.participant_index() as usize)
        .ok_or(Error::Protocol)?;
    let sender =
        commonware_cryptography::bls12381::PublicKey::decode(member.consensus_pubkey.as_slice())
            .map_err(|_| Error::Protocol)?;
    Ok((partial, sender))
}
