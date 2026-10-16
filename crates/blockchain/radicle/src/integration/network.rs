mod evidence;
pub(crate) mod gossip;
#[cfg(test)]
pub(crate) mod material_tests;
mod receive;

pub use evidence::{EndpointEvidenceHandle, SignedEndpointEvidence};
pub use gossip::{ReceivedRepositoryMessage, RepositoryGossipMessage};

use super::publication::{review::Error as PublicationError, store::SelectedReviewPublication};
use super::review::{CheckpointPreparedReview, CheckpointPreparedReviewRefs};
use super::{material, signing, RepositoryMaterialPublisher};
use crate::integration::{RadicleStatusHandle, RadicleVotingGate};
use crate::{
    endpoint::{
        sign_response, AnchorSnapshot, ChainIdentity, EndpointActor, EndpointAddress,
        EndpointFrame, EndpointHandle, EndpointProtocol, EndpointResponseBody, OsRequestIds,
        PeerId, ReceiveOutcome, SignedEndpointResponse, VerifiedEndpoint, HANDLE_DEADLINE,
        MAX_ADDRESSES, MAX_ENDPOINT_TTL_BLOCKS, UNKNOWN_ANCHOR_TIMEOUT_MS,
    },
    manager::{
        BoxFuture, EndpointResolver, FinalizedSnapshot, ManagerError, RadicleManagerHandle,
        RepositoryAuthorityReader,
    },
};
use alloy_primitives::Address;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::{CheckedSender as _, LimitedSender, Receiver, Recipients};
use commonware_runtime::IoBuf;
use outbe_primitives::radicle_publication::{PublicationAttestation, PUBLICATION_NAMESPACE};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEndpointIdentity {
    pub validator: Address,
    pub node_id: [u8; 32],
    pub addresses: Vec<EndpointAddress>,
}

pub struct LocalEndpointIdentityChannel;

#[derive(Clone)]
pub struct LocalEndpointIdentityPublisher {
    validator: Address,
    expected_node_id: [u8; 32],
    sender: watch::Sender<Option<LocalEndpointIdentity>>,
}

#[derive(Clone)]
pub struct LocalEndpointIdentityHandle {
    validator: Address,
    receiver: watch::Receiver<Option<LocalEndpointIdentity>>,
}

impl LocalEndpointIdentityChannel {
    #[must_use]
    pub fn create(
        initial: LocalEndpointIdentity,
    ) -> (LocalEndpointIdentityPublisher, LocalEndpointIdentityHandle) {
        let validator = initial.validator;
        let expected_node_id = initial.node_id;
        let (sender, receiver) = watch::channel(Some(initial));
        (
            LocalEndpointIdentityPublisher {
                validator,
                expected_node_id,
                sender,
            },
            LocalEndpointIdentityHandle {
                validator,
                receiver,
            },
        )
    }
}

impl LocalEndpointIdentityPublisher {
    pub fn update(&self, node_id: [u8; 32], mut addresses: Vec<EndpointAddress>) -> bool {
        addresses.sort();
        let valid = node_id == self.expected_node_id
            && !addresses.is_empty()
            && addresses.len() <= MAX_ADDRESSES
            && !addresses.windows(2).any(|pair| pair[0] == pair[1]);
        self.sender
            .send_replace(valid.then_some(LocalEndpointIdentity {
                validator: self.validator,
                node_id,
                addresses,
            }));
        valid
    }

    pub fn unavailable(&self) {
        self.sender.send_replace(None);
    }
}

impl LocalEndpointIdentityHandle {
    fn current(&self) -> Option<LocalEndpointIdentity> {
        self.receiver.borrow().clone()
    }
}

pub struct EndpointNetwork;

/// Owned send and receive halves of the endpoint transport.
pub struct EndpointTransport<S, R> {
    pub sender: S,
    pub receiver: R,
}

/// Signing key and current local identity used for endpoint responses.
pub struct EndpointSigningIdentity {
    pub signer: bls12381::PrivateKey,
    pub local: LocalEndpointIdentityHandle,
}

pub struct EndpointNetworkService {
    actor: EndpointActor<OsRequestIds>,
    state: EndpointNetworkState,
    repository_material_publisher: Option<RepositoryMaterialPublisher>,
}

struct EndpointNetworkState {
    chain: ChainIdentity,
    handle: EndpointHandle,
    commands: mpsc::Receiver<NetworkCommand>,
    evidence: EndpointEvidenceHandle,
    status: RadicleStatusHandle,
    repository_material: material::RepositoryMaterialHandle,
    repository_authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    repository_publication: outbe_primitives::radicle_publication::RepositoryPublicationMailbox,
    queued_publication: Option<QueuedPublication>,
    repository_inbox: BTreeMap<(PeerId, u8), RepositoryGossipMessage>,
    repository_inbox_material: Option<[u8; 32]>,
}

#[derive(Clone)]
pub struct EndpointNetworkResolver {
    commands: mpsc::Sender<NetworkCommand>,
}

