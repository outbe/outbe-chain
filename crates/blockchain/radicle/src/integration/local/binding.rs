//! Current public material and a fixed no-op native snapshot from the local validating node.
use super::*;
use crate::integration::{
    publication::binding::{verify_material_publication, VerifiedMaterialPublication},
    signing::{witness::RepositoryWitnessBundle, MAX_PUBLIC_OUTPUT_BYTES},
};
use crate::manager::FinalizedMaterialSigningState;
use radicle::{crypto::BlsPublicKey, git};
use tokio::sync::OwnedSemaphorePermit;

pub(super) const MAGIC: &[u8; 4] = b"OMB1";
const MAX_FRAME: usize = 5
    + state::MAX_AUTHORITY_BYTES
    + 2 * (8 + 32 + PUBLICATION_BODY_LEN)
    + 40
    + 4
    + MAX_PUBLIC_OUTPUT_BYTES;
pub struct MaterialSigningContext {
    pub state: FinalizedMaterialSigningState,
    pub material: RepositorySigningMaterial,
}

async fn current(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
) -> Result<MaterialSigningContext, Error> {
    let authority = authority.ok_or(Error::Unavailable)?;
    let resolver = resolver.ok_or(Error::Unavailable)?;
    let state = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        authority.read_current_material_signing_state()
    })
    .await
    .map_err(|_| Error::Unavailable)?
    .map_err(|_| Error::Unavailable)?;
    let owner = BlsPublicKey::from_bytes(state.checkpoint().receipt().body.owner_bls_public.0)
        .map_err(|_| Error::Protocol)?;
    let material = resolver
        .repository_signing_material(owner)
        .await
        .map_err(|_| Error::Unavailable)?;
    if material.authority() != state.authority() {
        return Err(Error::Unavailable);
    }
    Ok(MaterialSigningContext { state, material })
}

pub(super) async fn read(
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    resolver: Option<EndpointNetworkResolver>,
    permit: OwnedSemaphorePermit,
) -> Result<Vec<u8>, Error> {
    let context = current(authority, resolver, permit).await?;
    let mut bytes = [
        MAGIC.as_slice(),
        &[2],
        &state::encode_authority(context.state.authority())?,
    ]
    .concat();
    for receipt in [
        context.state.checkpoint().receipt(),
        context.state.material().receipt(),
    ] {
        bytes.extend(receipt.accepted_height.to_be_bytes());
        bytes.extend(receipt.receipt_hash.0);
        bytes.extend(receipt.body.to_bytes());
    }
    bytes.extend(context.state.anchor().number.to_be_bytes());
    bytes.extend(context.state.anchor().hash.0);
    let output = context.material.canonical_output();
    bytes.extend((output.len() as u32).to_be_bytes());
    bytes.extend(output);
    Ok(bytes)
}

impl LocalReceiptClient {
    pub async fn read_material_signing_context(&self) -> Result<MaterialSigningContext, Error> {
        let bytes = self.exchange(MAGIC, MAX_FRAME).await?;
        if bytes == [MAGIC.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if bytes.len() > MAX_FRAME
            || bytes.get(..5) != Some([MAGIC.as_slice(), &[2]].concat().as_slice())
        {
            return Err(Error::Protocol);
        }
        let mut input = state::Input(&bytes[5..]);
        let authority = state::decode_authority(&mut input, self.chain)?;
        let block = authority.block();
        let checkpoint = input.receipt(block, self.chain)?;
        let bound = input.receipt(block, self.chain)?;
        let anchor = FinalizedBlock {
            number: input.u64()?,
            hash: B256::from(input.array::<32>()?),
        };
        let length = u32::from_be_bytes(input.array()?) as usize;
        if length > MAX_PUBLIC_OUTPUT_BYTES {
            return Err(Error::Protocol);
        }
        let output = input.take(length)?;
        let body = &checkpoint.receipt().body;
        if !input.0.is_empty()
            || !matches!(
                bound.receipt().body.action,
                outbe_primitives::radicle_publication::PublicationAction::Bootstrap
                    | outbe_primitives::radicle_publication::PublicationAction::BindMaterial
            )
            || bound.receipt().body.sequence > body.sequence
            || bound.receipt().body.repo_id != body.repo_id
            || bound.receipt().body.owner_bls_public != body.owner_bls_public
            || bound.receipt().body.descriptor_hash != body.descriptor_hash
            || anchor.number > block.number
            || (anchor.number == block.number && anchor.hash != block.hash)
            || (anchor.number == 0 && anchor.hash != self.chain.genesis_hash)
        {
            return Err(Error::Protocol);
        }
        let owner =
            BlsPublicKey::from_bytes(body.owner_bls_public.0).map_err(|_| Error::Protocol)?;
        let material =
            RepositorySigningMaterial::from_canonical_output(authority.clone(), output, owner)
                .map_err(|_| Error::Protocol)?;
        Ok(MaterialSigningContext {
            state: FinalizedMaterialSigningState {
                authority,
                checkpoint,
                material: bound,
                anchor,
            },
            material,
        })
    }
    pub async fn attest_material_publication(
        &self,
        context: &MaterialSigningContext,
        repo: &radicle::storage::git::Repository,
        package: &VerifiedMaterialPublication,
        witness: &RepositoryWitnessBundle,
    ) -> Result<outbe_primitives::radicle_publication::PublicationAttestation, Error> {
        self.attest_patch_publication(
            &context.material,
            repo,
            package.body(),
            package.manifest(),
            witness,
        )
        .await
    }
    pub async fn queue_material_publication(
        &self,
        repo: &radicle::storage::git::Repository,
        package: &VerifiedMaterialPublication,
        witness: &RepositoryWitnessBundle,
        batch: &outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), Error> {
        self.queue_patch_publication(repo, package.body(), package.manifest(), witness, batch)
            .await
    }
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
    let context = current(authority, resolver, permit).await?;
    let permit = readers
        .try_acquire_owned()
        .map_err(|_| Error::Unavailable)?;
    let (selected, batch) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store = store.ok_or(Error::Unavailable)?;
        let mut store = store.lock().map_err(|_| Error::Unavailable)?;
        let snapshot = store
            .replay_checkpoint_snapshot(reader.as_ref(), context.state.checkpoint())
            .map_err(|_| Error::Unavailable)?;
        let repo = store
            .open_checkpoint_repository(
                reader.as_ref(),
                context.state.checkpoint(),
                &request.path,
                git::UserInfo {
                    alias: radicle::node::Alias::new("outbe"),
                    key: radicle::crypto::PublicKey::from_bytes(
                        request.node.ok_or(Error::Unavailable)?,
                    ),
                },
            )
            .map_err(|_| Error::Unavailable)?;
        let verified = verify_material_publication(
            &context.state,
            &snapshot,
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
            .persist_and_select_material(&repo, Arc::new(verified), &request.witness)
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
