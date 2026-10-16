//! Fixed Review jobs reconstructed from this node's finalized permission and archive.
use super::*;
use crate::integration::{
    publication::review::{verify_review_publication, VerifiedReviewPublication},
    review::{
        prepare_patch_review_from_checkpoint, CheckpointPreparedReview,
        CheckpointPreparedReviewRefs,
    },
    signing::RepositoryPartial,
};
use commonware_cryptography::{Hasher as _, Sha256};
use radicle::{
    crypto::{BlsEnvelope, RepositorySubject},
    git,
    storage::{git::Repository, ReadRepository as _, WriteRepository as _},
};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use tokio::sync::OwnedSemaphorePermit;

pub(super) const COB: &[u8; 4] = b"OCR1";
pub(super) const REFS: &[u8; 4] = b"OCV1";
pub(super) const MIN_REQUEST: usize = 4 + 32 + 32 + 2;

pub(super) struct Request {
    magic: &'static [u8; 4],
    proposal: U256,
    subject: B256,
    path: PathBuf,
    signature: Option<BlsEnvelope>,
    node: [u8; 32],
}
impl Request {
    pub fn parse(bytes: &[u8], node: Option<[u8; 32]>) -> Result<Self, Error> {
        let magic = match bytes.get(..4) {
            Some(b) if b == COB => COB,
            Some(b) if b == REFS => REFS,
            _ => return Err(Error::Protocol),
        };
        let mut input = state::Input(&bytes[4..]);
        let proposal = U256::from_be_bytes(input.array::<32>()?);
        let subject = B256::from(input.array::<32>()?);
        let length = usize::from(u16::from_be_bytes(input.array()?));
        let path = PathBuf::from(std::ffi::OsString::from_vec(input.take(length)?.to_vec()));
        if !path.is_absolute() {
            return Err(Error::Protocol);
        }
        let signature = if magic == REFS {
            Some(BlsEnvelope::from_bytes(input.take(269)?).map_err(|_| Error::Protocol)?)
        } else {
            None
        };
        if !input.0.is_empty() {
            return Err(Error::Protocol);
        }
        Ok(Self {
            magic,
            proposal,
            subject,
            path,
            signature,
            node: node.ok_or(Error::Unavailable)?,
        })
    }
}

pub(super) async fn sign(
    reader: Arc<dyn RepositoryReceiptReader>,
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    store: Option<Arc<Mutex<ReviewPackageStore>>>,
    readers: Arc<Semaphore>,
    permit: OwnedSemaphorePermit,
    request: Request,
) -> Result<Vec<u8>, Error> {
    let signer = resolver.clone().ok_or(Error::Unavailable)?;
    let context = preparation::current(
        reader.clone(),
        authority,
        resolver,
        store.clone(),
        readers.clone(),
        permit,
        request.proposal,
    )
    .await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    enum Job {
        Cob(CheckpointPreparedReview),
        Refs(CheckpointPreparedReviewRefs),
    }
    let magic = request.magic;
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store = store.ok_or(Error::Unavailable)?;
        let repo = store
            .lock()
            .map_err(|_| Error::Unavailable)?
            .open_bootstrap_repository(
                reader.as_ref(),
                &request.path,
                git::UserInfo {
                    alias: radicle::node::Alias::new("outbe"),
                    key: radicle::crypto::PublicKey::from_bytes(request.node),
                },
            )
            .map_err(|_| Error::Unavailable)?;
        if repo.id().into_sha1() != Some(context.state.checkpoint().receipt().body.repo_id.0) {
            return Err(Error::Protocol);
        }
        let signing = context
            .material
            .review_context(context.snapshot.checkpoint().receipt().receipt_hash.0);
        let review = prepare_patch_review_from_checkpoint(
            &repo,
            context.state.approval(),
            &context.snapshot,
            &signing,
        )
        .map_err(|_| Error::Unavailable)?;
        let job = match request.signature {
            Some(signature) => Job::Refs(
                review
                    .attach(&repo, &signature)
                    .map_err(|_| Error::Unavailable)?
                    .1,
            ),
            None => Job::Cob(review),
        };
        let subject = match &job {
            Job::Cob(job) => job.subject(),
            Job::Refs(job) => job.subject(),
        };
        if B256::from(Sha256::hash(&[subject.as_bytes()]).0) != request.subject {
            return Err(Error::Unavailable);
        }
        Ok(job)
    })
    .await
    .map_err(|_| Error::Unavailable)??;
    // The actor samples its own authority/material once more immediately before using its share.
    let partial = match job {
        Job::Cob(job) => signer.sign_review(Arc::new(job)).await,
        Job::Refs(job) => signer.sign_review_refs(Arc::new(job)).await,
    }
    .map_err(|_| Error::Unavailable)?;
    Ok([magic.as_slice(), &[2], &partial.to_bytes()].concat())
}