struct EndpointRefresh<'a> {
    local_validator: Address,
    snapshot: &'a FinalizedSnapshot,
    queued: &'a mut BTreeMap<PeerId, (SignedEndpointResponse, u64)>,
}

enum NetworkCommand {
    Gossip {
        message: Box<RepositoryGossipMessage>,
        result: oneshot::Sender<Result<usize, signing::Error>>,
    },
    DrainGossip {
        result: oneshot::Sender<Result<Vec<ReceivedRepositoryMessage>, signing::Error>>,
    },
    QueueBootstrapPublication {
        publication: Arc<super::publication::store::SelectedBootstrapPublication>,
        batch: Box<outbe_primitives::radicle_publication::PublicationBatch>,
        result: oneshot::Sender<Result<(), super::bootstrap::Error>>,
    },
    QueueReviewPublication {
        publication: Arc<SelectedReviewPublication>,
        batch: Box<outbe_primitives::radicle_publication::PublicationBatch>,
        result: oneshot::Sender<Result<(), PublicationError>>,
    },
    RepositoryBootstrapAttestation {
        publication: Arc<super::publication::store::SelectedBootstrapPublication>,
        result: oneshot::Sender<Result<PublicationAttestation, super::bootstrap::Error>>,
    },
    RepositoryBootstrapPartial {
        job: RepositoryBootstrapJob,
        result: oneshot::Sender<Result<signing::RepositoryPartial, signing::Error>>,
    },
    RepositoryReviewAttestation {
        publication: Arc<SelectedReviewPublication>,
        result: oneshot::Sender<Result<PublicationAttestation, PublicationError>>,
    },
    RepositoryReviewPartial {
        job: RepositoryReviewJob,
        result: oneshot::Sender<Result<signing::RepositoryPartial, signing::Error>>,
    },
    RepositoryMaterial {
        owner: radicle::crypto::BlsPublicKey,
        result: oneshot::Sender<Result<signing::RepositorySigningMaterial, signing::Error>>,
    },
    Refresh {
        snapshot: FinalizedSnapshot,
        result: oneshot::Sender<Result<Vec<VerifiedEndpoint>, ManagerError>>,
    },
    Shutdown {
        result: oneshot::Sender<Result<(), ManagerError>>,
    },
}

pub(crate) enum RepositoryBootstrapJob {
    Document(Arc<super::bootstrap::PreparedBootstrapDocument>),
    Identity(Arc<super::bootstrap::PreparedBootstrapIdentity>),
    Refs(Arc<super::bootstrap::PreparedBootstrapRefs>),
}

enum RepositoryReviewJob {
    Cob(Arc<CheckpointPreparedReview>),
    Refs(Arc<CheckpointPreparedReviewRefs>),
}

enum QueuedPublication {
    Bootstrap(Arc<super::publication::store::SelectedBootstrapPublication>),
    Review(Arc<SelectedReviewPublication>),
}

impl EndpointNetwork {
    #[must_use]
    pub fn build(
        chain: ChainIdentity,
        status: RadicleStatusHandle,
    ) -> (
        EndpointNetworkService,
        EndpointNetworkResolver,
        EndpointEvidenceHandle,
    ) {
        let protocol = EndpointProtocol::new(chain, OsRequestIds);
        let (actor, handle) = EndpointActor::new(protocol);
        let (commands, receiver) = mpsc::channel(256);
        let evidence = EndpointEvidenceHandle::default();
        let (repository_material_publisher, repository_material) = material::channel();
        (
            EndpointNetworkService {
                actor,
                repository_material_publisher: Some(repository_material_publisher),
                state: EndpointNetworkState {
                    chain,
                    handle,
                    commands: receiver,
                    evidence: evidence.clone(),
                    status,
                    repository_material,
                    repository_authority: None,
                    repository_publication: Default::default(),
                    queued_publication: None,
                    repository_inbox: BTreeMap::new(),
                    repository_inbox_material: None,
                },
            },
            EndpointNetworkResolver { commands },
            evidence,
        )
    }
}

