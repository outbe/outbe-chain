//! Complete native packages are verified and persisted before attesting or forwarding.

use super::{bootstrap, state, Error};
use crate::integration::{
    bootstrap::verify_bootstrap_publication,
    publication::{PublicationManifest, MAX_MANIFEST_BYTES},
    signing::witness::{RepositoryWitnessBundle, MAX_WITNESS_BYTES},
};
use outbe_primitives::radicle_publication::{
    PublicationAction, PublicationBatch, PublicationBody, MAX_PUBLICATION_BATCH_LEN,
    PUBLICATION_BODY_LEN,
};
use radicle::{
    git,
    storage::{git::Repository, ReadRepository as _, WriteRepository as _},
};
use std::{
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;

pub(super) const ATTEST: &[u8; 4] = b"OCA1";
pub(super) const QUEUE: &[u8; 4] = b"OCQ1";
pub(super) const ATTEST_FRAME: usize = 5 + PUBLICATION_BODY_LEN + 1 + 100;
pub(super) const MAX_REQUEST: usize = 4
    + 2
    + u16::MAX as usize
    + 20
    + PUBLICATION_BODY_LEN
    + 4
    + MAX_MANIFEST_BYTES
    + 4
    + MAX_WITNESS_BYTES
    + MAX_PUBLICATION_BATCH_LEN;

pub(super) struct PublicationInput<'a> {
    pub body: &'a PublicationBody,
    pub manifest: &'a PublicationManifest,
    pub witness: &'a RepositoryWitnessBundle,
}

pub(super) fn request(
    magic: &[u8; 4],
    repo: &Repository,
    content_manifest: git::Oid,
    input: PublicationInput<'_>,
) -> Result<Vec<u8>, Error> {
    let PublicationInput {
        body,
        manifest,
        witness,
    } = input;
    if repo.id().into_sha1() != Some(body.repo_id.0) || !repo.raw().path().is_absolute() {
        return Err(Error::Protocol);
    }
    let path = repo.raw().path().as_os_str().as_bytes();
    let length = u16::try_from(path.len()).map_err(|_| Error::Protocol)?;
    let manifest = manifest.to_bytes().map_err(|_| Error::Protocol)?;
    let witness = witness.to_bytes().map_err(|_| Error::Protocol)?;
    Ok([
        magic.as_slice(),
        &length.to_be_bytes(),
        path,
        &content_manifest.into_sha1().ok_or(Error::Protocol)?,
        &body.to_bytes(),
        &(manifest.len() as u32).to_be_bytes(),
        &manifest,
        &(witness.len() as u32).to_be_bytes(),
        &witness,
    ]
    .concat())
}

pub(super) struct Request {
    pub path: PathBuf,
    pub content_manifest: git::Oid,
    pub body: PublicationBody,
    pub manifest: PublicationManifest,
    pub witness: RepositoryWitnessBundle,
    pub batch: Option<PublicationBatch>,
    pub node: Option<[u8; 32]>,
}
impl Request {
    pub fn parse(request: &[u8], node: Option<[u8; 32]>) -> Result<Self, Error> {
        let queue = match request.get(..4) {
            Some(magic) if magic == QUEUE => true,
            Some(magic) if magic == ATTEST => false,
            _ => return Err(Error::Protocol),
        };
        let mut input = state::Input(&request[4..]);
        let length = usize::from(u16::from_be_bytes(input.array()?));
        let path = PathBuf::from(std::ffi::OsString::from_vec(input.take(length)?.to_vec()));
        if !path.is_absolute() {
            return Err(Error::Protocol);
        }
        let content_manifest = git::Oid::from_sha1(input.array()?);
        let body = PublicationBody::from_bytes(input.take(PUBLICATION_BODY_LEN)?)
            .map_err(|_| Error::Protocol)?;
        let manifest =
            PublicationManifest::from_bytes(bounded_bytes(&mut input, MAX_MANIFEST_BYTES)?)
                .map_err(|_| Error::Protocol)?;
        let witness =
            RepositoryWitnessBundle::from_bytes(bounded_bytes(&mut input, MAX_WITNESS_BYTES)?)
                .map_err(|_| Error::Protocol)?;
        let batch = if queue {
            let batch = PublicationBatch::from_bytes(input.0).map_err(|_| Error::Protocol)?;
            if batch.body != body {
                return Err(Error::Protocol);
            }
            Some(batch)
        } else {
            if !input.0.is_empty() {
                return Err(Error::Protocol);
            }
            None
        };
        match body.action {
            PublicationAction::Bootstrap => {}
            PublicationAction::Review
            | PublicationAction::Merge
            | PublicationAction::BindMaterial
                if content_manifest.is_zero() && node.is_some() => {}
            _ => return Err(Error::Protocol),
        }
        Ok(Self {
            path,
            content_manifest,
            body,
            manifest,
            witness,
            batch,
            node,
        })
    }
}

