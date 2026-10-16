//! Bounded public contribution relay on the existing authenticated Outbe peer channel.

use super::*;
use commonware_codec::DecodeExt as _;
use outbe_primitives::radicle_publication::{PublicationBatch, PublicationBody};

pub(crate) const MAGIC: &[u8; 4] = b"OSIG";
pub const MAX_REPOSITORY_GOSSIP_BYTES: usize =
    6 + outbe_primitives::radicle_publication::PUBLICATION_BODY_LEN + 1 + 100;
pub const MAX_REPOSITORY_GOSSIP_MESSAGES: usize = 4 * outbe_consensus::bls::MAX_VALIDATORS as usize;

#[derive(Clone)]
pub enum RepositoryGossipMessage {
    Partial {
        ordinal: u8,
        frame: signing::RepositoryPartial,
    },
    Attestation {
        body: PublicationBody,
        attestation: PublicationAttestation,
    },
}

#[derive(Clone)]
pub struct ReceivedRepositoryMessage {
    pub sender: bls12381::PublicKey,
    pub message: RepositoryGossipMessage,
}

impl RepositoryGossipMessage {
    pub fn to_bytes(&self) -> Result<Vec<u8>, signing::Error> {
        let (tag, payload) = match self {
            Self::Partial { ordinal, frame } if *ordinal < 3 => {
                (0, [&[*ordinal], frame.to_bytes().as_slice()].concat())
            }
            Self::Attestation { body, attestation } => (
                1,
                PublicationBatch {
                    body: body.clone(),
                    attestations: vec![attestation.clone()],
                }
                .to_bytes()
                .map_err(|_| signing::Error::Partial)?,
            ),
            _ => return Err(signing::Error::Partial),
        };
        Ok([MAGIC.as_slice(), &[1, tag], &payload].concat())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, signing::Error> {
        if bytes.len() > MAX_REPOSITORY_GOSSIP_BYTES
            || bytes.get(..5) != Some([MAGIC.as_slice(), &[1]].concat().as_slice())
        {
            return Err(signing::Error::Partial);
        }
        match bytes.get(5) {
            Some(0) if bytes.len() == 7 + 268 && bytes[6] < 3 => Ok(Self::Partial {
                ordinal: bytes[6],
                frame: signing::RepositoryPartial::from_bytes(&bytes[7..])?,
            }),
            Some(1) => {
                let mut batch = PublicationBatch::from_bytes(&bytes[6..])
                    .map_err(|_| signing::Error::Partial)?;
                if batch.attestations.len() != 1 {
                    return Err(signing::Error::Partial);
                }
                Ok(Self::Attestation {
                    body: batch.body,
                    attestation: batch.attestations.pop().ok_or(signing::Error::Partial)?,
                })
            }
            _ => Err(signing::Error::Partial),
        }
    }

    /// Relay authentication only. The collector also verifies the actual native subject/polynomial
    /// before journaling a partial, and complete native/q evidence before publication attestation.
    pub fn verify_sender(
        &self,
        material: &signing::RepositorySigningMaterial,
        sender: &bls12381::PublicKey,
    ) -> Result<(), signing::Error> {
        match self {
            Self::Partial { ordinal, frame } if *ordinal < 3 => {
                material.verify_relay_partial(sender, frame)
            }
            Self::Attestation { body, attestation } => {
                let member = material
                    .authority()
                    .committee()
                    .committee
                    .get(attestation.participant_index as usize)
                    .ok_or(signing::Error::Participant)?;
                let key = bls12381::PublicKey::decode(member.consensus_pubkey.as_slice())
                    .map_err(|_| signing::Error::Participant)?;
                if &key != sender {
                    return Err(signing::Error::Participant);
                }
                material.verify_publication_attestation(body, attestation)
            }
            _ => Err(signing::Error::Partial),
        }
    }

    fn slot(&self) -> u8 {
        match self {
            Self::Partial { ordinal, .. } => *ordinal,
            Self::Attestation { .. } => 3,
        }
    }
}

impl EndpointNetworkState {
    fn gossip_material(&self) -> Result<signing::RepositorySigningMaterial, signing::Error> {
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
        let owner = radicle::crypto::BlsPublicKey::from_bytes(
            authority
                .committee()
                .vrf_group_public_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| signing::Error::Material)?,
        )?;
        self.repository_material.current(authority, owner)
    }

