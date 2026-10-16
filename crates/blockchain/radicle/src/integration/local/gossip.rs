//! Public contribution relay for the sidecar collector; no secret material is transported.

use super::{state, EndpointNetworkResolver, Error, LocalReceiptClient, RepositorySigningMaterial};
use crate::integration::{
    network::gossip::{MAX_REPOSITORY_GOSSIP_BYTES, MAX_REPOSITORY_GOSSIP_MESSAGES},
    ReceivedRepositoryMessage, RepositoryGossipMessage,
};
use commonware_codec::{DecodeExt as _, Encode as _};
use commonware_cryptography::bls12381;

pub(super) const SEND: &[u8; 4] = b"OCG1";
pub(super) const TAKE: &[u8; 4] = b"OCT1";
const MAX_REPLY: usize =
    7 + MAX_REPOSITORY_GOSSIP_MESSAGES * (48 + 2 + MAX_REPOSITORY_GOSSIP_BYTES);

impl LocalReceiptClient {
    pub async fn gossip_repository_message(
        &self,
        material: &RepositorySigningMaterial,
        message: &RepositoryGossipMessage,
    ) -> Result<usize, Error> {
        self.check_relay_chain(material)?;
        let request = [
            SEND.as_slice(),
            &message.to_bytes().map_err(|_| Error::Protocol)?,
        ]
        .concat();
        let response = self.exchange(&request, 7).await?;
        if response == [SEND.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if response.len() != 7 || response[..5] != [SEND.as_slice(), &[2]].concat() {
            return Err(Error::Protocol);
        }
        let sent = usize::from(u16::from_be_bytes(
            response[5..].try_into().map_err(|_| Error::Protocol)?,
        ));
        if sent >= material.authority().committee().committee.len() {
            return Err(Error::Protocol);
        }
        Ok(sent)
    }

    pub async fn take_repository_messages(
        &self,
        material: &RepositorySigningMaterial,
    ) -> Result<Vec<ReceivedRepositoryMessage>, Error> {
        self.check_relay_chain(material)?;
        let response = self.exchange(TAKE, MAX_REPLY).await?;
        if response == [TAKE.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if response.get(..5) != Some([TAKE.as_slice(), &[2]].concat().as_slice()) {
            return Err(Error::Protocol);
        }
        let mut input = state::Input(&response[5..]);
        let count = usize::from(u16::from_be_bytes(input.array()?));
        if count > 4 * material.authority().committee().committee.len() {
            return Err(Error::Protocol);
        }
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            let sender =
                bls12381::PublicKey::decode(input.take(48)?).map_err(|_| Error::Protocol)?;
            let length = usize::from(u16::from_be_bytes(input.array()?));
            if length > MAX_REPOSITORY_GOSSIP_BYTES {
                return Err(Error::Protocol);
            }
            let message = RepositoryGossipMessage::from_bytes(input.take(length)?)
                .map_err(|_| Error::Protocol)?;
            message
                .verify_sender(material, &sender)
                .map_err(|_| Error::Protocol)?;
            messages.push(ReceivedRepositoryMessage { sender, message });
        }
        if !input.0.is_empty() {
            return Err(Error::Protocol);
        }
        Ok(messages)
    }

    fn check_relay_chain(&self, material: &RepositorySigningMaterial) -> Result<(), Error> {
        if material.authority().chain_id() != self.chain.chain_id
            || material.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

pub(super) async fn relay(
    resolver: Option<EndpointNetworkResolver>,
    request: &[u8],
) -> Result<Vec<u8>, Error> {
    let resolver = resolver.ok_or(Error::Unavailable)?;
    if request.get(..4) == Some(SEND.as_slice()) {
        let message =
            RepositoryGossipMessage::from_bytes(&request[4..]).map_err(|_| Error::Protocol)?;
        let sent = resolver
            .gossip_repository_message(message)
            .await
            .map_err(|_| Error::Unavailable)?;
        return Ok([
            SEND.as_slice(),
            &[2],
            &u16::try_from(sent)
                .map_err(|_| Error::Protocol)?
                .to_be_bytes(),
        ]
        .concat());
    }
    let messages = resolver
        .take_repository_messages()
        .await
        .map_err(|_| Error::Unavailable)?;
    if messages.len() > MAX_REPOSITORY_GOSSIP_MESSAGES {
        return Err(Error::Protocol);
    }
    let mut response = [
        TAKE.as_slice(),
        &[2],
        &(messages.len() as u16).to_be_bytes(),
    ]
    .concat();
    for received in messages {
        let message = received.message.to_bytes().map_err(|_| Error::Protocol)?;
        response.extend(received.sender.encode());
        response.extend((message.len() as u16).to_be_bytes());
        response.extend(message);
    }
    Ok(response)
}