fn bounded_bytes<'a>(input: &mut state::Input<'a>, maximum: usize) -> Result<&'a [u8], Error> {
    let length = u32::from_be_bytes(input.array()?) as usize;
    if length > maximum {
        return Err(Error::Protocol);
    }
    input.take(length)
}

pub(super) struct PublishInput {
    pub state: super::serve::ServerState,
    pub permit: OwnedSemaphorePermit,
}

pub(super) async fn publish(input: PublishInput, request: Request) -> Result<Vec<u8>, Error> {
    let PublishInput { state, permit } = input;
    let super::serve::ServerState {
        authority,
        material: resolver,
        publications: store,
        readers,
        ..
    } = state;
    let Request {
        path,
        content_manifest,
        body,
        manifest,
        witness,
        batch,
        ..
    } = request;
    let store = store.ok_or(Error::Unavailable)?;
    let signer = resolver.clone().ok_or(Error::Unavailable)?;
    let context = bootstrap::current(authority, resolver, permit).await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    let selected = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let expected = crate::integration::bootstrap::prepare_bootstrap_document(
            context.state.descriptor(),
            &context.material,
        )
        .map_err(|_| Error::Unavailable)?;
        if expected.body().repo_id != body.repo_id {
            return Err(Error::Protocol);
        }
        let repo =
            Repository::open(path, expected.repository_id()).map_err(|_| Error::Unavailable)?;
        let content = repo
            .raw()
            .find_blob(content_manifest.into())
            .map_err(|_| Error::Unavailable)?;
        let publication = verify_bootstrap_publication(
            &repo,
            crate::integration::bootstrap::BootstrapPublicationInput {
                state: &context.state,
                material: &context.material,
                content_manifest: content.content(),
            },
            &body,
            &manifest,
            &witness,
        )
        .map_err(|_| Error::Unavailable)?;
        // An invalid final batch entry cannot select a durable attestation slot.
        if let Some(batch) = &batch {
            for attestation in &batch.attestations {
                context
                    .material
                    .verify_publication_attestation(&batch.body, attestation)
                    .map_err(|_| Error::Protocol)?;
            }
        }
        let selected = store
            .lock()
            .map_err(|_| Error::Unavailable)?
            .persist_and_select_bootstrap(&repo, Arc::new(publication), &witness)
            .map_err(|_| Error::Unavailable)?;
        Ok((Arc::new(selected), batch))
    })
    .await
    .map_err(|_| Error::Unavailable)??;
    let (selected, batch) = selected;
    if let Some(batch) = batch {
        signer
            .queue_bootstrap_publication(selected, batch)
            .await
            .map_err(|_| Error::Unavailable)?;
        Ok([QUEUE.as_slice(), &[2]].concat())
    } else {
        let attestation = signer
            .attest_bootstrap(selected.clone())
            .await
            .map_err(|_| Error::Unavailable)?;
        let batch = PublicationBatch {
            body: selected.body().clone(),
            attestations: vec![attestation],
        }
        .to_bytes()
        .map_err(|_| Error::Protocol)?;
        Ok([ATTEST.as_slice(), &[2], &batch].concat())
    }
}