    pub(super) fn receive_gossip(
        &mut self,
        sender: bls12381::PublicKey,
        bytes: &[u8],
    ) -> Result<(), signing::Error> {
        let message = RepositoryGossipMessage::from_bytes(bytes)?;
        let material = self.gossip_material()?;
        message.verify_sender(&material, &sender)?;
        self.sync_gossip_material(&material);
        // At most Doc/COB/refs + one attestation per authenticated current participant.
        self.repository_inbox
            .insert((PeerId::from_public_key(&sender), message.slot()), message);
        Ok(())
    }

    pub(super) fn drain_gossip(
        &mut self,
    ) -> Result<Vec<ReceivedRepositoryMessage>, signing::Error> {
        let material = match self.gossip_material() {
            Ok(material) => material,
            Err(error) => {
                self.repository_inbox.clear();
                self.repository_inbox_material = None;
                return Err(error);
            }
        };
        self.sync_gossip_material(&material);
        std::mem::take(&mut self.repository_inbox)
            .into_iter()
            .map(|((peer, _), message)| {
                Ok(ReceivedRepositoryMessage {
                    sender: peer
                        .to_public_key()
                        .map_err(|_| signing::Error::Participant)?,
                    message,
                })
            })
            .collect()
    }

    fn sync_gossip_material(&mut self, material: &signing::RepositorySigningMaterial) {
        if self.repository_inbox_material != Some(material.material_hash()) {
            self.repository_inbox.clear();
            self.repository_inbox_material = Some(material.material_hash());
        }
    }