impl EndpointNetworkResolver {
    pub async fn gossip_repository_message(
        &self,
        message: RepositoryGossipMessage,
    ) -> Result<usize, signing::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::Gossip {
                message: Box::new(message),
                result,
            })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }
    pub async fn take_repository_messages(
        &self,
    ) -> Result<Vec<ReceivedRepositoryMessage>, signing::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::DrainGossip { result })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }
    pub async fn queue_bootstrap_publication(
        &self,
        publication: Arc<super::publication::store::SelectedBootstrapPublication>,
        batch: outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), super::bootstrap::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::QueueBootstrapPublication {
                publication,
                batch: Box::new(batch),
                result,
            })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }
    /// Forward only a durable complete package with freshly verified current attestations.
    pub async fn queue_review_publication(
        &self,
        publication: Arc<SelectedReviewPublication>,
        batch: outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), PublicationError> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::QueueReviewPublication {
                publication,
                batch: Box::new(batch),
                result,
            })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    /// Rechecks a durably selected local package before signing its fixed publication body.
    pub async fn attest_review(
        &self,
        publication: Arc<SelectedReviewPublication>,
    ) -> Result<PublicationAttestation, PublicationError> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::RepositoryReviewAttestation {
                publication,
                result,
            })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    pub async fn sign_review(
        &self,
        job: Arc<CheckpointPreparedReview>,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        self.review_partial(RepositoryReviewJob::Cob(job)).await
    }
    pub async fn sign_review_refs(
        &self,
        job: Arc<CheckpointPreparedReviewRefs>,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        self.review_partial(RepositoryReviewJob::Refs(job)).await
    }
    pub async fn attest_bootstrap(
        &self,
        publication: Arc<super::publication::store::SelectedBootstrapPublication>,
    ) -> Result<PublicationAttestation, super::bootstrap::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::RepositoryBootstrapAttestation {
                publication,
                result,
            })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    pub async fn sign_bootstrap_document(
        &self,
        job: Arc<super::bootstrap::PreparedBootstrapDocument>,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        self.bootstrap_partial(RepositoryBootstrapJob::Document(job))
            .await
    }
    pub async fn sign_bootstrap_identity(
        &self,
        job: Arc<super::bootstrap::PreparedBootstrapIdentity>,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        self.bootstrap_partial(RepositoryBootstrapJob::Identity(job))
            .await
    }
    pub async fn sign_bootstrap_refs(
        &self,
        job: Arc<super::bootstrap::PreparedBootstrapRefs>,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        self.bootstrap_partial(RepositoryBootstrapJob::Refs(job))
            .await
    }
    async fn bootstrap_partial(
        &self,
        job: RepositoryBootstrapJob,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::RepositoryBootstrapPartial { job, result })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    async fn review_partial(
        &self,
        job: RepositoryReviewJob,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::RepositoryReviewPartial { job, result })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    /// A local bounded request; the service samples its own current finalized authority.
    pub async fn repository_signing_material(
        &self,
        owner: radicle::crypto::BlsPublicKey,
    ) -> Result<signing::RepositorySigningMaterial, signing::Error> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::RepositoryMaterial { owner, result })
            .map_err(|_| signing::Error::Unavailable)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| signing::Error::Unavailable)?
            .map_err(|_| signing::Error::Unavailable)?
    }

    pub async fn shutdown(&self) -> Result<(), ManagerError> {
        let (result, response) = oneshot::channel();
        tokio::time::timeout(HANDLE_DEADLINE, async {
            self.commands
                .send(NetworkCommand::Shutdown { result })
                .await
                .map_err(|_| ManagerError::Endpoint("shutdown mailbox closed".into()))?;
            response
                .await
                .map_err(|_| ManagerError::Endpoint("shutdown acknowledgement closed".into()))?
        })
        .await
        .map_err(|_| ManagerError::ShutdownDeadline("endpoint"))?
    }
}

impl EndpointResolver for EndpointNetworkResolver {
    fn refresh<'a>(
        &'a self,
        snapshot: &'a FinalizedSnapshot,
    ) -> BoxFuture<'a, Result<Vec<VerifiedEndpoint>, ManagerError>> {
        Box::pin(async move {
            let (result, response) = oneshot::channel();
            self.commands
                .try_send(NetworkCommand::Refresh {
                    snapshot: snapshot.clone(),
                    result,
                })
                .map_err(|_| ManagerError::Stopped)?;
            tokio::time::timeout(HANDLE_DEADLINE, response)
                .await
                .map_err(|_| ManagerError::Endpoint("endpoint refresh deadline exceeded".into()))?
                .map_err(|_| ManagerError::Stopped)?
        })
    }
}

impl EndpointNetworkService {
    pub fn with_repository_publication_mailbox(
        mut self,
        mailbox: outbe_primitives::radicle_publication::RepositoryPublicationMailbox,
    ) -> Self {
        self.state.repository_publication = mailbox;
        self
    }
    pub fn with_repository_authority_reader(
        mut self,
        reader: Arc<dyn RepositoryAuthorityReader>,
    ) -> Self {
        self.state.repository_authority = Some(reader);
        self
    }

    /// Transfer the single public-output publisher to the engine lifecycle owner.
    pub fn take_repository_material_publisher(&mut self) -> Option<RepositoryMaterialPublisher> {
        self.repository_material_publisher.take()
    }

    pub async fn run<S, R>(
        self,
        transport: EndpointTransport<S, R>,
        identity: EndpointSigningIdentity,
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey> + Send + 'static,
        R: Receiver<PublicKey = bls12381::PublicKey> + Send + 'static,
        R::Error: std::fmt::Display,
    {
        self.state.run(self.actor, transport, identity).await
    }
}

impl EndpointNetworkState {
    fn bootstrap_publication_material(
        &self,
        publication: &super::publication::store::SelectedBootstrapPublication,
    ) -> Result<signing::RepositorySigningMaterial, super::bootstrap::Error> {
        let state = self
            .repository_authority
            .as_ref()
            .ok_or(signing::Error::Unavailable)?
            .read_current_bootstrap_signing_state()
            .map_err(signing::Error::from)?;
        if state.authority().chain_id() != self.chain.chain_id
            || state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(super::bootstrap::Error::Descriptor);
        }
        let owner =
            radicle::crypto::BlsPublicKey::from_bytes(publication.body().owner_bls_public.0)?;
        let material = self
            .repository_material
            .current(state.authority().clone(), owner)?;
        publication.authorize_current(&state, &material)?;
        Ok(material)
    }