pub(super) async fn publish(
    reader: Arc<dyn RepositoryReceiptReader>,
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    store: Option<Arc<Mutex<ReviewPackageStore>>>,
    readers: Arc<Semaphore>,
    permit: OwnedSemaphorePermit,
    request: publication::Request,
) -> Result<Vec<u8>, Error> {
    let signer = resolver.clone().ok_or(Error::Unavailable)?;
    let context = preparation::current(
        reader.clone(),
        authority,
        resolver,
        store.clone(),
        readers.clone(),
        permit,
        request.body.proposal_id,
    )
    .await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    let (selected, batch) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store = store.ok_or(Error::Unavailable)?;
        let mut store = store.lock().map_err(|_| Error::Unavailable)?;
        let repo = store
            .open_bootstrap_repository(
                reader.as_ref(),
                &request.path,
                git::UserInfo {
                    alias: radicle::node::Alias::new("outbe"),
                    key: radicle::crypto::PublicKey::from_bytes(
                        request.node.ok_or(Error::Unavailable)?,
                    ),
                },
            )
            .map_err(|_| Error::Unavailable)?;
        let verified = verify_review_publication(
            &repo,
            &context.state,
            &context.snapshot,
            &context.material,
            &request.body,
            &request.manifest,
            &request.witness,
        )
        .map_err(|_| Error::Unavailable)?;
        if let Some(batch) = &request.batch {
            for attestation in &batch.attestations {
                context
                    .material
                    .verify_publication_attestation(&batch.body, attestation)
                    .map_err(|_| Error::Protocol)?;
            }
        }
        let selected = store
            .persist_and_select(&repo, Arc::new(verified), &request.witness)
            .map_err(|_| Error::Unavailable)?;
        Ok::<_, Error>((Arc::new(selected), request.batch))
    })
    .await
    .map_err(|_| Error::Unavailable)??;
    if let Some(batch) = batch {
        signer
            .queue_review_publication(selected, batch)
            .await
            .map_err(|_| Error::Unavailable)?;
        Ok([publication::QUEUE.as_slice(), &[2]].concat())
    } else {
        let attestation = signer
            .attest_review(selected.clone())
            .await
            .map_err(|_| Error::Unavailable)?;
        let batch = outbe_primitives::radicle_publication::PublicationBatch {
            body: selected.body().clone(),
            attestations: vec![attestation],
        }
        .to_bytes()
        .map_err(|_| Error::Protocol)?;
        Ok([publication::ATTEST.as_slice(), &[2], &batch].concat())
    }
}

fn request(
    magic: &[u8; 4],
    chain: ChainIdentity,
    context: &PatchPreparationContext,
    repo: &Repository,
    subject: &RepositorySubject,
    signature: Option<&BlsEnvelope>,
) -> Result<Vec<u8>, Error> {
    if context.state.authority().chain_id() != chain.chain_id
        || context.state.authority().genesis_hash() != chain.genesis_hash
        || repo.id().into_sha1() != Some(context.state.checkpoint().receipt().body.repo_id.0)
        || !repo.raw().path().is_absolute()
    {
        return Err(Error::Protocol);
    }
    let path = repo.raw().path().as_os_str().as_bytes();
    let length = u16::try_from(path.len()).map_err(|_| Error::Protocol)?;
    let mut bytes = [
        magic.as_slice(),
        &context
            .state
            .approval()
            .approval
            .proposal_id
            .to_be_bytes::<32>(),
        Sha256::hash(&[subject.as_bytes()]).as_ref(),
        &length.to_be_bytes(),
        path,
    ]
    .concat();
    if let Some(signature) = signature {
        bytes.extend(signature.to_bytes());
    }
    Ok(bytes)
}