    pub(super) fn gossip<S>(
        &self,
        sender: &mut S,
        identity: &bls12381::PrivateKey,
        message: &RepositoryGossipMessage,
    ) -> Result<usize, signing::Error>
    where
        S: LimitedSender<PublicKey = bls12381::PublicKey>,
    {
        let material = self.gossip_material()?;
        message.verify_sender(&material, &identity.public_key())?;
        let bytes = message.to_bytes()?;
        let own = PeerId::from_public_key(&identity.public_key());
        Ok(material
            .authority()
            .committee()
            .committee
            .iter()
            .filter(|member| {
                let Ok(key) = bls12381::PublicKey::decode(member.consensus_pubkey.as_slice())
                else {
                    return false;
                };
                let peer = PeerId::from_public_key(&key);
                peer != own && send(sender, peer, bytes.clone())
            })
            .count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        integration::{
            bootstrap::prepare_bootstrap_document,
            local::tests::{bootstrap_state, signing_fixture},
        },
        manager::{FinalizedPatchSigningState, FinalizedRepositoryAuthority},
    };
    use outbe_consensus::hybrid::VrfMaterialProvider;

    struct Reader(std::sync::Mutex<FinalizedRepositoryAuthority>);
    impl RepositoryAuthorityReader for Reader {
        fn read_current_repository_authority(
            &self,
        ) -> Result<FinalizedRepositoryAuthority, ManagerError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn read_current_patch_signing_state(
            &self,
            _: alloy_primitives::U256,
        ) -> Result<FinalizedPatchSigningState, ManagerError> {
            Err(ManagerError::Snapshot(
                "relay fixture has no patch approval".into(),
            ))
        }
    }

    #[tokio::test]
    async fn relay_inbox_is_bounded_by_current_participant_and_step_and_discards_old_material() {
        let (patch, output, keys, shares) = signing_fixture(4);
        let bootstrap = bootstrap_state(&patch);
        let reader = Arc::new(Reader(std::sync::Mutex::new(patch.authority.clone())));
        let (_, status) = super::super::super::RadicleStatusChannel::enabled(
            patch.authority.committee.committee[0].address,
            [1; 32],
        );
        let (service, _, _) = EndpointNetwork::build(
            ChainIdentity {
                chain_id: 2026,
                genesis_hash: alloy_primitives::B256::repeat_byte(7),
            },
            status,
        );
        let mut service = service.with_repository_authority_reader(reader.clone());
        let publisher = service.take_repository_material_publisher().unwrap();
        publisher.publish(5, 6, Some(&output));
        let owner = radicle::crypto::BlsPublicKey::from_bytes(
            patch.material.receipt.body.owner_bls_public.0,
        )
        .unwrap();
        let material =
            signing::RepositorySigningMaterial::new(patch.authority.clone(), output.clone(), owner)
                .unwrap();
        let job = prepare_bootstrap_document(bootstrap.descriptor(), &material).unwrap();
        let mut state = service.state;
        let mut saved = None;
        for (i, key) in keys.iter().enumerate() {
            let provider =
                VrfMaterialProvider::new(6, output.public().clone(), Some(shares[i].clone()));
            let frame = material
                .sign_bootstrap_document(
                    &provider,
                    &job,
                    &bootstrap,
                    patch.authority.committee.committee[i].address,
                    key,
                )
                .unwrap();
            for ordinal in 0..3 {
                let message = RepositoryGossipMessage::Partial {
                    ordinal,
                    frame: frame.clone(),
                };
                let bytes = message.to_bytes().unwrap();
                assert_eq!(
                    RepositoryGossipMessage::from_bytes(&bytes)
                        .unwrap()
                        .to_bytes()
                        .unwrap(),
                    bytes
                );
                for _ in 0..10 {
                    state.receive_gossip(key.public_key(), &bytes).unwrap();
                }
                if saved.is_none() {
                    saved = Some(bytes);
                }
            }
            let attestation = PublicationAttestation {
                participant_index: i as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    key.sign(PUBLICATION_NAMESPACE, &job.body().to_bytes())
                        .encode()
                        .as_ref(),
                ),
            };
            let bytes = RepositoryGossipMessage::Attestation {
                body: job.body().clone(),
                attestation,
            }
            .to_bytes()
            .unwrap();
            assert_eq!(bytes.len(), MAX_REPOSITORY_GOSSIP_BYTES);
            assert_eq!(
                RepositoryGossipMessage::from_bytes(&bytes)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
                bytes
            );
            state.receive_gossip(key.public_key(), &bytes).unwrap();
        }
        assert_eq!(state.repository_inbox.len(), 4 * keys.len());
        assert!(state.repository_inbox.len() <= MAX_REPOSITORY_GOSSIP_MESSAGES);
        let saved = saved.unwrap();
        assert!(state.receive_gossip(keys[1].public_key(), &saved).is_err());
        assert!(state
            .receive_gossip(bls12381::PrivateKey::from_seed(999).public_key(), &saved)
            .is_err());
        let mut corrupt = saved.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(state
            .receive_gossip(keys[0].public_key(), &corrupt)
            .is_err());
        for invalid in [
            saved[..6].to_vec(),
            saved[..saved.len() - 1].to_vec(),
            [saved.clone(), vec![0]].concat(),
        ] {
            assert!(RepositoryGossipMessage::from_bytes(&invalid).is_err());
        }
        for (offset, value) in [(4, 2), (5, 2), (6, 3)] {
            let mut invalid = saved.clone();
            invalid[offset] = value;
            assert!(RepositoryGossipMessage::from_bytes(&invalid).is_err());
        }
        assert_eq!(state.drain_gossip().unwrap().len(), 16);
        assert!(state.drain_gossip().unwrap().is_empty());
        state.receive_gossip(keys[0].public_key(), &saved).unwrap();
        {
            let mut next = reader.0.lock().unwrap();
            next.epoch += 1;
            next.committee.vrf_material_version += 1;
        }
        publisher.publish(6, 7, Some(&output));
        assert!(state.drain_gossip().unwrap().is_empty());
        assert!(state.receive_gossip(keys[0].public_key(), &saved).is_err());
        publisher.publish(6, 7, None);
        assert!(state.drain_gossip().is_err());
        assert!(state.repository_inbox.is_empty());
    }
}