    fn queue_bootstrap_publication(
        &mut self,
        publication: Arc<super::publication::store::SelectedBootstrapPublication>,
        batch: outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), super::bootstrap::Error> {
        if batch.body != *publication.body() || batch.to_bytes().is_err() {
            return Err(signing::Error::Context.into());
        }
        let material = self.bootstrap_publication_material(&publication)?;
        for attestation in &batch.attestations {
            material.verify_publication_attestation(&batch.body, attestation)?;
        }
        self.repository_publication
            .set_ready(Some(batch))
            .map_err(|_| signing::Error::Unavailable)?;
        self.queued_publication = Some(QueuedPublication::Bootstrap(publication));
        Ok(())
    }

    fn refresh_publication_queue(&mut self) {
        let stale = match self.queued_publication.as_ref() {
            Some(QueuedPublication::Bootstrap(publication)) => {
                self.bootstrap_publication_material(publication).is_err()
            }
            Some(QueuedPublication::Review(publication)) => {
                self.review_publication_material(publication).is_err()
            }
            None => false,
        };
        if stale {
            let _ = self.repository_publication.set_ready(None);
            self.queued_publication = None;
        }
    }

    fn queue_review_publication(
        &mut self,
        publication: Arc<SelectedReviewPublication>,
        batch: outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), PublicationError> {
        if batch.body != *publication.body() || batch.to_bytes().is_err() {
            return Err(PublicationError::Context);
        }
        let material = self.review_publication_material(&publication)?;
        for attestation in &batch.attestations {
            material.verify_publication_attestation(&batch.body, attestation)?;
        }
        self.repository_publication
            .set_ready(Some(batch))
            .map_err(|_| signing::Error::Unavailable)?;
        self.queued_publication = Some(QueuedPublication::Review(publication));
        Ok(())
    }

    fn bootstrap_attestation(
        &self,
        publication: &super::publication::store::SelectedBootstrapPublication,
        identity: &bls12381::PrivateKey,
        validator: Address,
    ) -> Result<PublicationAttestation, super::bootstrap::Error> {
        let material = self.bootstrap_publication_material(publication)?;
        let index = material.participant_for(validator, identity)?;
        let signature = identity
            .sign(PUBLICATION_NAMESPACE, &publication.body().to_bytes())
            .encode();
        Ok(PublicationAttestation {
            participant_index: index.get(),
            signature: alloy_primitives::FixedBytes::from_slice(signature.as_ref()),
        })
    }

    fn review_attestation(
        &self,
        publication: &SelectedReviewPublication,
        identity: &bls12381::PrivateKey,
        validator: Address,
    ) -> Result<PublicationAttestation, PublicationError> {
        let material = self.review_publication_material(publication)?;
        let index = material.participant_for(validator, identity)?;
        let signature = identity
            .sign(PUBLICATION_NAMESPACE, &publication.body().to_bytes())
            .encode();
        Ok(PublicationAttestation {
            participant_index: index.get(),
            signature: alloy_primitives::FixedBytes::from_slice(signature.as_ref()),
        })
    }

