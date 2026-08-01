//! Bounded local control sessions shared by the four fixed OCOMP roles.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use crate::{
    ControlFrameV1, ControlMagic, ControlRoleV1, HelloAckV1, HelloV1, LocalErrorCode, LocalErrorV1,
    NodeMessageKind, ProtocolError, SchemaLimits, WorkerMessageKind,
    CONTROL_FRAME_LEN_AFTER_PREFIX,
};
use alloy_primitives::B256;
use thiserror::Error;

/// Fixed role understood by the local UID/method table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRole {
    Node,
    Supervisor,
    SnapshotExporter,
    Worker,
}

impl ControlRole {
    const fn wire(self) -> ControlRoleV1 {
        match self {
            Self::Node => ControlRoleV1::Node,
            Self::Supervisor => ControlRoleV1::Supervisor,
            Self::SnapshotExporter => ControlRoleV1::SnapshotExporter,
            Self::Worker => ControlRoleV1::Worker,
        }
    }

    const fn capability_bits(self, magic: ControlMagic) -> u64 {
        match (self, magic) {
            (Self::Supervisor, ControlMagic::Node) => {
                method_bit(NodeMessageKind::ListFinalizedJobs as u16)
                    | method_bit(NodeMessageKind::GetJobSpec as u16)
                    | method_bit(NodeMessageKind::OpenSnapshotLease as u16)
                    | method_bit(NodeMessageKind::RequestAttestation as u16)
                    | method_bit(NodeMessageKind::PrepareVoteTransaction as u16)
                    | method_bit(NodeMessageKind::GetOcompHealth as u16)
            }
            (Self::SnapshotExporter, ControlMagic::Node) => {
                method_bit(NodeMessageKind::RenewSnapshotLease as u16)
                    | method_bit(NodeMessageKind::ListSnapshotHandoffs as u16)
                    | method_bit(NodeMessageKind::GetSnapshotHandoff as u16)
                    | method_bit(NodeMessageKind::BuildFinalizedIntentProof as u16)
                    | method_bit(NodeMessageKind::BuildLysisOpenings as u16)
                    | method_bit(NodeMessageKind::CheckProjectionContainment as u16)
                    | method_bit(NodeMessageKind::CommitSnapshotExport as u16)
                    | method_bit(NodeMessageKind::GetOcompHealth as u16)
            }
            (Self::Supervisor, ControlMagic::Worker) => {
                method_bit(WorkerMessageKind::RunUnit as u16)
            }
            _ => 0,
        }
    }
}

