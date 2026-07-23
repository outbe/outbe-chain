use alloy_primitives::B256;

use crate::{
    committee::{verify_low_s_prehash, OcompCommitteeSnapshotV1, POC_COMMITTEE_THRESHOLD},
    error::ProtocolError,
    schema::{impl_top_level_codec, require, wire_struct, SchemaLimits},
};

wire_struct! {
    pub struct OrderedSignatureV1 {
        pub validator_index: u8,
        pub signature_rs: [u8; 64],
    }
}

wire_struct! {
    pub struct ExecutionCertificateV1 {
        pub result_committee_snapshot_hash: B256,
        pub signer_bitmap: u8,
        pub ordered_signatures: Vec<OrderedSignatureV1>,
        pub result_digest: B256,
    }
    validate = validate_certificate;
}
impl_top_level_codec!(ExecutionCertificateV1, ExecutionCertificateV1);

impl ExecutionCertificateV1 {
    pub fn validate_shape(&self) -> Result<(), ProtocolError> {
        require(
            self.signer_bitmap & 0xf0 == 0,
            "certificate bitmap high bits",
        )?;
        require(
            self.signer_bitmap.count_ones() == u32::from(POC_COMMITTEE_THRESHOLD),
            "certificate threshold bitmap",
        )?;
        require(
            self.ordered_signatures.len() == usize::from(POC_COMMITTEE_THRESHOLD),
            "certificate signature count",
        )?;
        let mut previous = None;
        for signature in &self.ordered_signatures {
            require(signature.validator_index < 4, "certificate signer index")?;
            require(
                self.signer_bitmap & (1 << signature.validator_index) != 0,
                "certificate signer bitmap binding",
            )?;
            if let Some(last) = previous {
                require(last < signature.validator_index, "certificate signer order")?;
            }
            previous = Some(signature.validator_index);
        }
        Ok(())
    }

    pub fn verify(
        &self,
        snapshot: &OcompCommitteeSnapshotV1,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        self.validate_shape()?;
        require(
            self.result_committee_snapshot_hash == snapshot.snapshot_hash(limits)?,
            "certificate committee binding",
        )?;
        for ordered in &self.ordered_signatures {
            let member = &snapshot.ordered_members[usize::from(ordered.validator_index)];
            require(
                member.valid_from_height <= current_height
                    && current_height < member.valid_until_height_exclusive,
                "certificate key height validity",
            )?;
            verify_low_s_prehash(
                &member.ocomp_public_key_sec1,
                self.result_digest,
                &ordered.signature_rs,
            )?;
        }
        Ok(())
    }
}

fn validate_certificate(
    certificate: &ExecutionCertificateV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    certificate.validate_shape()
}