    fn review_publication_material(
        &self,
        publication: &SelectedReviewPublication,
    ) -> Result<signing::RepositorySigningMaterial, PublicationError> {
        let state = self
            .repository_authority
            .as_ref()
            .ok_or(signing::Error::Unavailable)?
            .read_current_patch_signing_state(publication.body().proposal_id)
            .map_err(signing::Error::from)?;
        if state.authority().chain_id() != self.chain.chain_id
            || state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(PublicationError::Context);
        }
        let owner =
            radicle::crypto::BlsPublicKey::from_bytes(publication.body().owner_bls_public.0)?;
        let material = self
            .repository_material
            .current(state.authority().clone(), owner)?;
        publication.authorize_current(&state, &material)?;
        Ok(material)
    }

    fn bootstrap_partial(
        &self,
        job: &RepositoryBootstrapJob,
        signer: &bls12381::PrivateKey,
        validator: Address,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        let body = match job {
            RepositoryBootstrapJob::Document(job) => job.body(),
            RepositoryBootstrapJob::Identity(job) => job.body(),
            RepositoryBootstrapJob::Refs(job) => job.body(),
        };
        let owner = radicle::crypto::BlsPublicKey::from_bytes(body.owner_bls_public.0)?;
        let state = self
            .repository_authority
            .as_ref()
            .ok_or(signing::Error::Unavailable)?
            .read_current_bootstrap_signing_state()?;
        if state.authority().chain_id() != self.chain.chain_id
            || state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(signing::Error::Material);
        }
        let material = self
            .repository_material
            .current(state.authority().clone(), owner)?;
        let provider = self.repository_material.local_signer()?;
        match job {
            RepositoryBootstrapJob::Document(job) => {
                material.sign_bootstrap_document(&provider, job, &state, validator, signer)
            }
            RepositoryBootstrapJob::Identity(job) => {
                material.sign_bootstrap_identity(&provider, job, &state, validator, signer)
            }
            RepositoryBootstrapJob::Refs(job) => {
                material.sign_bootstrap_refs(&provider, job, &state, validator, signer)
            }
        }
    }

    fn review_partial(
        &self,
        job: &RepositoryReviewJob,
        signer: &bls12381::PrivateKey,
        validator: Address,
    ) -> Result<signing::RepositoryPartial, signing::Error> {
        let (proposal, owner) = match job {
            RepositoryReviewJob::Cob(job) => (job.approval().proposal_id, job.owner()),
            RepositoryReviewJob::Refs(job) => (job.approval().proposal_id, job.owner()),
        };
        let owner = match owner {
            radicle::crypto::RepositoryKey::Bls12381G2(key) => key,
            _ => return Err(signing::Error::Context),
        };
        let state = self
            .repository_authority
            .as_ref()
            .ok_or(signing::Error::Unavailable)?
            .read_current_patch_signing_state(proposal)?;
        if state.authority().chain_id() != self.chain.chain_id
            || state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(signing::Error::Material);
        }
        let material = self
            .repository_material
            .current(state.authority().clone(), owner)?;
        let provider = self.repository_material.local_signer()?;
        match job {
            RepositoryReviewJob::Cob(job) => {
                material.sign_current_review(&provider, job, &state, validator, signer)
            }
            RepositoryReviewJob::Refs(job) => {
                material.sign_current_review_refs(&provider, job, &state, validator, signer)
            }
        }
    }

    fn current_repository_material(
        &self,
        owner: radicle::crypto::BlsPublicKey,
    ) -> Result<signing::RepositorySigningMaterial, signing::Error> {
        let authority = self
            .repository_authority
            .as_ref()
            .ok_or(signing::Error::Unavailable)?
            .read_current_repository_authority()?;
        if authority.chain_id() != self.chain.chain_id
            || authority.genesis_hash() != self.chain.genesis_hash
        {
            return Err(signing::Error::Material);
        }
        self.repository_material.current(authority, owner)
    }

    async fn run<S, R>(
        mut self,
        endpoint_actor: EndpointActor<OsRequestIds>,
        transport: EndpointTransport<S, R>,
        identity: EndpointSigningIdentity,
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey> + Send + 'static,
        R: Receiver<PublicKey = bls12381::PublicKey> + Send + 'static,
        R::Error: std::fmt::Display,
    {
        let EndpointTransport {
            mut sender,
            mut receiver,
        } = transport;
        let EndpointSigningIdentity { signer, local } = identity;
        // Dropping or unwinding the network service also aborts its owned actor.
        let mut actor = JoinSet::new();
        actor.spawn(endpoint_actor.run());
        let mut current = None;
        let mut queued = BTreeMap::<PeerId, (SignedEndpointResponse, u64)>::new();
        let mut shutdown_ack = None;
        let mut material_updates = true;
        let outcome = loop {
            tokio::select! {
                changed = self.repository_material.changed(), if material_updates => {
                    material_updates = changed.is_ok();
                    self.refresh_publication_queue();
                }
                joined = actor.join_next() => {
                    break Err(match joined {
                        Some(Err(error)) => ManagerError::Task(format!("endpoint actor: {error}")),
                        _ => ManagerError::Endpoint("endpoint actor stopped unexpectedly".into()),
                    });
                }
                command = self.commands.recv() => {
                    match command {
                        Some(NetworkCommand::Gossip {message,result})=>{
                            let _ = result.send(self.gossip(&mut sender,&signer,&message));
                        }
                        Some(NetworkCommand::DrainGossip {result})=>{
                            let _ = result.send(self.drain_gossip());
                        }
                        Some(NetworkCommand::QueueBootstrapPublication {publication,batch,result})=>{
                            let _ = result.send(self.queue_bootstrap_publication(publication,*batch));
                        }
                        Some(NetworkCommand::QueueReviewPublication {publication,batch,result})=>{
                            let _ = result.send(self.queue_review_publication(publication,*batch));
                        }
                        Some(NetworkCommand::RepositoryReviewAttestation {publication,result})=>{
                            let _=result.send(self.review_attestation(&publication,&signer,local.validator));
                        }
                        Some(NetworkCommand::RepositoryBootstrapAttestation {publication,result})=>{
                            let attestation = self.bootstrap_attestation(&publication,&signer,local.validator);
                            let _ = result.send(attestation);
                        }
                        Some(NetworkCommand::RepositoryBootstrapPartial {job,result})=>{
                            let partial = self.bootstrap_partial(&job,&signer,local.validator);
                            let _ = result.send(partial);
                        }
                        Some(NetworkCommand::RepositoryReviewPartial {job,result})=>{
                            let _=result.send(self.review_partial(&job,&signer,local.validator));
                        }
                        Some(NetworkCommand::RepositoryMaterial { owner, result }) => {
                            let material = self.current_repository_material(owner);
                            let _ = result.send(material);
                        }
                        Some(NetworkCommand::Refresh { snapshot, result }) => {
                            self.refresh_publication_queue();
                            let refreshed = self.refresh(
                                &mut sender,
                                EndpointRefresh { local_validator: local.validator, snapshot: &snapshot, queued: &mut queued },
                            ).await;
                            current = Some(snapshot);
                            let _ = result.send(refreshed);
                        }
                        Some(NetworkCommand::Shutdown { result }) => {
                            shutdown_ack = Some(result);
                            break Ok(());
                        }
                        None => break Ok(()),
                    }
                }
                received = receiver.recv() => {
                    let (peer, bytes) = match received {
                        Ok(received) => received,
                        Err(error) => break Err(ManagerError::Endpoint(error.to_string())),
                    };
                    if let Err(error) = self.receive(
                        receive::EndpointReceiveContext {
                            sender: &mut sender,
                            signer: &signer,
                            local: &local,
                            snapshot: current.as_ref(),
                            queued: &mut queued,
                        },
                        (peer, bytes),
                    ).await {
                        break Err(error);
                    }
                }
            }
        };
        self.finish_run(&mut actor, outcome, shutdown_ack).await
    }

    async fn finish_run(
        &mut self,
        actor: &mut JoinSet<()>,
        outcome: Result<(), ManagerError>,
        shutdown_ack: Option<oneshot::Sender<Result<(), ManagerError>>>,
    ) -> Result<(), ManagerError> {
        let _ = self.repository_publication.set_ready(None);
        self.queued_publication = None;
        self.commands.close();
        let cleanup = if actor.is_empty() {
            Ok(())
        } else {
            stop_actor(&self.handle, actor).await
        };
        // Preserve a transport failure even if cleanup also fails.
        let outcome = match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => Err(ManagerError::Task(format!(
                "{error}; endpoint actor cleanup also failed: {cleanup}"
            ))),
        };
        if let Some(result) = shutdown_ack {
            let _ = result.send(outcome.clone());
        }
        outcome
    }

    async fn refresh<S>(
        &self,
        sender: &mut S,
        context: EndpointRefresh<'_>,
    ) -> Result<Vec<VerifiedEndpoint>, ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let EndpointRefresh {
            local_validator,
            snapshot,
            queued,
        } = context;
        let anchor = anchor(snapshot)?;
        let now = now_millis();
        queued.retain(|_, (_, deadline)| *deadline > now);
        for resolved in self
            .handle
            .resolve(anchor.clone(), snapshot.block.number, now)
            .await
            .map_err(ManagerError::endpoint)?
        {
            let raw = queued.remove(&resolved.peer).map(|(response, _)| response);
            if let (Ok(verified), Some(response)) = (resolved.result, raw) {
                self.publish(resolved.peer, response, verified);
            }
        }
        self.evidence.prune(snapshot);

        for validator in snapshot
            .validators
            .iter()
            .filter(|validator| validator.address != local_validator && validator.node_id.is_some())
        {
            let request = match self.handle.request(validator.peer, now).await {
                Ok(request) => request,
                Err(_) => continue,
            };
            let request_id = match EndpointFrame::decode(&request) {
                Ok(EndpointFrame::Request(request)) => request.request_id(),
                _ => continue,
            };
            if !send(sender, validator.peer, request) {
                let _ = self.handle.send_failed(validator.peer, request_id).await;
            }
        }
        Ok(self
            .evidence
            .snapshot()
            .into_iter()
            .map(|proof| verified_from_response(proof.peer, &proof.response))
            .collect())
    }

    fn publish(&self, peer: PeerId, response: SignedEndpointResponse, _verified: VerifiedEndpoint) {
        let encoded_frame = EndpointFrame::Response(Box::new(response.clone())).encode();
        self.evidence.publish(SignedEndpointEvidence {
            peer,
            response,
            encoded_frame,
        });
    }
}