const fn method_bit(kind: u16) -> u64 {
    match kind {
        0x0010..=0x001f => 1_u64 << (kind - 0x0010),
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointIdentity {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub boot_nonce: B256,
    pub protocol_bundle_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NegotiatedSession {
    pub session_generation: u64,
    pub max_control_body_bytes: usize,
    pub protocol_bundle_hash: B256,
}

#[derive(Clone, Copy, Debug)]
pub struct ServerPolicy {
    magic: ControlMagic,
    local_role: ControlRole,
    peer_role: ControlRole,
    expected_peer_uid: u32,
    identity: EndpointIdentity,
    session_generation: u64,
    limits: SchemaLimits,
}

impl ServerPolicy {
    #[must_use]
    pub const fn node(
        peer_role: ControlRole,
        expected_peer_uid: u32,
        identity: EndpointIdentity,
        session_generation: u64,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            magic: ControlMagic::Node,
            local_role: ControlRole::Node,
            peer_role,
            expected_peer_uid,
            identity,
            session_generation,
            limits,
        }
    }

    #[must_use]
    pub const fn worker(
        expected_supervisor_uid: u32,
        identity: EndpointIdentity,
        session_generation: u64,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            magic: ControlMagic::Worker,
            local_role: ControlRole::Worker,
            peer_role: ControlRole::Supervisor,
            expected_peer_uid: expected_supervisor_uid,
            identity,
            session_generation,
            limits,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClientPolicy {
    magic: ControlMagic,
    local_role: ControlRole,
    expected_server_role: ControlRole,
    expected_server_uid: u32,
    identity: EndpointIdentity,
    limits: SchemaLimits,
}

impl ClientPolicy {
    #[must_use]
    pub const fn supervisor_to_node(
        expected_server_uid: u32,
        identity: EndpointIdentity,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            magic: ControlMagic::Node,
            local_role: ControlRole::Supervisor,
            expected_server_role: ControlRole::Node,
            expected_server_uid,
            identity,
            limits,
        }
    }

    #[must_use]
    pub const fn exporter_to_node(
        expected_server_uid: u32,
        identity: EndpointIdentity,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            magic: ControlMagic::Node,
            local_role: ControlRole::SnapshotExporter,
            expected_server_role: ControlRole::Node,
            expected_server_uid,
            identity,
            limits,
        }
    }

    #[must_use]
    pub const fn supervisor_to_worker(
        expected_server_uid: u32,
        identity: EndpointIdentity,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            magic: ControlMagic::Worker,
            local_role: ControlRole::Supervisor,
            expected_server_role: ControlRole::Worker,
            expected_server_uid,
            identity,
            limits,
        }
    }
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("local control I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("local control frame failed canonical validation: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("local control peer uid {actual} is not the fixed expected uid {expected}")]
    UnauthorizedPeer { expected: u32, actual: u32 },
    #[error("local control peer claimed role {actual:?}, expected {expected:?}")]
    RoleMismatch {
        expected: ControlRoleV1,
        actual: ControlRoleV1,
    },
    #[error("local control chain/genesis identity does not match")]
    ChainIdentityMismatch,
    #[error("local control peer has no common protocol bundle")]
    NoCommonBundle,
    #[error("local control peer claimed capabilities outside its fixed role")]
    CapabilityEscalation,
    #[error("local control frame length {actual} exceeds cap {limit}")]
    FrameTooLarge { limit: usize, actual: usize },
    #[error("local control session generation {actual} is stale; expected {expected}")]
    StaleSession { expected: u64, actual: u64 },
    #[error("local control request id {actual} is invalid; expected {expected}")]
    UnexpectedRequestId { expected: u64, actual: u64 },
    #[error("local control role {role:?} cannot call message kind {kind:#06x}")]
    UnauthorizedMethod { role: ControlRole, kind: u16 },
    #[error("local control expected message kind {expected:#06x}, received {actual:#06x}")]
    UnexpectedMessage { expected: u16, actual: u16 },
    #[error("local control peer rejected kind {rejected_kind:#06x} with code {error_code}")]
    RemoteRejected { rejected_kind: u16, error_code: u16 },
    #[error("local control peer closed the session")]
    ConnectionClosed,
    #[error("local control endpoint configured zero session generation")]
    ZeroSessionGeneration,
    #[error("local control limit does not fit its wire field")]
    LimitOverflow,
    #[error("cannot determine local control process credentials")]
    EffectiveUidUnavailable,
    #[error("local service user {0} does not exist in /etc/passwd")]
    UnknownUser(String),
    #[error("process effective uid {actual} does not match service user {user} uid {expected}")]
    EffectiveUserMismatch {
        user: String,
        expected: u32,
        actual: u32,
    },
}

#[derive(Debug)]
pub struct ControlServerSession {
    stream: UnixStream,
    policy: ServerPolicy,
    peer_credentials: PeerCredentials,
    handshaken: bool,
    next_request_id: u64,
}

impl ControlServerSession {
    pub fn accept(stream: UnixStream, policy: ServerPolicy) -> Result<Self, ControlError> {
        if policy.session_generation == 0 {
            return Err(ControlError::ZeroSessionGeneration);
        }
        let peer_credentials = peer_credentials(&stream)?;
        if peer_credentials.uid != policy.expected_peer_uid {
            return Err(ControlError::UnauthorizedPeer {
                expected: policy.expected_peer_uid,
                actual: peer_credentials.uid,
            });
        }
        Ok(Self {
            stream,
            policy,
            peer_credentials,
            handshaken: false,
            next_request_id: 1,
        })
    }

    #[must_use]
    pub const fn peer_credentials(&self) -> PeerCredentials {
        self.peer_credentials
    }

    pub fn handshake(&mut self) -> Result<NegotiatedSession, ControlError> {
        let frame = self.receive_raw_frame()?;
        let expected = hello_kind(self.policy.magic);
        if frame.message_kind != expected || frame.session_generation != 0 || frame.request_id != 0
        {
            return Err(ControlError::UnexpectedMessage {
                expected,
                actual: frame.message_kind,
            });
        }
        let hello = HelloV1::decode_body(&frame.body, &self.policy.limits)?;
        if hello.role != self.policy.peer_role.wire() {
            self.send_rejection(frame.message_kind, LocalErrorCode::Unauthorized, false)?;
            return Err(ControlError::RoleMismatch {
                expected: self.policy.peer_role.wire(),
                actual: hello.role,
            });
        }
        if hello.chain_id != self.policy.identity.chain_id
            || hello.genesis_hash != self.policy.identity.genesis_hash
        {
            self.send_rejection(frame.message_kind, LocalErrorCode::Malformed, false)?;
            return Err(ControlError::ChainIdentityMismatch);
        }
        if hello.protocol_bundle_hash != self.policy.identity.protocol_bundle_hash {
            self.send_rejection(frame.message_kind, LocalErrorCode::NoCommonBundle, false)?;
            return Err(ControlError::NoCommonBundle);
        }
        let fixed_capabilities = self.policy.peer_role.capability_bits(self.policy.magic);
        if hello.capability_bits != fixed_capabilities {
            self.send_rejection(frame.message_kind, LocalErrorCode::Unauthorized, false)?;
            return Err(ControlError::CapabilityEscalation);
        }
        let peer_limit = usize::try_from(hello.max_control_body_bytes)
            .map_err(|_| ControlError::LimitOverflow)?;
        let negotiated_limit = peer_limit.min(self.policy.limits.max_control_body_bytes);
        if negotiated_limit == 0 {
            self.send_rejection(frame.message_kind, LocalErrorCode::LimitExceeded, false)?;
            return Err(ControlError::FrameTooLarge {
                limit: self.policy.limits.max_control_body_bytes,
                actual: 0,
            });
        }
        let ack = HelloAckV1 {
            role: self.policy.local_role.wire(),
            server_boot_nonce: self.policy.identity.boot_nonce,
            protocol_bundle_hash: self.policy.identity.protocol_bundle_hash,
            capability_bits: fixed_capabilities,
            max_control_body_bytes: u32::try_from(negotiated_limit)
                .map_err(|_| ControlError::LimitOverflow)?,
            session_generation: self.policy.session_generation,
        };
        self.send_frame(ControlFrameV1 {
            magic: self.policy.magic,
            message_kind: hello_ack_kind(self.policy.magic),
            session_generation: 0,
            request_id: 0,
            body: ack.encode_body(&self.policy.limits)?,
        })?;
        self.handshaken = true;
        Ok(NegotiatedSession {
            session_generation: self.policy.session_generation,
            max_control_body_bytes: negotiated_limit,
            protocol_bundle_hash: self.policy.identity.protocol_bundle_hash,
        })
    }

    pub fn receive_request(&mut self) -> Result<ControlFrameV1, ControlError> {
        let frame = self.receive_raw_frame()?;
        if !self.handshaken || frame.session_generation != self.policy.session_generation {
            return Err(ControlError::StaleSession {
                expected: self.policy.session_generation,
                actual: frame.session_generation,
            });
        }
        if frame.request_id != self.next_request_id {
            return Err(ControlError::UnexpectedRequestId {
                expected: self.next_request_id,
                actual: frame.request_id,
            });
        }
        if !method_allowed(self.policy.magic, self.policy.peer_role, frame.message_kind) {
            return Err(ControlError::UnauthorizedMethod {
                role: self.policy.peer_role,
                kind: frame.message_kind,
            });
        }
        self.next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or(ControlError::UnexpectedRequestId {
                    expected: u64::MAX,
                    actual: frame.request_id,
                })?;
        Ok(frame)
    }

    pub fn receive_raw_frame(&mut self) -> Result<ControlFrameV1, ControlError> {
        read_frame(&mut self.stream, self.policy.magic, &self.policy.limits)
    }

    pub fn send_response(
        &mut self,
        request_id: u64,
        message_kind: u16,
        body: Vec<u8>,
    ) -> Result<(), ControlError> {
        if !self.handshaken || request_id == 0 || request_id >= self.next_request_id {
            return Err(ControlError::UnexpectedRequestId {
                expected: self.next_request_id.saturating_sub(1),
                actual: request_id,
            });
        }
        self.send_frame(ControlFrameV1 {
            magic: self.policy.magic,
            message_kind,
            session_generation: self.policy.session_generation,
            request_id,
            body,
        })
    }

    fn send_frame(&mut self, frame: ControlFrameV1) -> Result<(), ControlError> {
        write_frame(&mut self.stream, &frame, &self.policy.limits)
    }

    fn send_rejection(
        &mut self,
        rejected_kind: u16,
        code: LocalErrorCode,
        retryable: bool,
    ) -> Result<(), ControlError> {
        let body = LocalErrorV1 {
            rejected_kind,
            error_code: code as u16,
            retryable,
        }
        .encode_body(&self.policy.limits)?;
        self.send_frame(ControlFrameV1 {
            magic: self.policy.magic,
            message_kind: error_kind(self.policy.magic),
            session_generation: 0,
            request_id: 0,
            body,
        })
    }
}

#[derive(Debug)]
pub struct ControlClientSession {
    stream: UnixStream,
    policy: ClientPolicy,
    session_generation: Option<u64>,
    next_request_id: u64,
    outstanding_request: Option<u64>,
}

impl ControlClientSession {
    pub fn connect(stream: UnixStream, policy: ClientPolicy) -> Result<Self, ControlError> {
        let credentials = peer_credentials(&stream)?;
        if credentials.uid != policy.expected_server_uid {
            return Err(ControlError::UnauthorizedPeer {
                expected: policy.expected_server_uid,
                actual: credentials.uid,
            });
        }
        Ok(Self {
            stream,
            policy,
            session_generation: None,
            next_request_id: 1,
            outstanding_request: None,
        })
    }

    pub fn handshake(&mut self) -> Result<NegotiatedSession, ControlError> {
        let max_control_body_bytes = u32::try_from(self.policy.limits.max_control_body_bytes)
            .map_err(|_| ControlError::LimitOverflow)?;
        let hello = HelloV1 {
            role: self.policy.local_role.wire(),
            chain_id: self.policy.identity.chain_id,
            genesis_hash: self.policy.identity.genesis_hash,
            process_nonce: self.policy.identity.boot_nonce,
            protocol_bundle_hash: self.policy.identity.protocol_bundle_hash,
            capability_bits: self.policy.local_role.capability_bits(self.policy.magic),
            max_control_body_bytes,
        };
        self.send_frame(ControlFrameV1 {
            magic: self.policy.magic,
            message_kind: hello_kind(self.policy.magic),
            session_generation: 0,
            request_id: 0,
            body: hello.encode_body(&self.policy.limits)?,
        })?;
        let response = self.receive_raw_frame()?;
        if response.message_kind == error_kind(self.policy.magic) {
            let error = LocalErrorV1::decode_body(&response.body, &self.policy.limits)?;
            return Err(ControlError::RemoteRejected {
                rejected_kind: error.rejected_kind,
                error_code: error.error_code,
            });
        }
        let expected = hello_ack_kind(self.policy.magic);
        if response.message_kind != expected
            || response.session_generation != 0
            || response.request_id != 0
        {
            return Err(ControlError::UnexpectedMessage {
                expected,
                actual: response.message_kind,
            });
        }
        let ack = HelloAckV1::decode_body(&response.body, &self.policy.limits)?;
        if ack.role != self.policy.expected_server_role.wire() {
            return Err(ControlError::RoleMismatch {
                expected: self.policy.expected_server_role.wire(),
                actual: ack.role,
            });
        }
        if ack.protocol_bundle_hash != self.policy.identity.protocol_bundle_hash {
            return Err(ControlError::NoCommonBundle);
        }
        if ack.capability_bits != self.policy.local_role.capability_bits(self.policy.magic) {
            return Err(ControlError::CapabilityEscalation);
        }
        if ack.session_generation == 0 {
            return Err(ControlError::ZeroSessionGeneration);
        }
        let negotiated_limit =
            usize::try_from(ack.max_control_body_bytes).map_err(|_| ControlError::LimitOverflow)?;
        if negotiated_limit == 0 || negotiated_limit > self.policy.limits.max_control_body_bytes {
            return Err(ControlError::FrameTooLarge {
                limit: self.policy.limits.max_control_body_bytes,
                actual: negotiated_limit,
            });
        }
        self.session_generation = Some(ack.session_generation);
        Ok(NegotiatedSession {
            session_generation: ack.session_generation,
            max_control_body_bytes: negotiated_limit,
            protocol_bundle_hash: ack.protocol_bundle_hash,
        })
    }

    pub fn send_request(&mut self, message_kind: u16, body: Vec<u8>) -> Result<(), ControlError> {
        if let Some(request_id) = self.outstanding_request {
            return Err(ControlError::UnexpectedRequestId {
                expected: request_id,
                actual: request_id,
            });
        }
        let request_id = self.next_request_id;
        self.send_request_with_id(message_kind, request_id, body)?;
        self.outstanding_request = Some(request_id);
        self.next_request_id =
            self.next_request_id
                .checked_add(1)
                .ok_or(ControlError::UnexpectedRequestId {
                    expected: u64::MAX,
                    actual: request_id,
                })?;
        Ok(())
    }

    pub fn send_request_with_id(
        &mut self,
        message_kind: u16,
        request_id: u64,
        body: Vec<u8>,
    ) -> Result<(), ControlError> {
        let session_generation = self.session_generation.ok_or(ControlError::StaleSession {
            expected: 1,
            actual: 0,
        })?;
        self.send_frame(ControlFrameV1 {
            magic: self.policy.magic,
            message_kind,
            session_generation,
            request_id,
            body,
        })
    }

    pub fn receive_response(&mut self) -> Result<ControlFrameV1, ControlError> {
        let expected_request_id =
            self.outstanding_request
                .ok_or(ControlError::UnexpectedRequestId {
                    expected: self.next_request_id,
                    actual: 0,
                })?;
        let response = self.receive_raw_frame()?;
        let session_generation = self.session_generation.ok_or(ControlError::StaleSession {
            expected: 1,
            actual: 0,
        })?;
        if response.session_generation != session_generation {
            return Err(ControlError::StaleSession {
                expected: session_generation,
                actual: response.session_generation,
            });
        }
        if response.request_id != expected_request_id {
            return Err(ControlError::UnexpectedRequestId {
                expected: expected_request_id,
                actual: response.request_id,
            });
        }
        self.outstanding_request = None;
        Ok(response)
    }

    fn send_frame(&mut self, frame: ControlFrameV1) -> Result<(), ControlError> {
        write_frame(&mut self.stream, &frame, &self.policy.limits)
    }

    fn receive_raw_frame(&mut self) -> Result<ControlFrameV1, ControlError> {
        read_frame(&mut self.stream, self.policy.magic, &self.policy.limits)
    }
}

fn read_frame(
    stream: &mut UnixStream,
    magic: ControlMagic,
    limits: &SchemaLimits,
) -> Result<ControlFrameV1, ControlError> {
    let mut prefix = [0_u8; 4];
    if let Err(error) = stream.read_exact(&mut prefix) {
        return if error.kind() == ErrorKind::UnexpectedEof {
            Err(ControlError::ConnectionClosed)
        } else {
            Err(ControlError::Io(error))
        };
    }
    let frame_len =
        usize::try_from(u32::from_be_bytes(prefix)).map_err(|_| ControlError::LimitOverflow)?;
    let cap = CONTROL_FRAME_LEN_AFTER_PREFIX
        .checked_add(limits.max_control_body_bytes)
        .ok_or(ControlError::LimitOverflow)?;
    if frame_len > cap {
        return Err(ControlError::FrameTooLarge {
            limit: cap,
            actual: frame_len,
        });
    }
    let total = frame_len
        .checked_add(prefix.len())
        .ok_or(ControlError::LimitOverflow)?;
    let mut encoded = Vec::with_capacity(total);
    encoded.extend_from_slice(&prefix);
    encoded.resize(total, 0);
    stream.read_exact(&mut encoded[4..])?;
    ControlFrameV1::decode(&encoded, magic, limits).map_err(ControlError::from)
}

fn write_frame(
    stream: &mut UnixStream,
    frame: &ControlFrameV1,
    limits: &SchemaLimits,
) -> Result<(), ControlError> {
    let encoded = frame.encode(limits)?;
    stream.write_all(&encoded)?;
    stream.flush()?;
    Ok(())
}

fn method_allowed(magic: ControlMagic, role: ControlRole, kind: u16) -> bool {
    role.capability_bits(magic) & method_bit(kind) != 0
}

const fn hello_kind(magic: ControlMagic) -> u16 {
    match magic {
        ControlMagic::Node => NodeMessageKind::Hello as u16,
        ControlMagic::Worker => WorkerMessageKind::Hello as u16,
    }
}

const fn hello_ack_kind(magic: ControlMagic) -> u16 {
    match magic {
        ControlMagic::Node => NodeMessageKind::HelloAck as u16,
        ControlMagic::Worker => WorkerMessageKind::HelloAck as u16,
    }
}

const fn error_kind(magic: ControlMagic) -> u16 {
    match magic {
        ControlMagic::Node => NodeMessageKind::Error as u16,
        ControlMagic::Worker => WorkerMessageKind::Error as u16,
    }
}

/// Compile ceilings generated for the PoC. They do not arm a fork or select a
/// bundle; callers must still supply one exact [`EndpointIdentity`].
#[must_use]
pub fn poc_schema_limits() -> SchemaLimits {
    crate::profile::poc_schema_limits()
}

#[allow(unsafe_code)]
pub fn effective_uid() -> Result<u32, ControlError> {
    // SAFETY: `geteuid` has no preconditions and only reads the credentials of
    // the current process. Unlike `/proc/self/status`, it is available on both
    // Unix targets supported by local peer credentials (Linux and macOS).
    let uid = unsafe { libc::geteuid() };
    u32::try_from(uid).map_err(|_| ControlError::EffectiveUidUnavailable)
}

pub fn uid_for_user(user: &str) -> Result<u32, ControlError> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .filter_map(parse_passwd_entry)
        .find_map(|(name, uid)| (name == user).then_some(uid))
        .ok_or_else(|| ControlError::UnknownUser(user.to_owned()))
}

