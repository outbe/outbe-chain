use super::*;

impl Job<'_> {
    pub(super) fn ordinal(&self) -> u8 {
        match self {
            Self::Document(_) | Self::Review(_) | Self::Merge(_) => 0,
            Self::Identity(_) | Self::ReviewRefs(..) | Self::MergeRefs(..) => 1,
            Self::Refs(_) => 2,
        }
    }
    pub(super) fn verify(
        &self,
        context: SigningContext<'_>,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<()> {
        match (self, context) {
            (Self::Document(job), SigningContext::Bootstrap(c)) => c
                .material
                .verify_bootstrap_document_partial(job, &c.state, sender, frame)?,
            (Self::Identity(job), SigningContext::Bootstrap(c)) => c
                .material
                .verify_bootstrap_identity_partial(job, &c.state, sender, frame)?,
            (Self::Refs(job), SigningContext::Bootstrap(c)) => c
                .material
                .verify_bootstrap_refs_partial(job, &c.state, sender, frame)?,
            (Self::Review(job), SigningContext::Review(c)) => c
                .material
                .verify_current_review_partial(job, &c.state, sender, frame)?,
            (Self::ReviewRefs(job, _), SigningContext::Review(c)) => c
                .material
                .verify_current_review_refs_partial(job, &c.state, sender, frame)?,
            (Self::Merge(job), SigningContext::Review(c)) => c
                .material
                .verify_current_merge_partial(job, &c.state, sender, frame)?,
            (Self::MergeRefs(job, _), SigningContext::Review(c)) => c
                .material
                .verify_current_merge_refs_partial(job, &c.state, sender, frame)?,
            _ => return Err("collector job/context mismatch".into()),
        }
        Ok(())
    }
    pub(super) fn recover(
        &self,
        context: SigningContext<'_>,
        frames: &Contributions,
    ) -> Result<BlsEnvelope> {
        Ok(match (self, context) {
            (Self::Document(job), SigningContext::Bootstrap(c)) => c
                .material
                .recover_bootstrap_document(job, &c.state, frames)?,
            (Self::Identity(job), SigningContext::Bootstrap(c)) => c
                .material
                .recover_bootstrap_identity(job, &c.state, frames)?,
            (Self::Refs(job), SigningContext::Bootstrap(c)) => {
                c.material.recover_bootstrap_refs(job, &c.state, frames)?
            }
            (Self::Review(job), SigningContext::Review(c)) => {
                c.material.recover_current_review(job, &c.state, frames)?
            }
            (Self::ReviewRefs(job, _), SigningContext::Review(c)) => c
                .material
                .recover_current_review_refs(job, &c.state, frames)?,
            (Self::Merge(job), SigningContext::Review(c)) => {
                c.material.recover_current_merge(job, &c.state, frames)?
            }
            (Self::MergeRefs(job, _), SigningContext::Review(c)) => c
                .material
                .recover_current_merge_refs(job, &c.state, frames)?,
            _ => return Err("collector job/context mismatch".into()),
        })
    }
    pub(super) async fn sign(
        &self,
        client: &LocalReceiptClient,
        context: SigningContext<'_>,
        repo: &Repository,
        artifact: Option<git::Oid>,
    ) -> Result<RepositoryPartial> {
        Ok(match (self, context) {
            (Self::Document(_), SigningContext::Bootstrap(c)) => {
                client.sign_bootstrap_document(c).await?
            }
            (Self::Identity(job), SigningContext::Bootstrap(c)) => {
                client
                    .sign_bootstrap_identity(
                        c,
                        job,
                        repo,
                        artifact.ok_or("bootstrap artifact missing")?,
                    )
                    .await?
            }
            (Self::Refs(job), SigningContext::Bootstrap(c)) => {
                client
                    .sign_bootstrap_refs(
                        c,
                        job,
                        repo,
                        artifact.ok_or("bootstrap artifact missing")?,
                    )
                    .await?
            }
            (Self::Review(job), SigningContext::Review(c)) => {
                client.sign_review(c, repo, job).await?
            }
            (Self::ReviewRefs(job, signature), SigningContext::Review(c)) => {
                client.sign_review_refs(c, repo, job, signature).await?
            }
            (Self::Merge(job), SigningContext::Review(c)) => {
                client.sign_merge(c, repo, job).await?
            }
            (Self::MergeRefs(job, signature), SigningContext::Review(c)) => {
                client.sign_merge_refs(c, repo, job, signature).await?
            }
            _ => return Err("collector job/context mismatch".into()),
        })
    }
}