/// Drain the manager before closing its endpoint dependency. Each stage gets
/// `deadline`, including endpoint cleanup after a manager failure or timeout.
/// The manager reserves up to one second within its budget for abort-and-join.
pub async fn shutdown_bounded<E>(
    deadline: Duration,
    manager: RadicleManagerHandle,
    endpoint: E,
) -> Result<(), ManagerError>
where
    E: Future<Output = Result<(), ManagerError>>,
{
    let manager = manager.shutdown_bounded(deadline).await;
    let endpoint = tokio::time::timeout(deadline, endpoint)
        .await
        .unwrap_or(Err(ManagerError::ShutdownDeadline("endpoint")));
    match (manager, endpoint) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(manager), Err(endpoint)) => Err(ManagerError::Shutdown {
            manager: Box::new(manager),
            endpoint: Box::new(endpoint),
        }),
    }
}

async fn stop_actor(handle: &EndpointHandle, actor: &mut JoinSet<()>) -> Result<(), ManagerError> {
    let end = tokio::time::Instant::now() + HANDLE_DEADLINE;
    let drain_end = end - Duration::from_secs(1);
    let stopped = tokio::time::timeout_at(drain_end, handle.shutdown())
        .await
        .map_err(|_| ManagerError::ShutdownDeadline("endpoint actor"))
        .and_then(|result| {
            result.map_err(|error| match error {
                crate::endpoint::HandleError::Deadline => {
                    ManagerError::ShutdownDeadline("endpoint actor")
                }
                error => ManagerError::endpoint(error),
            })
        });
    if stopped.is_err() {
        actor.abort_all();
    }
    let join_end = if stopped.is_err() { end } else { drain_end };
    match tokio::time::timeout_at(join_end, actor.join_next()).await {
        Ok(Some(Ok(()))) => stopped,
        // Keep the failure that required cancellation, including a closed ACK.
        Ok(Some(Err(error))) if error.is_cancelled() && stopped.is_err() => stopped,
        // A panic carries more information than the closed ACK it caused.
        Ok(Some(Err(error))) => Err(ManagerError::Task(format!("endpoint actor: {error}"))),
        Ok(None) => Err(ManagerError::Endpoint("endpoint actor join missing".into())),
        Err(_) => cancel_and_join(actor, end).await,
    }
}