pub fn effective_user_name() -> Result<String, ControlError> {
    let uid = effective_uid()?;
    let passwd = fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .filter_map(parse_passwd_entry)
        .find_map(|(name, candidate)| (candidate == uid).then_some(name.to_owned()))
        .ok_or_else(|| ControlError::UnknownUser(format!("uid:{uid}")))
}

pub fn require_effective_user(user: &str) -> Result<u32, ControlError> {
    let expected = uid_for_user(user)?;
    require_effective_uid_for(expected, user.to_owned())
}

pub fn require_effective_uid(expected: u32) -> Result<u32, ControlError> {
    require_effective_uid_for(expected, format!("uid:{expected}"))
}

fn require_effective_uid_for(expected: u32, user: String) -> Result<u32, ControlError> {
    let actual = effective_uid()?;
    if expected != actual {
        return Err(ControlError::EffectiveUserMismatch {
            user,
            expected,
            actual,
        });
    }
    Ok(actual)
}

fn parse_passwd_entry(line: &str) -> Option<(&str, u32)> {
    let mut fields = line.split(':');
    let name = fields.next()?;
    fields.next()?;
    let uid = fields.next()?.parse().ok()?;
    Some((name, uid))
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, ControlError> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `length` are valid writable objects for the
    // exact sizes supplied, and `stream.as_raw_fd()` remains owned and open for
    // the duration of this non-owning `getsockopt` call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(ControlError::Io(std::io::Error::last_os_error()));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>()) {
        return Err(ControlError::EffectiveUidUnavailable);
    }
    Ok(PeerCredentials {
        pid: u32::try_from(credentials.pid).unwrap_or_default(),
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, ControlError> {
    let file_descriptor = stream.as_raw_fd();
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: `uid` and `gid` are valid writable objects and `file_descriptor`
    // remains owned and open for the duration of this non-owning call.
    let peer_id_result = unsafe { libc::getpeereid(file_descriptor, &raw mut uid, &raw mut gid) };
    if peer_id_result != 0 {
        return Err(ControlError::Io(std::io::Error::last_os_error()));
    }

    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: `pid` and `length` are valid writable objects for the exact
    // sizes supplied, and `file_descriptor` remains owned and open for this
    // non-owning `getsockopt` call.
    let peer_pid_result = unsafe {
        libc::getsockopt(
            file_descriptor,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if peer_pid_result != 0 {
        return Err(ControlError::Io(std::io::Error::last_os_error()));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::pid_t>()) {
        return Err(ControlError::EffectiveUidUnavailable);
    }

    Ok(PeerCredentials {
        pid: u32::try_from(pid).map_err(|_| ControlError::EffectiveUidUnavailable)?,
        uid,
        gid,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_credentials(_stream: &UnixStream) -> Result<PeerCredentials, ControlError> {
    Err(ControlError::Io(std::io::Error::new(
        ErrorKind::Unsupported,
        "local peer credentials are supported only on Linux and macOS",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(unsafe_code)]
    fn effective_uid_reports_the_current_process_identity() {
        // SAFETY: `geteuid` has no preconditions and only reads current process
        // credentials.
        let expected = unsafe { libc::geteuid() };
        assert_eq!(effective_uid().unwrap(), expected);
    }

    #[test]
    #[allow(unsafe_code)]
    fn peer_credentials_report_the_connected_process_identity() {
        let (left, right) = UnixStream::pair().expect("create connected local-control sockets");

        let left_peer = peer_credentials(&left).expect("read left peer credentials");
        let right_peer = peer_credentials(&right).expect("read right peer credentials");
        // SAFETY: these libc calls have no preconditions and only read the
        // current process credentials.
        let (effective_uid, effective_gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let expected = PeerCredentials {
            pid: std::process::id(),
            uid: effective_uid,
            gid: effective_gid,
        };

        assert_eq!(left_peer, expected);
        assert_eq!(right_peer, expected);
    }
}
