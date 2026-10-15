//! Framing protocol.
#![warn(clippy::missing_docs_in_private_items)]
use std::{fmt, io};

use bytes::{Buf, BufMut};
use radicle::node::Link;

use crate::service::Message;
use crate::{PROTOCOL_VERSION, wire, wire::varint, wire::varint::VarInt};

/// Protocol version strings all start with the magic sequence `rad`, followed
/// by a version number.
pub const PROTOCOL_VERSION_STRING: Version = Version([b'r', b'a', b'd', PROTOCOL_VERSION]);

/// Protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version([u8; 4]);

impl Version {
    /// Version number.
    pub fn number(&self) -> u8 {
        self.0[3]
    }
}

impl wire::Encode for Version {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.0);
    }
}

impl wire::Decode for Version {
    fn decode(buf: &mut impl Buf) -> Result<Self, wire::Error> {
        let mut version = [0u8; 4];

        buf.try_copy_to_slice(&mut version[..])?;

        if version[..3] != *b"rad" || !matches!(version[3], 1 | 2) {
            return Err(wire::Invalid::ProtocolVersion { actual: version }.into());
        }
        Ok(Self(version))
    }
}

/// Identifies a (multiplexed) stream.
///
/// Stream IDs are variable-length integers with the least significant 3 bits
/// denoting the stream type and initiator.
///
/// The first bit denotes the initiator (outbound or inbound), while the second
/// and third bit denote the stream type. See `StreamKind`.
///
/// In a situation where Alice connects to Bob, Alice will have the initiator
/// bit set to `1` for all streams she creates, while Bob will have it set to `0`.
///
/// This ensures that Stream IDs never collide.
/// Additionally, Stream IDs must never be reused within a connection.
///
/// +=======+==================================+
/// | Bits  | Stream Type                      |
/// +=======+==================================+
/// | 0b000 | Outbound Control stream          |
/// +-------+----------------------------------+
/// | 0b001 | Inbound Control stream           |
/// +-------+----------------------------------+
/// | 0b010 | Outbound Gossip stream           |
/// +-------+----------------------------------+
/// | 0b011 | Inbound Gossip stream            |
/// +-------+----------------------------------+
/// | 0b100 | Outbound Git stream              |
/// +-------+----------------------------------+
/// | 0b101 | Inbound Git stream               |
/// +-------+----------------------------------+
///
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(VarInt);

impl StreamId {
    /// Get the initiator of this stream.
    pub fn link(&self) -> Link {
        let n = *self.0;
        if 0b1 & n == 0 {
            Link::Outbound
        } else {
            Link::Inbound
        }
    }

    /// Get the kind of stream this is.
    pub fn kind(&self) -> Result<StreamType, u8> {
        let id = *self.0;
        let kind = ((id >> 1) & 0b11) as u8;

        StreamType::try_from(kind)
    }

    /// Create a control identifier.
    pub fn control(link: Link) -> Self {
        let link = if link.is_outbound() { 0 } else { 1 };
        Self(VarInt::from(((u8::from(StreamType::Control)) << 1) | link))
    }

    /// Create a gossip identifier.
    pub fn gossip(link: Link) -> Self {
        let link = if link.is_outbound() { 0 } else { 1 };
        Self(VarInt::from((u8::from(StreamType::Gossip) << 1) | link))
    }

    /// Create a git identifier.
    pub fn git(link: Link) -> Self {
        let link = if link.is_outbound() { 0 } else { 1 };
        Self(VarInt::from((u8::from(StreamType::Git) << 1) | link))
    }

    /// Get the nth identifier while preserving the stream type and initiator.
    pub fn nth(self, n: u64) -> Result<Self, varint::BoundsExceeded> {
        let id = *self.0 + (n << 3);
        VarInt::new(id).map(Self)
    }
}

impl From<StreamId> for u64 {
    fn from(value: StreamId) -> Self {
        *value.0
    }
}

