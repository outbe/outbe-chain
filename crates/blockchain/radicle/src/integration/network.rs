use crate::integration::{RadicleStatusHandle, RadicleVotingGate};
use crate::{
    endpoint::{
        sign_response, AnchorSnapshot, AuthorityRecord, ChainIdentity, EndpointActor,
        EndpointAddress, EndpointFrame, EndpointHandle, EndpointProtocol, EndpointResponseBody,
        OsRequestIds, PeerId, ReceiveOutcome, SignedEndpointResponse, VerifiedEndpoint,
        HANDLE_DEADLINE, MAX_ENDPOINT_TTL_BLOCKS, UNKNOWN_ANCHOR_TIMEOUT_MS,
    },
    manager::{BoxFuture, EndpointResolver, FinalizedSnapshot, ManagerError},
};
use alloy_primitives::Address;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::{CheckedSender as _, LimitedSender, Receiver, Recipients};
use commonware_runtime::IoBuf;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalEndpointIdentity {
    pub validator: Address,
    pub node_id: [u8; 32],
    pub addresses: Vec<EndpointAddress>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedEndpointEvidence {
    pub peer: PeerId,
    pub response: SignedEndpointResponse,
    pub encoded_frame: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct EndpointEvidenceHandle(Arc<RwLock<BTreeMap<PeerId, SignedEndpointEvidence>>>);

impl EndpointEvidenceHandle {
    #[must_use]
    pub fn snapshot(&self) -> Vec<SignedEndpointEvidence> {
        self.0
            .read()
            .expect("endpoint evidence lock poisoned")
            .values()
            .cloned()
            .collect()
    }
}

pub struct EndpointNetwork;

pub struct EndpointNetworkService {
    chain: ChainIdentity,
    actor: Option<EndpointActor<OsRequestIds>>,
    handle: EndpointHandle,
    commands: mpsc::Receiver<NetworkCommand>,
    evidence: EndpointEvidenceHandle,
    status: RadicleStatusHandle,
}

#[derive(Clone)]
pub struct EndpointNetworkResolver {
    commands: mpsc::Sender<NetworkCommand>,
}

enum NetworkCommand {
    Refresh {
        snapshot: FinalizedSnapshot,
        result: oneshot::Sender<Result<Vec<VerifiedEndpoint>, ManagerError>>,
    },
    Shutdown {
        result: oneshot::Sender<Result<(), ManagerError>>,
    },
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
        (
            EndpointNetworkService {
                chain,
                actor: Some(actor),
                handle,
                commands: receiver,
                evidence: evidence.clone(),
                status,
            },
            EndpointNetworkResolver { commands },
            evidence,
        )
    }
}

impl EndpointNetworkResolver {
    pub async fn shutdown(&self) -> Result<(), ManagerError> {
        let (result, response) = oneshot::channel();
        self.commands
            .try_send(NetworkCommand::Shutdown { result })
            .map_err(|_| ManagerError::Stopped)?;
        tokio::time::timeout(HANDLE_DEADLINE, response)
            .await
            .map_err(|_| ManagerError::Stopped)?
            .map_err(|_| ManagerError::Stopped)?
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
    pub async fn run<S, R>(
        mut self,
        mut sender: S,
        mut receiver: R,
        signer: bls12381::PrivateKey,
        local: LocalEndpointIdentity,
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey> + Send + 'static,
        R: Receiver<PublicKey = bls12381::PublicKey> + Send + 'static,
        R::Error: std::fmt::Display,
    {
        let actor = tokio::spawn(
            self.actor
                .take()
                .expect("endpoint service may only run once")
                .run(),
        );
        let mut current = None;
        let mut queued = BTreeMap::<PeerId, (SignedEndpointResponse, u64)>::new();
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    match command {
                        Some(NetworkCommand::Refresh { snapshot, result }) => {
                            let refreshed = self.refresh(
                                &mut sender,
                                &local,
                                &snapshot,
                                &mut queued,
                            ).await;
                            current = Some(snapshot);
                            let _ = result.send(refreshed);
                        }
                        Some(NetworkCommand::Shutdown { result }) => {
                            let stopped = self.handle.shutdown().await
                                .map_err(|error| ManagerError::Endpoint(error.to_string()));
                            let _ = result.send(stopped);
                            break;
                        }
                        None => break,
                    }
                }
                received = receiver.recv() => {
                    let (peer, bytes) = received
                        .map_err(|error| ManagerError::Endpoint(error.to_string()))?;
                    self.receive(
                        &mut sender,
                        &signer,
                        &local,
                        current.as_ref(),
                        &mut queued,
                        (peer, bytes),
                    ).await?;
                }
            }
        }
        actor
            .await
            .map_err(|error| ManagerError::Endpoint(error.to_string()))?;
        Ok(())
    }

    async fn refresh<S>(
        &self,
        sender: &mut S,
        local: &LocalEndpointIdentity,
        snapshot: &FinalizedSnapshot,
        queued: &mut BTreeMap<PeerId, (SignedEndpointResponse, u64)>,
    ) -> Result<Vec<VerifiedEndpoint>, ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let anchor = anchor(snapshot)?;
        let now = now_millis();
        queued.retain(|_, (_, deadline)| *deadline > now);
        for resolved in self
            .handle
            .resolve(anchor.clone(), snapshot.block.number, now)
            .await
            .map_err(|error| ManagerError::Endpoint(error.to_string()))?
        {
            let raw = queued.remove(&resolved.peer).map(|(response, _)| response);
            if let (Ok(verified), Some(response)) = (resolved.result, raw) {
                self.publish(resolved.peer, response, verified);
            }
        }
        self.prune(snapshot);

        for validator in snapshot
            .validators
            .iter()
            .filter(|validator| validator.address != local.validator && validator.node_id.is_some())
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

    async fn receive<S>(
        &self,
        sender: &mut S,
        signer: &bls12381::PrivateKey,
        local: &LocalEndpointIdentity,
        snapshot: Option<&FinalizedSnapshot>,
        queued: &mut BTreeMap<PeerId, (SignedEndpointResponse, u64)>,
        received: (bls12381::PublicKey, IoBuf),
    ) -> Result<(), ManagerError>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let (sender_key, bytes) = received;
        let peer = PeerId::from_public_key(&sender_key);
        match EndpointFrame::decode(bytes.as_ref()) {
            Ok(EndpointFrame::Request(request)) => {
                if self.status.snapshot().voting_gate != RadicleVotingGate::SignerAllowed {
                    return Ok(());
                }
                let Some(snapshot) = snapshot else {
                    return Ok(());
                };
                let Some(validator) = snapshot.validators.iter().find(|validator| {
                    validator.address == local.validator
                        && validator.peer == PeerId::from_public_key(&signer.public_key())
                        && validator.node_id == Some(local.node_id)
                }) else {
                    return Ok(());
                };
                let Some(valid_until) = snapshot.block.number.checked_add(MAX_ENDPOINT_TTL_BLOCKS)
                else {
                    return Ok(());
                };
                let response = sign_response(
                    EndpointResponseBody {
                        request_id: request.request_id(),
                        chain_id: self.chain.chain_id,
                        genesis_hash: self.chain.genesis_hash,
                        validator: validator.address,
                        node_id: local.node_id,
                        addresses: local.addresses.clone(),
                        anchor_number: snapshot.block.number,
                        anchor_hash: snapshot.block.hash,
                        valid_until,
                    },
                    signer,
                )
                .map_err(|error| ManagerError::Endpoint(error.to_string()))?;
                let _ = send(
                    sender,
                    peer,
                    EndpointFrame::Response(Box::new(response)).encode(),
                );
            }
            Ok(EndpointFrame::Response(response)) => {
                let Some(snapshot) = snapshot else {
                    return Ok(());
                };
                let response = *response;
                let anchor = (response.body().anchor_number == snapshot.block.number)
                    .then(|| anchor(snapshot))
                    .transpose()?;
                match self
                    .handle
                    .response(
                        peer,
                        response.clone(),
                        snapshot.block.number,
                        anchor,
                        now_millis(),
                    )
                    .await
                {
                    Ok(ReceiveOutcome::Verified(verified)) => {
                        self.publish(peer, response, verified);
                    }
                    Ok(ReceiveOutcome::Queued { .. }) => {
                        queued.insert(
                            peer,
                            (
                                response,
                                now_millis().saturating_add(UNKNOWN_ANCHOR_TIMEOUT_MS),
                            ),
                        );
                    }
                    Err(_) => {}
                }
            }
            Err(_) => {}
        }
        Ok(())
    }

    fn publish(&self, peer: PeerId, response: SignedEndpointResponse, _verified: VerifiedEndpoint) {
        let encoded_frame = EndpointFrame::Response(Box::new(response.clone())).encode();
        self.evidence
            .0
            .write()
            .expect("endpoint evidence lock poisoned")
            .insert(
                peer,
                SignedEndpointEvidence {
                    peer,
                    response,
                    encoded_frame,
                },
            );
    }

    fn prune(&self, snapshot: &FinalizedSnapshot) {
        self.evidence
            .0
            .write()
            .expect("endpoint evidence lock poisoned")
            .retain(|peer, proof| {
                let body = proof.response.body();
                body.valid_until > snapshot.block.number
                    && snapshot.validators.iter().any(|validator| {
                        validator.address == body.validator
                            && validator.peer == *peer
                            && validator.node_id == Some(body.node_id)
                    })
            });
    }
}

pub async fn shutdown_bounded<M, E>(
    deadline: Duration,
    manager: M,
    endpoint: E,
) -> Result<(), ManagerError>
where
    M: Future<Output = Result<(), ManagerError>>,
    E: Future<Output = Result<(), ManagerError>>,
{
    tokio::time::timeout(deadline, async move {
        let (manager, endpoint) = tokio::join!(manager, endpoint);
        manager?;
        endpoint
    })
    .await
    .map_err(|_| ManagerError::Stopped)?
}

fn anchor(snapshot: &FinalizedSnapshot) -> Result<AnchorSnapshot, ManagerError> {
    AnchorSnapshot::new(
        snapshot.block.number,
        snapshot.block.hash,
        snapshot
            .validators
            .iter()
            .filter_map(|validator| {
                Some(AuthorityRecord {
                    validator: validator.address,
                    peer: validator.peer,
                    node_id: validator.node_id?,
                })
            })
            .collect(),
    )
    .map_err(|error| ManagerError::Snapshot(error.to_string()))
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