async fn cancel_and_join(
    actor: &mut JoinSet<()>,
    end: tokio::time::Instant,
) -> Result<(), ManagerError> {
    actor.abort_all();
    match tokio::time::timeout_at(end, actor.join_next()).await {
        Ok(Some(Err(error))) if !error.is_cancelled() => {
            Err(ManagerError::Task(format!("endpoint actor: {error}")))
        }
        Ok(_) => Err(ManagerError::ShutdownDeadline("endpoint actor")),
        Err(_) => Err(ManagerError::ShutdownDeadline(
            "endpoint actor cancellation",
        )),
    }
}

fn anchor(snapshot: &FinalizedSnapshot) -> Result<AnchorSnapshot, ManagerError> {
    snapshot.endpoint_anchor().map_err(ManagerError::snapshot)
}

fn verified_from_response(peer: PeerId, response: &SignedEndpointResponse) -> VerifiedEndpoint {
    let body = response.body();
    VerifiedEndpoint {
        validator: body.validator,
        peer,
        node_id: body.node_id,
        addresses: body.addresses.clone(),
        anchor_number: body.anchor_number,
        anchor_hash: body.anchor_hash,
        valid_until: body.valid_until,
    }
}

fn send<S>(sender: &mut S, peer: PeerId, bytes: Vec<u8>) -> bool
where
    S: LimitedSender<PublicKey = bls12381::PublicKey>,
{
    let Ok(public_key) = peer.to_public_key() else {
        return false;
    };
    sender
        .check(Recipients::One(public_key))
        .map(|checked| checked.send(bytes, false).accepted())
        .unwrap_or(false)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
pub(crate) mod shutdown_tests {
    use super::*;
    use crate::{endpoint::HandleError, integration::RadicleStatusChannel};
    use alloy_primitives::B256;
    use commonware_actor::{Feedback, Unreliable};
    use commonware_p2p::CheckedSender;
    use commonware_runtime::IoBufs;

    #[derive(Clone)]
    pub(crate) struct NoSend;

    impl LimitedSender for NoSend {
        type PublicKey = bls12381::PublicKey;
        type Checked<'a> = NoSend;

        fn check(
            &mut self,
            _: Recipients<Self::PublicKey>,
        ) -> Result<Self::Checked<'_>, SystemTime> {
            Err(SystemTime::now())
        }
    }

    impl CheckedSender for NoSend {
        type PublicKey = bls12381::PublicKey;

        fn recipients(&self) -> Vec<Self::PublicKey> {
            vec![]
        }

        fn send(self, _: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
            panic!("sender is never admitted")
        }
    }

    #[derive(Debug)]
    pub(crate) struct TestReceiver(
        pub(crate) mpsc::UnboundedReceiver<(bls12381::PublicKey, IoBuf)>,
    );

    impl Receiver for TestReceiver {
        type PublicKey = bls12381::PublicKey;
        type Error = std::io::Error;

        async fn recv(&mut self) -> Result<(Self::PublicKey, IoBuf), Self::Error> {
            self.0.recv().await.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "transport closed witness")
            })
        }
    }

    struct Running {
        task: tokio::task::JoinHandle<Result<(), ManagerError>>,
        resolver: EndpointNetworkResolver,
        handle: EndpointHandle,
        incoming: mpsc::UnboundedSender<(bls12381::PublicKey, IoBuf)>,
    }

    fn network() -> Running {
        let (_, status) = RadicleStatusChannel::enabled(Address::ZERO, [1; 32]);
        let (service, resolver, _) = EndpointNetwork::build(
            ChainIdentity {
                chain_id: 1,
                genesis_hash: B256::ZERO,
            },
            status,
        );
        let handle = service.state.handle.clone();
        let (_, local) = LocalEndpointIdentityChannel::create(LocalEndpointIdentity {
            validator: Address::ZERO,
            node_id: [1; 32],
            addresses: vec![EndpointAddress::dns("local.example", 8776).unwrap()],
        });
        let (incoming, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(service.run(
            EndpointTransport {
                sender: NoSend,
                receiver: TestReceiver(receiver),
            },
            EndpointSigningIdentity {
                signer: bls12381::PrivateKey::from_seed(1),
                local,
            },
        ));
        Running {
            task,
            resolver,
            handle,
            incoming,
        }
    }

    #[tokio::test]
    async fn explicit_shutdown_ack_follows_actor_cleanup() {
        let running = network();
        running.handle.stats().await.unwrap();
        running.resolver.shutdown().await.unwrap();
        assert_eq!(running.handle.stats().await, Err(HandleError::Closed));
        running.task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn transport_failure_is_preserved_and_actor_reaped() {
        let running = network();
        running.handle.stats().await.unwrap();
        drop(running.incoming);
        assert_eq!(
            running.task.await.unwrap(),
            Err(ManagerError::Endpoint("transport closed witness".into()))
        );
        assert_eq!(running.handle.stats().await, Err(HandleError::Closed));
        assert!(running.resolver.shutdown().await.is_err());
    }

    #[tokio::test]
    async fn closing_network_commands_reaps_actor() {
        let running = network();
        running.handle.stats().await.unwrap();
        drop(running.resolver);
        tokio::time::timeout(Duration::from_secs(1), running.task)
            .await
            .expect("command closure must terminate the actor")
            .unwrap()
            .unwrap();
        assert_eq!(running.handle.stats().await, Err(HandleError::Closed));
    }

    #[tokio::test]
    async fn network_cancellation_aborts_actor() {
        let running = network();
        running.handle.stats().await.unwrap();
        running.task.abort();
        assert!(running.task.await.unwrap_err().is_cancelled());
        assert_eq!(running.handle.stats().await, Err(HandleError::Closed));
    }

    #[tokio::test]
    async fn actor_panic_after_ack_is_not_success() {
        let (actor, handle) = EndpointActor::new(EndpointProtocol::new(
            ChainIdentity {
                chain_id: 1,
                genesis_hash: B256::ZERO,
            },
            OsRequestIds,
        ));
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            actor.run().await;
            panic!("actor panic after ack witness");
        });
        let error = stop_actor(&handle, &mut tasks).await.unwrap_err();
        assert!(
            matches!(error, ManagerError::Task(ref message) if message.contains("actor panic after ack witness"))
        );
        assert!(tasks.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_actor_is_aborted_and_reaped_after_ack() {
        let (actor, handle) = EndpointActor::new(EndpointProtocol::new(
            ChainIdentity {
                chain_id: 1,
                genesis_hash: B256::ZERO,
            },
            OsRequestIds,
        ));
        let (alive, mut dropped) = oneshot::channel::<()>();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let _alive = alive;
            actor.run().await;
            std::future::pending::<()>().await;
        });
        assert_eq!(
            stop_actor(&handle, &mut tasks).await,
            Err(ManagerError::ShutdownDeadline("endpoint actor"))
        );
        assert!(tasks.is_empty());
        assert_eq!(
            dropped.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn actor_cancellation_panic_is_not_a_deadline() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("actor cancellation panic witness");
            }
        }
        let (actor, handle) = EndpointActor::new(EndpointProtocol::new(
            ChainIdentity {
                chain_id: 1,
                genesis_hash: B256::ZERO,
            },
            OsRequestIds,
        ));
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            actor.run().await;
            let _guard = PanicOnDrop;
            std::future::pending::<()>().await;
        });
        let error = stop_actor(&handle, &mut tasks).await.unwrap_err();
        assert!(tasks.is_empty());
        assert!(
            matches!(error, ManagerError::Task(ref message) if message.contains("actor cancellation panic witness"))
        );
    }

    #[tokio::test]
    async fn shutdown_preserves_closed_acknowledgement() {
        let (commands, mut receiver) = mpsc::channel(1);
        let resolver = EndpointNetworkResolver { commands };
        let responder = tokio::spawn(async move {
            let Some(NetworkCommand::Shutdown { result }) = receiver.recv().await else {
                panic!("expected shutdown");
            };
            drop(result);
        });
        assert_eq!(
            resolver.shutdown().await,
            Err(ManagerError::Endpoint(
                "shutdown acknowledgement closed".into()
            ))
        );
        responder.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_full_mailbox_waits_until_deadline() {
        let (commands, _receiver) = mpsc::channel(1);
        let (result, _response) = oneshot::channel();
        assert!(commands
            .try_send(NetworkCommand::Shutdown { result })
            .is_ok());
        let resolver = EndpointNetworkResolver { commands };
        let started = tokio::time::Instant::now();
        assert_eq!(
            resolver.shutdown().await,
            Err(ManagerError::ShutdownDeadline("endpoint"))
        );
        assert_eq!(started.elapsed(), HANDLE_DEADLINE);
    }

    #[tokio::test]
    async fn shutdown_closed_mailbox_is_not_deadline() {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        let resolver = EndpointNetworkResolver { commands };
        assert_eq!(
            resolver.shutdown().await,
            Err(ManagerError::Endpoint("shutdown mailbox closed".into()))
        );
    }
}