impl From<StreamId> for VarInt {
    fn from(value: StreamId) -> Self {
        value.0
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self.0)
    }
}

impl wire::Decode for StreamId {
    fn decode(buf: &mut impl Buf) -> Result<Self, wire::Error> {
        let id = VarInt::decode(buf)?;
        Ok(Self(id))
    }
}

impl wire::Encode for StreamId {
    fn encode(&self, buf: &mut impl BufMut) {
        self.0.encode(buf)
    }
}

/// Type of stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StreamType {
    /// Control stream, used to open and close streams.
    Control = 0b00,
    /// Gossip stream, used to exchange messages.
    Gossip = 0b01,
    /// Git stream, used for replication.
    Git = 0b10,
}

impl TryFrom<u8> for StreamType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00 => Ok(StreamType::Control),
            0b01 => Ok(StreamType::Gossip),
            0b10 => Ok(StreamType::Git),
            n => Err(n),
        }
    }
}

impl From<StreamType> for u8 {
    fn from(value: StreamType) -> Self {
        value as u8
    }
}

/// Protocol frame.
///
/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |      'r'      |      'a'      |      'd'      |      0x1      | Version
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                                                       |T T T I| Stream ID
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                                                              …| Data (variable size)
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// Note that the last four bits of the Stream ID encode the Stream Type
/// and Initiator.
/// The first three bits of the last four bits encode the Stream Type,
/// while the last bit encodes the Initiator.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<M = Message> {
    /// The protocol version.
    pub version: Version,
    /// The stream identifier.
    pub stream: StreamId,
    /// The frame payload.
    pub data: FrameData<M>,
}

impl<M> Frame<M> {
    /// Create a 'git' protocol frame.
    pub fn git(stream: StreamId, data: Vec<u8>) -> Self {
        Self {
            version: PROTOCOL_VERSION_STRING,
            stream,
            data: FrameData::Git(data),
        }
    }

    /// Create a 'control' protocol frame.
    pub fn control(link: Link, ctrl: Control) -> Self {
        Self {
            version: PROTOCOL_VERSION_STRING,
            stream: StreamId::control(link),
            data: FrameData::Control(ctrl),
        }
    }

    /// Create a 'gossip' protocol frame.
    pub fn gossip(link: Link, msg: M) -> Self
    where
        M: wire::Encode,
    {
        Self {
            version: Version([b'r', b'a', b'd', msg.protocol_version()]),
            stream: StreamId::gossip(link),
            data: FrameData::Gossip(msg),
        }
    }
}

/// Frame payload.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameData<M> {
    /// Control frame payload.
    Control(Control),
    /// Gossip frame payload.
    Gossip(M),
    /// Git frame payload. May contain packet-lines as well as packfile data.
    Git(Vec<u8>),
}

/// A control message sent over a control stream.
#[derive(Debug, PartialEq, Eq)]
pub enum Control {
    /// Request repository-key V2 using the V1 control stream.
    Upgrade {
        /// Requested version.
        version: u8,
    },
    /// Acknowledge repository-key V2 on the V1 control stream.
    UpgradeAck {
        /// Acknowledged version.
        version: u8,
    },
    /// Open a new stream.
    Open {
        /// The stream to open.
        stream: StreamId,
    },
    /// Close an existing stream.
    Close {
        /// The stream to close.
        stream: StreamId,
    },
    /// Signal an end-of-file. This can be used to simulate connections terminating
    /// without having to close the connection. These control messages are turned into
    /// [`io::ErrorKind::UnexpectedEof`] errors on read.
    Eof {
        /// The stream to send an EOF on.
        stream: StreamId,
    },
}

/// Type of control message.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlType {
    /// Control open byte.
    Open = 0,
    /// Control close byte.
    Close = 1,
    /// Control EOF byte.
    Eof = 2,
    /// Repository-key upgrade request.
    Upgrade = 3,
    /// Repository-key upgrade acknowledgement.
    UpgradeAck = 4,
}

