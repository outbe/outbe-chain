//! Host-mediated Tribute reads initiated by the enclave during preparation.

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    nod_materialization::ProtectedNodMaterializationV2, profile::poc_schema_limits,
};
use outbe_primitives::{nod_encryption::EncryptedNodV2, wwd_entity_id::WwdEntityId};
use serde::{Deserialize, Serialize};

use crate::{
    nod_materialization::{attestation, hash, LeagueFractionInputV2, NodOperation},
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_source::{TributeSourceSubject, TributeStateSource},
    TransportError,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StateNodAuthority {
    pub source: TributeSourceSubject,
    pub head: Vec<u8>,
    pub subtree_height: u8,
    pub source_bindings: Vec<B256>,
    pub nod_gratis_load_max: U256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrepareStateNodsRequest {
    pub authority: StateNodAuthority,
    pub batch: Vec<u8>,
    pub league_fractions: Vec<LeagueFractionInputV2>,
    pub sources: Vec<TributeStateSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeSourceLookup {
    pub subject: TributeSourceSubject,
    pub tribute_ids: Vec<WwdEntityId>,
}

/// Implementations read only the checkpoint supplied by the enclave request.
pub trait TributeSourceReader {
    fn read_tribute(
        &mut self,
        subject: &TributeSourceSubject,
        id: WwdEntityId,
    ) -> Result<TributeStateSource, TransportError>;
}

pub fn prepare_state_nods(
    request: PrepareStateNodsRequest,
    source_reader: &mut impl TributeSourceReader,
) -> Result<ProtectedNodMaterializationV2, TransportError> {
    prepare_with_exchange(request, source_reader, exchange)
}

fn prepare_with_exchange(
    mut request: PrepareStateNodsRequest,
    source_reader: &mut impl TributeSourceReader,
    mut exchange: impl FnMut(EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError>,
) -> Result<ProtectedNodMaterializationV2, TransportError> {
    if !request.sources.is_empty() || !request.authority.source_bindings.is_empty() {
        return Err(TransportError::NodMaterializationRejected(
            "state NOD preparation must start with an enclave source request".into(),
        ));
    }
    let expected =
        hash(b"outbe/nod/state-prepare-inputs", &request).map_err(TransportError::codec)?;
    let (key, response) = exchange(EnclaveRequest::PrepareStateNods {
        request: Box::new(request.clone()),
    })?;
    let lookup = match response {
        EnclaveResponse::TributeSourcesRequested {
            lookup,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected && lookup.subject == request.authority.source => {
            let preimage = attestation(b"outbe/nod/state-source-request", expected, &lookup)
                .map_err(TransportError::codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            lookup
        }
        response => return NodOperation::Prepare.reject_or_unexpected(&key, expected, response),
    };
    let bound = outbe_ocomp_protocol::nod_materialization::MAX_NOD_MATERIALIZATION_ACTIONS;
    if lookup.tribute_ids.is_empty() || lookup.tribute_ids.len() > bound {
        return Err(TransportError::UnexpectedResponse);
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in lookup.tribute_ids {
        if !seen.insert(id) || id.worldwide_day() != lookup.subject.worldwide_day {
            return Err(TransportError::UnexpectedResponse);
        }
        let source = source_reader.read_tribute(&lookup.subject, id)?;
        if source.tribute.context.tribute_id != id {
            return Err(TransportError::NodMaterializationRejected(
                "host returned a different Tribute".into(),
            ));
        }
        request.authority.source_bindings.push(source.binding()?);
        request.sources.push(source);
    }
    let expected =
        hash(b"outbe/nod/state-prepare-inputs", &request).map_err(TransportError::codec)?;
    let (key, response) = exchange(EnclaveRequest::PrepareStateNods {
        request: Box::new(request),
    })?;
    match response {
        EnclaveResponse::EncryptedNodsPreparedV2 {
            carrier,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = attestation(b"outbe/nod/state-prepare-result", expected, &carrier)
                .map_err(TransportError::codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            ProtectedNodMaterializationV2::decode_canonical(&carrier, &poc_schema_limits())
                .map_err(TransportError::codec)
        }
        response => NodOperation::Prepare.reject_or_unexpected(&key, expected, response),
    }
}

/// Acceptance consumes only the prepared witness and the node's local authority.
pub fn open_state_nods(
    authority: &StateNodAuthority,
    carrier: &ProtectedNodMaterializationV2,
) -> Result<Vec<EncryptedNodV2>, TransportError> {
    let carrier = carrier
        .encode_canonical(&poc_schema_limits())
        .map_err(TransportError::codec)?;
    let expected = hash(b"outbe/nod/state-open-inputs", &(authority, &carrier))
        .map_err(TransportError::codec)?;
    let (key, response) = exchange(EnclaveRequest::OpenStateNods {
        authority: authority.clone(),
        carrier,
    })?;
    match response {
        EnclaveResponse::EncryptedNodsOpenedV2 {
            nods,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = attestation(b"outbe/nod/state-open-result", expected, &nods)
                .map_err(TransportError::codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Ok(nods)
        }
        response => NodOperation::Open.reject_or_unexpected(&key, expected, response),
    }
}

#[cfg(feature = "test-utils")]
pub mod test_support;

fn exchange(request: EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    #[cfg(feature = "test-utils")]
    if let Some(exchange) = test_support::current() {
        return exchange(request);
    }
    crate::nod_transport::request(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tribute_source::TributeSourceCheckpoint;
    use alloy_primitives::{Address, U256};
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_ocomp_protocol::common::BoundedBytes;
    use outbe_primitives::{
        addresses::TRIBUTE_ADDRESS,
        storage::proof::AccountStorageProof,
        time::WorldwideDay,
        tribute_encryption::{EncryptedTributeV2, TributeContextV2},
        wwd_entity_id::derive_poseidon_entity_id,
    };

    struct Reader {
        expected: TributeSourceSubject,
        source: TributeStateSource,
        calls: usize,
    }

    impl TributeSourceReader for Reader {
        fn read_tribute(
            &mut self,
            subject: &TributeSourceSubject,
            id: WwdEntityId,
        ) -> Result<TributeStateSource, TransportError> {
            assert_eq!(*subject, self.expected);
            assert_eq!(id, self.source.tribute.context.tribute_id);
            self.calls += 1;
            Ok(self.source.clone())
        }
    }

    fn fixture() -> (PrepareStateNodsRequest, Reader) {
        let day = WorldwideDay::new(20261015);
        let owner = Address::repeat_byte(1);
        let id = derive_poseidon_entity_id(owner, day).unwrap();
        let subject = TributeSourceSubject {
            checkpoint: TributeSourceCheckpoint {
                block_number: 42,
                block_hash: B256::repeat_byte(2),
                state_root: B256::repeat_byte(3),
            },
            chain_id: 54322345,
            worldwide_day: day,
            tribute_count: 1,
        };
        let source = TributeStateSource {
            tribute: EncryptedTributeV2 {
                context: TributeContextV2 {
                    chain_id: subject.chain_id,
                    tribute_id: id,
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(1_000_000),
                    offer_input_hash: B256::repeat_byte(4),
                },
                encrypted_creator_public_key: vec![1; 56],
                encrypted_amounts: vec![2; 88],
            },
            day_index: 0,
            expected_words: vec![],
            proof: AccountStorageProof {
                address: TRIBUTE_ADDRESS,
                nonce: 0,
                balance: U256::ZERO,
                code_hash: B256::ZERO,
                storage_root: B256::ZERO,
                account_nodes: vec![],
                slots: vec![],
            },
        };
        (
            PrepareStateNodsRequest {
                authority: StateNodAuthority {
                    source: subject,
                    head: vec![1],
                    subtree_height: 0,
                    source_bindings: vec![],
                    nod_gratis_load_max: U256::ONE,
                },
                batch: vec![2],
                league_fractions: vec![],
                sources: vec![],
            },
            Reader {
                expected: subject,
                source,
                calls: 0,
            },
        )
    }

    fn lookup_response(
        request: &PrepareStateNodsRequest,
        id: WwdEntityId,
        signer: &SigningKey,
    ) -> EnclaveResponse {
        let inputs = hash(b"outbe/nod/state-prepare-inputs", request).unwrap();
        let lookup = TributeSourceLookup {
            subject: request.authority.source,
            tribute_ids: vec![id],
        };
        let preimage = attestation(b"outbe/nod/state-source-request", inputs, &lookup).unwrap();
        EnclaveResponse::TributeSourcesRequested {
            lookup,
            inputs_canonical_hash: inputs,
            attestation_tag: signer.sign(&preimage).to_bytes().to_vec(),
        }
    }

    #[test]
    fn signed_enclave_request_drives_fixed_checkpoint_reads_before_preparation() {
        let (request, mut reader) = fixture();
        let id = reader.source.tribute.context.tribute_id;
        let signer = SigningKey::from_bytes(&[0x61; 32]);
        let mut calls = 0;
        let result = prepare_with_exchange(request, &mut reader, |message| {
            let EnclaveRequest::PrepareStateNods { request } = message else {
                panic!("prepare expected")
            };
            calls += 1;
            let response = if calls == 1 {
                assert!(request.sources.is_empty());
                lookup_response(&request, id, &signer)
            } else {
                assert_eq!(request.sources.len(), 1);
                assert_eq!(
                    request.authority.source_bindings,
                    vec![request.sources[0].binding().unwrap()]
                );
                let carrier = ProtectedNodMaterializationV2 {
                    queue_sequence: 1,
                    first_nod_ordinal: 0,
                    encryption_binding: B256::repeat_byte(7),
                    encrypted_witness: BoundedBytes(vec![1]),
                    encrypted_nods: vec![BoundedBytes(vec![2])],
                }
                .encode_canonical(&poc_schema_limits())
                .unwrap();
                let inputs = hash(b"outbe/nod/state-prepare-inputs", &request).unwrap();
                let preimage =
                    attestation(b"outbe/nod/state-prepare-result", inputs, &carrier).unwrap();
                EnclaveResponse::EncryptedNodsPreparedV2 {
                    carrier,
                    inputs_canonical_hash: inputs,
                    attestation_tag: signer.sign(&preimage).to_bytes().to_vec(),
                }
            };
            Ok((signer.verifying_key().to_bytes(), response))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(reader.calls, 1);
        assert_eq!(result.encrypted_nods.len(), 1);
    }

    #[test]
    fn wrong_attestation_or_checkpoint_never_reaches_the_host_reader() {
        for wrong_checkpoint in [false, true] {
            let (request, mut reader) = fixture();
            let id = reader.source.tribute.context.tribute_id;
            let signer = SigningKey::from_bytes(&[0x61; 32]);
            assert!(prepare_with_exchange(request, &mut reader, |message| {
                let EnclaveRequest::PrepareStateNods { request } = message else {
                    panic!("prepare expected")
                };
                let mut response = lookup_response(&request, id, &signer);
                let EnclaveResponse::TributeSourcesRequested {
                    lookup,
                    attestation_tag,
                    ..
                } = &mut response
                else {
                    unreachable!()
                };
                if wrong_checkpoint {
                    lookup.subject.checkpoint.block_hash = B256::repeat_byte(0x78);
                } else {
                    attestation_tag[0] ^= 1;
                }
                Ok((signer.verifying_key().to_bytes(), response))
            })
            .is_err());
            assert_eq!(reader.calls, 0);
        }
    }
}
