//! Same-host ZeroMQ control transport for durable OCOMP discovery offers.
//!
//! The transport deliberately carries only fixed-size [`DiscoveryOfferRefV1`]
//! and [`DiscoveryAckRefV1`] values. The durable discovery spool owns delivery
//! correctness; a successful send has no persistence semantics.

use std::net::SocketAddr;

use bytes::Bytes;
use thiserror::Error;
use zeromq::{DealerSocket, Endpoint, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

use crate::discovery_control::{DiscoveryAckRefV1, DiscoveryControlError, DiscoveryOfferRefV1};

pub struct DiscoveryOfferClientV1 {
    socket: DealerSocket,
}

impl DiscoveryOfferClientV1 {
    pub async fn connect(address: SocketAddr) -> Result<Self, DiscoveryTransportErrorV1> {
        require_loopback(address)?;
        let mut socket = DealerSocket::new();
        socket
            .connect(&format!("tcp://{address}"))
            .await
            .map_err(transport)?;
        Ok(Self { socket })
    }

    /// Sends one wake-up/reference message. This method does not acknowledge,
    /// delete, or otherwise mutate durable spool state.
    pub async fn send_offer(
        &mut self,
        reference: &DiscoveryOfferRefV1,
    ) -> Result<(), DiscoveryTransportErrorV1> {
        let body = reference.encode_fixed();
        self.socket
            .send(Bytes::copy_from_slice(&body).into())
            .await
            .map_err(transport)
    }

    pub async fn receive_ack(
        &mut self,
        offered: &DiscoveryOfferRefV1,
    ) -> Result<DiscoveryAckRefV1, DiscoveryTransportErrorV1> {
        let message = self.socket.recv().await.map_err(transport)?;
        let frames = message.into_vec();
        if frames.len() != 1 {
            return Err(DiscoveryTransportErrorV1::MalformedMessage);
        }
        let acknowledged = DiscoveryAckRefV1::decode_fixed(&frames[0])?;
        if &acknowledged.offer_ref() != offered {
            return Err(DiscoveryTransportErrorV1::ConflictingAcknowledgment);
        }
        Ok(acknowledged)
    }
}

pub struct DiscoveryOfferServerV1 {
    socket: RouterSocket,
    address: SocketAddr,
}

impl DiscoveryOfferServerV1 {
    pub async fn bind(address: SocketAddr) -> Result<Self, DiscoveryTransportErrorV1> {
        require_loopback(address)?;
        let mut socket = RouterSocket::new();
        let endpoint = socket
            .bind(&format!("tcp://{address}"))
            .await
            .map_err(transport)?;
        let address = match endpoint {
            Endpoint::Tcp(_, port) => SocketAddr::new(address.ip(), port),
            _ => return Err(DiscoveryTransportErrorV1::NonTcpEndpoint),
        };
        Ok(Self { socket, address })
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub async fn receive_offer(
        &mut self,
    ) -> Result<ReceivedDiscoveryOfferV1, DiscoveryTransportErrorV1> {
        let frames = self.socket.recv().await.map_err(transport)?.into_vec();
        if frames.len() != 2 || frames[0].is_empty() {
            return Err(DiscoveryTransportErrorV1::MalformedMessage);
        }
        let reference = DiscoveryOfferRefV1::decode_fixed(&frames[1])?;
        Ok(ReceivedDiscoveryOfferV1 {
            route: frames[0].clone(),
            reference,
        })
    }

    pub async fn send_ack(
        &mut self,
        received: &ReceivedDiscoveryOfferV1,
        acknowledgment: &DiscoveryAckRefV1,
    ) -> Result<(), DiscoveryTransportErrorV1> {
        if &acknowledgment.offer_ref() != received.reference() {
            return Err(DiscoveryTransportErrorV1::ConflictingAcknowledgment);
        }
        let body = acknowledgment.encode_fixed();
        let message =
            ZmqMessage::try_from(vec![received.route.clone(), Bytes::copy_from_slice(&body)])
                .map_err(|_| DiscoveryTransportErrorV1::MalformedMessage)?;
        self.socket.send(message).await.map_err(transport)
    }
}

pub struct ReceivedDiscoveryOfferV1 {
    route: Bytes,
    reference: DiscoveryOfferRefV1,
}

impl ReceivedDiscoveryOfferV1 {
    #[must_use]
    pub const fn reference(&self) -> &DiscoveryOfferRefV1 {
        &self.reference
    }
}

fn require_loopback(address: SocketAddr) -> Result<(), DiscoveryTransportErrorV1> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(DiscoveryTransportErrorV1::NonLoopback(address))
    }
}

fn transport(error: zeromq::ZmqError) -> DiscoveryTransportErrorV1 {
    DiscoveryTransportErrorV1::Transport(error.to_string())
}

#[derive(Debug, Error)]
pub enum DiscoveryTransportErrorV1 {
    #[error(transparent)]
    Control(#[from] DiscoveryControlError),
    #[error("OCOMP discovery ZeroMQ endpoint {0} must be loopback")]
    NonLoopback(SocketAddr),
    #[error("OCOMP discovery ZeroMQ binding did not produce a TCP endpoint")]
    NonTcpEndpoint,
    #[error("malformed OCOMP discovery ZeroMQ control message")]
    MalformedMessage,
    #[error("OCOMP discovery acknowledgment conflicts with the offered reference")]
    ConflictingAcknowledgment,
    #[error("OCOMP discovery ZeroMQ transport failed: {0}")]
    Transport(String),
}