impl TryFrom<u8> for ControlType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00 => Ok(ControlType::Open),
            0b01 => Ok(ControlType::Close),
            0b10 => Ok(ControlType::Eof),
            3 => Ok(ControlType::Upgrade),
            4 => Ok(ControlType::UpgradeAck),
            n => Err(n),
        }
    }
}

impl From<ControlType> for u8 {
    fn from(value: ControlType) -> Self {
        value as u8
    }
}

impl wire::Decode for Control {
    fn decode(buf: &mut impl Buf) -> Result<Self, wire::Error> {
        match ControlType::try_from(u8::decode(buf)?) {
            Ok(ControlType::Open) => Ok(Control::Open {
                stream: StreamId::decode(buf)?,
            }),
            Ok(ControlType::Close) => Ok(Control::Close {
                stream: StreamId::decode(buf)?,
            }),
            Ok(ControlType::Eof) => Ok(Control::Eof {
                stream: StreamId::decode(buf)?,
            }),
            Ok(kind @ (ControlType::Upgrade | ControlType::UpgradeAck)) => {
                let version = u8::decode(buf)?;
                if version != 2 {
                    return Err(
                        wire::Invalid::ProtocolVersionUnsupported { actual: version }.into(),
                    );
                }
                Ok(if kind == ControlType::Upgrade {
                    Control::Upgrade { version }
                } else {
                    Control::UpgradeAck { version }
                })
            }
            Err(other) => Err(wire::Invalid::ControlType { actual: other }.into()),
        }
    }
}

impl wire::Encode for Control {
    fn encode(&self, buf: &mut impl BufMut) {
        match self {
            Self::Upgrade { version } => {
                u8::from(ControlType::Upgrade).encode(buf);
                version.encode(buf);
            }
            Self::UpgradeAck { version } => {
                u8::from(ControlType::UpgradeAck).encode(buf);
                version.encode(buf);
            }
            Self::Open { stream: id } => {
                u8::from(ControlType::Open).encode(buf);
                id.encode(buf);
            }
            Self::Eof { stream: id } => {
                u8::from(ControlType::Eof).encode(buf);
                id.encode(buf);
            }
            Self::Close { stream: id } => {
                u8::from(ControlType::Close).encode(buf);
                id.encode(buf);
            }
        }
    }
}

impl<M: wire::Decode> wire::Decode for Frame<M> {
    fn decode(buf: &mut impl Buf) -> Result<Self, wire::Error> {
        let version = Version::decode(buf)?;
        if !matches!(version.number(), 1 | 2) {
            return Err(wire::Invalid::ProtocolVersionUnsupported {
                actual: version.number(),
            }
            .into());
        }
        let stream = StreamId::decode(buf)?;

        match stream.kind() {
            Ok(StreamType::Control) => {
                let ctrl = Control::decode(buf)?;
                let frame = Frame {
                    version,
                    stream,
                    data: FrameData::Control(ctrl),
                };
                Ok(frame)
            }
            Ok(StreamType::Gossip) => {
                let data = varint::payload::decode_limit(buf, wire::Size::MAX as usize)?;
                let mut cursor = io::Cursor::new(data);
                let msg = M::decode_version(&mut cursor, version.number())?;
                let frame = Frame {
                    version,
                    stream,
                    data: FrameData::Gossip(msg),
                };

                // Nb. If there is data after the `Message` that is not decoded,
                // it is simply dropped here.

                Ok(frame)
            }
            Ok(StreamType::Git) => {
                let data = varint::payload::decode(buf)?;
                Ok(Frame {
                    version,
                    stream,
                    data: FrameData::Git(data),
                })
            }
            Err(n) => Err(wire::Invalid::StreamType { actual: n }.into()),
        }
    }
}