impl LocalReceiptClient {
    pub async fn attest_review_publication(
        &self,
        context: &PatchPreparationContext,
        repo: &Repository,
        package: &VerifiedReviewPublication,
        witness: &super::super::signing::witness::RepositoryWitnessBundle,
    ) -> Result<outbe_primitives::radicle_publication::PublicationAttestation, Error> {
        if package.body().chain_id != self.chain.chain_id
            || package.body().genesis_hash != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let request = publication::request(
            publication::ATTEST,
            repo,
            git::Oid::ZERO_SHA1,
            package.body(),
            package.manifest(),
            witness,
        )?;
        let bytes = self.exchange(&request, publication::ATTEST_FRAME).await?;
        if bytes == [publication::ATTEST.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if bytes.get(..5) != Some([publication::ATTEST.as_slice(), &[2]].concat().as_slice()) {
            return Err(Error::Protocol);
        }
        let mut batch =
            outbe_primitives::radicle_publication::PublicationBatch::from_bytes(&bytes[5..])
                .map_err(|_| Error::Protocol)?;
        if batch.body != *package.body() || batch.attestations.len() != 1 {
            return Err(Error::Protocol);
        }
        let attestation = batch.attestations.pop().ok_or(Error::Protocol)?;
        context
            .material
            .verify_publication_attestation(&batch.body, &attestation)
            .map_err(|_| Error::Protocol)?;
        Ok(attestation)
    }

    pub async fn queue_review_publication(
        &self,
        repo: &Repository,
        package: &VerifiedReviewPublication,
        witness: &super::super::signing::witness::RepositoryWitnessBundle,
        batch: &outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), Error> {
        if batch.body != *package.body()
            || batch.body.chain_id != self.chain.chain_id
            || batch.body.genesis_hash != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let mut request = publication::request(
            publication::QUEUE,
            repo,
            git::Oid::ZERO_SHA1,
            package.body(),
            package.manifest(),
            witness,
        )?;
        request.extend(batch.to_bytes().map_err(|_| Error::Protocol)?);
        let response = self.exchange(&request, 5).await?;
        if response == [publication::QUEUE.as_slice(), &[2]].concat() {
            Ok(())
        } else if response == [publication::QUEUE.as_slice(), &[0]].concat() {
            Err(Error::Unavailable)
        } else {
            Err(Error::Protocol)
        }
    }

    pub async fn sign_review(
        &self,
        context: &PatchPreparationContext,
        repo: &Repository,
        job: &CheckpointPreparedReview,
    ) -> Result<RepositoryPartial, Error> {
        let request = request(COB, self.chain, context, repo, job.subject(), None)?;
        let bytes = self.exchange(&request, bootstrap::SIGN_FRAME).await?;
        let (frame, sender) = bootstrap::decode_partial(&bytes, COB, &context.material)?;
        context
            .material
            .verify_current_review_partial(job, &context.state, &sender, &frame)
            .map_err(|_| Error::Protocol)?;
        Ok(frame)
    }
    pub async fn sign_review_refs(
        &self,
        context: &PatchPreparationContext,
        repo: &Repository,
        job: &CheckpointPreparedReviewRefs,
        review_signature: &BlsEnvelope,
    ) -> Result<RepositoryPartial, Error> {
        let request = request(
            REFS,
            self.chain,
            context,
            repo,
            job.subject(),
            Some(review_signature),
        )?;
        let bytes = self.exchange(&request, bootstrap::SIGN_FRAME).await?;
        let (frame, sender) = bootstrap::decode_partial(&bytes, REFS, &context.material)?;
        context
            .material
            .verify_current_review_refs_partial(job, &context.state, &sender, &frame)
            .map_err(|_| Error::Protocol)?;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_request_rejects_malformed_paths_steps_and_frames() {
        let path = b"/private/tmp/repository";
        let bytes = [
            COB.as_slice(),
            &U256::ONE.to_be_bytes::<32>(),
            &[1; 32],
            &(path.len() as u16).to_be_bytes(),
            path,
        ]
        .concat();
        let parsed = Request::parse(&bytes, Some([2; 32])).unwrap();
        assert_eq!(parsed.proposal, U256::ONE);
        assert_eq!(parsed.path, Path::new("/private/tmp/repository"));
        assert!(matches!(
            Request::parse(&bytes, None),
            Err(Error::Unavailable)
        ));
        for malformed in [
            bytes[..MIN_REQUEST - 1].to_vec(),
            bytes[..bytes.len() - 1].to_vec(),
            [bytes.clone(), vec![0]].concat(),
            [REFS.as_slice(), &bytes[4..]].concat(),
            [b"BAD1".as_slice(), &bytes[4..]].concat(),
        ] {
            assert!(matches!(
                Request::parse(&malformed, Some([2; 32])),
                Err(Error::Protocol)
            ));
        }
        let mut relative = bytes;
        relative[MIN_REQUEST] = b'x';
        assert!(matches!(
            Request::parse(&relative, Some([2; 32])),
            Err(Error::Protocol)
        ));
    }
}
