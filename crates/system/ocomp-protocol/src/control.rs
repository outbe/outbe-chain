use std::collections::BTreeSet;

use alloy_primitives::B256;

use crate::{
    common::BoundedBytes,
    error::ProtocolError,
    schema::{require, wire_enum_u8, wire_struct, NestedCodec, SchemaLimits},
    CanonicalReader, CanonicalWriter,
};

pub const CONTROL_VERSION_V1: u16 = 1;
pub const CONTROL_FRAME_HEADER_LEN: usize = 32;
pub const CONTROL_FRAME_LEN_AFTER_PREFIX: usize = 28;
pub const NODE_CONTROL_MAGIC: [u8; 4] = *b"OCL1";
pub const WORKER_CONTROL_MAGIC: [u8; 4] = *b"OWR1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlMagic {
    Node,
    Worker,
}

impl ControlMagic {
    #[must_use]
    pub const fn bytes(self) -> [u8; 4] {
        match self {
            Self::Node => NODE_CONTROL_MAGIC,
            Self::Worker => WORKER_CONTROL_MAGIC,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum NodeMessageKind {
    Hello = 0x0001,
    HelloAck = 0x0002,
    ListFinalizedJobs = 0x0010,
    GetJobSpec = 0x0011,
    OpenSnapshotLease = 0x0012,
    RenewSnapshotLease = 0x0013,
    ListSnapshotHandoffs = 0x0014,
    GetSnapshotHandoff = 0x0015,
    BuildFinalizedIntentProof = 0x0016,
    BuildLysisOpenings = 0x0017,
    CommitSnapshotExport = 0x0018,
    RequestAttestation = 0x0019,
    GetOcompHealth = 0x001a,
    Response = 0x7ffe,
    Error = 0x7fff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum WorkerMessageKind {
    Hello = 0x0001,
    HelloAck = 0x0002,
    RunUnit = 0x0010,
    UnitFinished = 0x7ffe,
    Error = 0x7fff,
}

wire_enum_u8! {
    pub enum UnitFinishedStatus {
        Success = 1,
        Failed = 2,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum LocalErrorCode {
    Malformed = 1,
    LimitExceeded = 2,
    Unauthorized = 3,
    StaleSession = 4,
    NoCommonBundle = 5,
    NotFound = 6,
    Conflict = 7,
    Busy = 8,
    SourceUnavailable = 9,
    InternalOcompUnavailable = 10,
}

wire_struct! {
    pub struct LocalErrorV1 {
        pub rejected_kind: u16,
        pub error_code: u16,
        pub retryable: bool,
    }
}

wire_enum_u8! {
    /// Fixed local process roles. This is operational transport metadata, not a
    /// consensus or fork-extensible registry.
    pub enum ControlRoleV1 {
        Node = 1,
        Supervisor = 2,
        SnapshotExporter = 3,
        Worker = 4,
        Relay = 5,
    }
}

wire_struct! {
    /// First frame of every local control session.
    pub struct HelloV1 {
        pub role: ControlRoleV1,
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub process_nonce: B256,
        pub protocol_bundle_hash: B256,
        pub capability_bits: u64,
        pub max_control_body_bytes: u32,
    }
}

wire_struct! {
    /// Server-selected parameters returned after an exact compatible hello.
    pub struct HelloAckV1 {
        pub role: ControlRoleV1,
        pub server_boot_nonce: B256,
        pub protocol_bundle_hash: B256,
        pub capability_bits: u64,
        pub max_control_body_bytes: u32,
        pub session_generation: u64,
    }
}

wire_struct! {
    pub struct CasObjectRefV1 {
        pub transport_digest: B256,
        pub encoded_bytes: u64,
        pub expected_ocb1_kind: Option<u16>,
    }
}

wire_struct! {
    pub struct RunUnitV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub plan_hash: B256,
        pub unit_index: u32,
        pub canonical_unit_spec: BoundedBytes,
        pub plan_ref: CasObjectRefV1,
        pub input_manifest_ref: CasObjectRefV1,
        pub ordered_input_refs: Vec<CasObjectRefV1>,
    }
    validate = validate_run_unit;
}

wire_struct! {
    pub struct UnitFinishedV1 {
        pub unit_id: B256,
        pub status: UnitFinishedStatus,
        pub exact_staged_bytes: u64,
        pub transport_digest: B256,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlFrameV1 {
    pub magic: ControlMagic,
    pub message_kind: u16,
    pub session_generation: u64,
    pub request_id: u64,
    pub body: Vec<u8>,
}

impl ControlFrameV1 {
    pub fn encode(&self, limits: &SchemaLimits) -> Result<Vec<u8>, ProtocolError> {
        require(
            self.body.len() <= limits.max_control_body_bytes,
            "control body cap",
        )?;
        let body_len =
            u32::try_from(self.body.len()).map_err(|_| ProtocolError::IntegerOverflow {
                what: "control body length",
            })?;
        let frame_len =
            u32::try_from(CONTROL_FRAME_LEN_AFTER_PREFIX + self.body.len()).map_err(|_| {
                ProtocolError::IntegerOverflow {
                    what: "control frame length",
                }
            })?;
        let mut output = Vec::with_capacity(CONTROL_FRAME_HEADER_LEN + self.body.len());
        output.extend_from_slice(&frame_len.to_be_bytes());
        output.extend_from_slice(&self.magic.bytes());
        output.extend_from_slice(&CONTROL_VERSION_V1.to_be_bytes());
        output.extend_from_slice(&self.message_kind.to_be_bytes());
        output.extend_from_slice(&self.session_generation.to_be_bytes());
        output.extend_from_slice(&self.request_id.to_be_bytes());
        output.extend_from_slice(&body_len.to_be_bytes());
        output.extend_from_slice(&self.body);
        Ok(output)
    }

    pub fn decode(
        encoded: &[u8],
        expected_magic: ControlMagic,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        if encoded.len() < 4 {
            return Err(ProtocolError::UnexpectedEof {
                offset: 0,
                needed: 4,
                remaining: encoded.len(),
            });
        }
        let frame_len = usize::try_from(u32::from_be_bytes(encoded[..4].try_into().map_err(
            |_| ProtocolError::UnexpectedEof {
                offset: 0,
                needed: 4,
                remaining: encoded.len(),
            },
        )?))
        .map_err(|_| ProtocolError::IntegerOverflow {
            what: "control frame length",
        })?;
        let cap = CONTROL_FRAME_LEN_AFTER_PREFIX
            .checked_add(limits.max_control_body_bytes)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "control frame cap",
            })?;
        if frame_len > cap {
            return Err(ProtocolError::CapacityExceeded {
                what: "control frame bytes",
                limit: cap,
                actual: frame_len,
            });
        }
        require(encoded.len() == frame_len + 4, "control frame exact length")?;
        let mut input = CanonicalReader::new(&encoded[4..], limits.codec)?;
        let magic = input.read_fixed::<4>()?;
        require(magic == expected_magic.bytes(), "control frame magic")?;
        require(
            input.read_u16()? == CONTROL_VERSION_V1,
            "control frame version",
        )?;
        let message_kind = input.read_u16()?;
        let session_generation = input.read_u64()?;
        let request_id = input.read_u64()?;
        let body_len =
            usize::try_from(input.read_u32()?).map_err(|_| ProtocolError::IntegerOverflow {
                what: "control body length",
            })?;
        require(
            body_len <= limits.max_control_body_bytes,
            "control body cap",
        )?;
        require(body_len == input.remaining(), "control body exact length")?;
        let body = input
            .read_fixed_dynamic(body_len, limits.max_control_body_bytes)?
            .to_vec();
        input.finish()?;
        Ok(Self {
            magic: expected_magic,
            message_kind,
            session_generation,
            request_id,
            body,
        })
    }
}

macro_rules! impl_control_body_codec {
    ($type:ty) => {
        impl $type {
            pub fn encode_body(&self, limits: &SchemaLimits) -> Result<Vec<u8>, ProtocolError> {
                let mut output = CanonicalWriter::new(limits.codec);
                self.encode_nested(&mut output, limits)?;
                Ok(output.into_bytes())
            }

            pub fn decode_body(
                encoded: &[u8],
                limits: &SchemaLimits,
            ) -> Result<Self, ProtocolError> {
                let mut input = CanonicalReader::new(encoded, limits.codec)?;
                let value = Self::decode_nested(&mut input, limits)?;
                input.finish()?;
                Ok(value)
            }
        }
    };
}

impl_control_body_codec!(HelloV1);
impl_control_body_codec!(HelloAckV1);
impl_control_body_codec!(LocalErrorV1);
impl_control_body_codec!(UnitFinishedV1);

impl RunUnitV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.ordered_input_refs.len() <= limits.max_unit_inputs,
            "worker input reference cap",
        )?;
        let mut digests = BTreeSet::new();
        require(
            digests.insert(self.plan_ref.transport_digest)
                && digests.insert(self.input_manifest_ref.transport_digest),
            "worker authority references distinct",
        )?;
        for reference in &self.ordered_input_refs {
            require(
                digests.insert(reference.transport_digest),
                "worker input references distinct",
            )?;
        }
        Ok(())
    }

    pub fn encode_body(&self, limits: &SchemaLimits) -> Result<Vec<u8>, ProtocolError> {
        self.validate_semantics(limits)?;
        let mut output = CanonicalWriter::new(limits.codec);
        self.encode_nested(&mut output, limits)?;
        Ok(output.into_bytes())
    }

    pub fn decode_body(encoded: &[u8], limits: &SchemaLimits) -> Result<Self, ProtocolError> {
        let mut input = CanonicalReader::new(encoded, limits.codec)?;
        let value = Self::decode_nested(&mut input, limits)?;
        input.finish()?;
        value.validate_semantics(limits)?;
        Ok(value)
    }
}

fn validate_run_unit(request: &RunUnitV1, limits: &SchemaLimits) -> Result<(), ProtocolError> {
    request.validate_semantics(limits)
}