impl<M: wire::Encode> wire::Encode for Frame<M> {
    fn encode(&self, buf: &mut impl BufMut) {
        self.version.encode(buf);
        self.stream.encode(buf);
        match &self.data {
            FrameData::Control(ctrl) => ctrl.encode(buf),
            FrameData::Git(data) => varint::payload::encode(data, buf),
            FrameData::Gossip(msg) => varint::payload::encode(&msg.encode_to_vec(), buf),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn upgrade_control_bytes_preserve_v1_framing() {
        use wire::{Decode as _, Encode as _};
        for (ctrl, tag) in [
            (Control::Upgrade { version: 2 }, 3),
            (Control::UpgradeAck { version: 2 }, 4),
        ] {
            let frame = Frame::<Message>::control(Link::Outbound, ctrl);
            let bytes = frame.encode_to_vec();
            assert_eq!(bytes, [b'r', b'a', b'd', 1, 0, tag, 2]);
            assert_eq!(
                Frame::<Message>::decode(&mut bytes.as_slice()).unwrap(),
                frame
            );
        }
        assert!(Frame::<Message>::decode(&mut b"rad\x01\x00\x03\x03".as_slice()).is_err());
        assert!(Frame::<Message>::decode(&mut b"rad\x01\x00\x05\x02".as_slice()).is_err());
    }

    #[test]
    fn native_namespace_frames_preserve_signed_encoding_and_transport_identity() {
        use crate::bounded::BoundedVec;
        use crate::service::message::{AnnouncementMessage, RefsAnnouncement};
        use radicle::crypto::{RepositoryKey, Signer as _, SigningKey};
        use radicle::storage::refs::RefsAt;
        use radicle::test::fixtures;
        use wire::{Decode as _, Encode as _};

        let transport = SigningKey::mock(28);
        let repo =
            radicle::identity::RepoId::from("11".repeat(20).parse::<radicle::git::Oid>().unwrap());
        let at = "22".repeat(20).parse::<radicle::git::Oid>().unwrap();
        let timestamp = radicle::node::Timestamp::from(localtime::LocalTime::from_millis(1234567));
        let ed = RepositoryKey::from(transport.public_key());
        for (key, version) in [(ed, 1), (fixtures::bls_repository_key(), 2), (ed, 2)] {
            let mut ann = RefsAnnouncement::new(
                repo,
                BoundedVec::from(Some(RefsAt { remote: key, at })),
                timestamp,
            );
            ann.encoding = version;
            let signed = AnnouncementMessage::Refs(ann).signed(&transport);
            assert_eq!(signed.node, *transport.public_key());
            assert!(signed.verify());
            let frame = Frame::gossip(Link::Outbound, Message::from(signed.clone()));
            let encoded = frame.encode_to_vec();
            assert_eq!(&encoded[..4], &[b'r', b'a', b'd', version]);
            let mut body = vec![0, 6];
            body.extend_from_slice(transport.public_key().as_ref());
            body.extend_from_slice(&signed.signature.to_vec());
            body.extend_from_slice(&20u16.to_be_bytes());
            body.extend_from_slice(repo.as_ref());
            body.extend_from_slice(&1u16.to_be_bytes());
            if version == 1 {
                body.extend_from_slice(transport.public_key().as_ref());
            } else {
                body.extend_from_slice(&key.to_tagged_bytes());
            }
            body.extend_from_slice(&20u16.to_be_bytes());
            body.extend_from_slice(at.as_ref());
            timestamp.encode(&mut body);
            let mut expected = vec![b'r', b'a', b'd', version, 2];
            varint::payload::encode(&body, &mut expected);
            assert_eq!(encoded, expected);
            println!(
                "WIRE_VECTOR version={version} key={key} frame={}",
                encoded
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            let decoded = Frame::<Message>::decode(&mut encoded.as_slice()).unwrap();
            assert_eq!(decoded, frame);
            let FrameData::Gossip(Message::Announcement(relayed)) = decoded.data else {
                panic!("refs announcement expected")
            };
            assert!(relayed.verify());
            assert_eq!(
                Frame::gossip(Link::Outbound, Message::from(relayed)).encode_to_vec(),
                encoded
            );
        }
    }

    #[test]
    fn v2_is_rejected_before_payload_decode_without_negotiation() {
        use crate::deserializer::Deserializer;
        let mut inbox = Deserializer::<1024, Frame>::new(128);
        inbox.input(b"rad\x02").unwrap();
        assert!(matches!(
            inbox.deserialize_negotiated(false),
            Err(wire::Invalid::RepositoryKeyNegotiation)
        ));
        assert!(inbox.deserialize_negotiated(true).unwrap().is_none());
    }

    #[test]
    fn oversized_gossip_and_incomplete_payloads_are_checked_before_allocation() {
        use wire::{Decode as _, Encode as _};
        let mut frame = b"rad\x01".to_vec();
        StreamId::gossip(Link::Outbound).encode(&mut frame);
        varint::VarInt::new(wire::Size::MAX as u64 + 1)
            .unwrap()
            .encode(&mut frame);
        assert!(matches!(
            Frame::<Message>::decode(&mut frame.as_slice()),
            Err(wire::Error::Invalid(wire::Invalid::PayloadSize { .. }))
        ));
        let huge = varint::VarInt::MAX.encode_to_vec();
        assert!(matches!(
            varint::payload::decode(&mut huge.as_slice()),
            Err(wire::Error::UnexpectedEnd { .. })
        ));
        let git = Frame::<Message>::git(
            StreamId::git(Link::Outbound),
            vec![0; wire::Size::MAX as usize * 3],
        );
        let bytes = git.encode_to_vec();
        assert_eq!(
            Frame::<Message>::decode(&mut bytes.as_slice()).unwrap(),
            git
        );
    }

    #[test]
    fn test_stream_id() {
        assert_eq!(StreamId(VarInt(0b000)).kind().unwrap(), StreamType::Control);
        assert_eq!(StreamId(VarInt(0b010)).kind().unwrap(), StreamType::Gossip);
        assert_eq!(StreamId(VarInt(0b100)).kind().unwrap(), StreamType::Git);
        assert_eq!(StreamId(VarInt(0b001)).link(), Link::Inbound);
        assert_eq!(StreamId(VarInt(0b000)).link(), Link::Outbound);
        assert_eq!(StreamId(VarInt(0b101)).link(), Link::Inbound);
        assert_eq!(StreamId(VarInt(0b100)).link(), Link::Outbound);

        assert_eq!(StreamId::git(Link::Outbound), StreamId(VarInt(0b100)));
        assert_eq!(StreamId::control(Link::Outbound), StreamId(VarInt(0b000)));
        assert_eq!(StreamId::gossip(Link::Outbound), StreamId(VarInt(0b010)));

        assert_eq!(StreamId::git(Link::Inbound), StreamId(VarInt(0b101)));
        assert_eq!(StreamId::control(Link::Inbound), StreamId(VarInt(0b001)));
        assert_eq!(StreamId::gossip(Link::Inbound), StreamId(VarInt(0b011)));
    }

    #[test]
    fn test_encode_git_large() {
        use wire::Encode as _;

        let size = u16::MAX as usize * 3;
        assert!(
            size > (wire::Size::MAX as usize * 2),
            "we want to test sizes that are way larger than any gossip message"
        );

        let a_lot_of_data = vec![0u8; size];

        let frame: Frame<Message> = Frame::git(StreamId(0u8.into()), a_lot_of_data);

        // In previous versions since 3c5668e this would panic.
        let bytes = frame.encode_to_vec();

        assert!(
            bytes.len() > wire::Size::MAX as usize * 2,
            "just making sure that whatever was encoded is still quite large"
        );
    }
}
