//! Real cryptographic fixtures for state-backed encrypted Nod materialization.

use alloy_primitives::{keccak256, B256, U256};
use outbe_ocomp_protocol::{
    nod_materialization::{NodMaterializationBatchV1, ProtectedNodMaterializationV2},
    profile::poc_schema_limits,
    result::NodActionV1,
    test_utils::{account_mpt_with_proofs, storage_trie},
};
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    storage::{
        hashmap::HashMapStorageProvider,
        proof::{AccountStorageProof, StorageSlotProof},
        StorageHandle,
    },
    time::WorldwideDay,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2, TributeContextV2},
    wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId},
};
use outbe_tee::{
    nod_materialization::{attestation, hash, LeagueFractionInputV2},
    nod_state_materialization::{PrepareStateNodsRequest, StateNodAuthority, TributeSourceReader},
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_source::{TributeSourceCheckpoint, TributeSourceSubject, TributeStateSource},
    TransportError,
};
use outbe_tribute::{
    state_proof::TributeRecordStateProof, storage_records::StoredTribute, TributeContract,
};
use std::collections::{BTreeMap, BTreeSet};

pub const NETWORK_SECRET: [u8; 32] = [0x5a; 32];

pub fn enclave_scope() -> outbe_tee::nod_state_materialization::test_support::Guard {
    outbe_tee::nod_state_materialization::test_support::scope(exchange)
}

fn exchange(request: EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let keys = outbe_tee_enclave::keys::EnclaveKeys::new([0x31; 32], Some([0x32; 32]))
        .map_err(rejected)?;
    let response = match request {
        EnclaveRequest::PrepareStateNods { request } => {
            let inputs =
                hash(b"outbe/nod/state-prepare-inputs", &request).map_err(TransportError::codec)?;
            if request.sources.is_empty() {
                let lookup =
                    outbe_tee_enclave::nod_state_materialization::request_sources(&request)
                        .map_err(rejected)?;
                let preimage = attestation(b"outbe/nod/state-source-request", inputs, &lookup)
                    .map_err(TransportError::codec)?;
                EnclaveResponse::TributeSourcesRequested {
                    lookup,
                    inputs_canonical_hash: inputs,
                    attestation_tag: keys.sign_attestation(&preimage).to_vec(),
                }
            } else {
                let carrier = outbe_tee_enclave::nod_state_materialization::prepare(
                    &NETWORK_SECRET,
                    &request,
                )
                .map_err(rejected)?
                .encode_canonical(&poc_schema_limits())
                .map_err(|error| match error {
                    outbe_ocomp_protocol::error::ProtocolError::CapacityExceeded { .. } => {
                        TransportError::NodMaterializationCapacityExceeded(error.to_string())
                    }
                    error => TransportError::codec(error),
                })?;
                let preimage = attestation(b"outbe/nod/state-prepare-result", inputs, &carrier)
                    .map_err(TransportError::codec)?;
                EnclaveResponse::EncryptedNodsPreparedV2 {
                    carrier,
                    inputs_canonical_hash: inputs,
                    attestation_tag: keys.sign_attestation(&preimage).to_vec(),
                }
            }
        }
        EnclaveRequest::OpenStateNods { authority, carrier } => {
            let inputs = hash(b"outbe/nod/state-open-inputs", &(&authority, &carrier))
                .map_err(TransportError::codec)?;
            let decoded =
                ProtectedNodMaterializationV2::decode_canonical(&carrier, &poc_schema_limits())
                    .map_err(TransportError::codec)?;
            let nods = outbe_tee_enclave::nod_state_materialization::open(
                &NETWORK_SECRET,
                &authority,
                &decoded,
            )
            .map_err(rejected)?;
            let preimage = attestation(b"outbe/nod/state-open-result", inputs, &nods)
                .map_err(TransportError::codec)?;
            EnclaveResponse::EncryptedNodsOpenedV2 {
                nods,
                inputs_canonical_hash: inputs,
                attestation_tag: keys.sign_attestation(&preimage).to_vec(),
            }
        }
        _ => return Err(TransportError::UnexpectedResponse),
    };
    Ok((keys.attestation_pub(), response))
}

fn rejected(error: impl std::fmt::Display) -> TransportError {
    TransportError::NodMaterializationRejected(error.to_string())
}

pub struct MaterializationFixtureBuilder<'a> {
    actions: &'a [NodActionV1],
    chain_id: u64,
}
impl<'a> MaterializationFixtureBuilder<'a> {
    pub fn new(actions: &'a [NodActionV1], chain_id: u64) -> Self {
        Self { actions, chain_id }
    }
    pub fn build(&self) -> Result<MaterializationFixture, String> {
        let day = WorldwideDay::new(self.actions.first().ok_or("empty fixture")?.wwd);
        let mut subject = TributeSourceSubject {
            checkpoint: TributeSourceCheckpoint {
                block_number: 90,
                block_hash: B256::repeat_byte(0x31),
                state_root: B256::repeat_byte(0x72),
            },
            chain_id: self.chain_id,
            worldwide_day: day,
            tribute_count: self
                .actions
                .len()
                .try_into()
                .map_err(|_| "fixture count overflow")?,
        };
        let bodies = self
            .actions
            .iter()
            .map(|action| source_body(action, self.chain_id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut provider = HashMapStorageProvider::new(self.chain_id);
        let selected = StorageHandle::enter(&mut provider, |storage| -> Result<_, String> {
            let contract = TributeContract::new(storage);
            for body in &bodies {
                contract
                    .insert_state_record(&StoredTribute::from(body))
                    .map_err(|e| e.to_string())?;
            }
            bodies
                .iter()
                .enumerate()
                .map(|(index, body)| {
                    outbe_tribute::state_proof::expected_record_words(
                        &contract,
                        &subject.into(),
                        index as u32,
                        body,
                    )
                    .map(|words| words.into_iter().map(|(slot, _)| slot).collect::<Vec<_>>())
                    .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        let words = provider
            .storage
            .iter()
            .filter(|((address, _), _)| *address == TRIBUTE_ADDRESS)
            .map(|((_, slot), value)| (*slot, *value))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>();
        let (storage_root, nodes) = storage_trie(&words);
        let account = alloy_consensus::TrieAccount {
            nonce: 0,
            balance: U256::ZERO,
            storage_root,
            code_hash: keccak256([]),
        };
        let (root, accounts) = account_mpt_with_proofs(&[(TRIBUTE_ADDRESS, account)]);
        subject.checkpoint.state_root = root;
        let slots = words
            .into_iter()
            .zip(nodes)
            .map(|((slot, value), nodes)| {
                let slot = B256::from(slot.to_be_bytes::<32>());
                (slot, StorageSlotProof { slot, value, nodes })
            })
            .collect::<BTreeMap<_, _>>();
        let proof = |selected: &[B256]| AccountStorageProof {
            address: TRIBUTE_ADDRESS,
            nonce: account.nonce,
            balance: account.balance,
            storage_root,
            code_hash: account.code_hash,
            account_nodes: accounts[&TRIBUTE_ADDRESS].clone(),
            slots: selected.iter().map(|slot| slots[slot].clone()).collect(),
        };
        let length_slot = StorageHandle::enter(&mut provider, |storage| {
            TributeContract::new(storage).day_ids.list(&day).base_slot()
        });
        let day_proof = proof(&[B256::from(length_slot.to_be_bytes::<32>())]);
        let sources = bodies
            .into_iter()
            .zip(selected)
            .enumerate()
            .map(|(index, (tribute, slots))| {
                (
                    tribute.context.tribute_id,
                    TributeRecordStateProof {
                        day_index: index as u32,
                        tribute,
                        account: proof(&slots),
                    },
                )
            })
            .collect();
        Ok(MaterializationFixture {
            subject,
            day_proof,
            sources,
        })
    }
}

pub struct MaterializationFixture {
    pub subject: TributeSourceSubject,
    pub day_proof: AccountStorageProof,
    pub sources: BTreeMap<WwdEntityId, TributeRecordStateProof>,
}
impl MaterializationFixture {
    pub fn canonical_source_bodies(&self) -> Result<Vec<Vec<u8>>, String> {
        self.sources
            .values()
            .map(|source| {
                outbe_primitives::entity_codec::encode_tribute_v2(&source.tribute)
                    .map_err(|e| e.to_string())
            })
            .collect()
    }
    pub fn seed_sources(&self, storage: &StorageHandle<'_>) -> Result<(), String> {
        let contract = TributeContract::new(storage.clone());
        let mut sources = self.sources.values().collect::<Vec<_>>();
        sources.sort_by_key(|source| source.day_index);
        for source in sources {
            let record = StoredTribute::from(&source.tribute);
            if contract
                .records
                .exists(record.tribute_id)
                .map_err(|e| e.to_string())?
            {
                contract
                    .records
                    .update(&record)
                    .map_err(|e| e.to_string())?;
            } else {
                contract
                    .insert_state_record(&record)
                    .map_err(|e| e.to_string())?;
            }
        }
        contract
            .retired_days
            .write(&self.subject.worldwide_day, true)
            .map_err(|e| e.to_string())?;
        let nod = outbe_nod::NodContract::new(storage.clone());
        nod.ocomp_source_block_number
            .write(
                &self.subject.worldwide_day,
                self.subject.checkpoint.block_number,
            )
            .map_err(|e| e.to_string())?;
        nod.ocomp_source_block_hash
            .write(
                &self.subject.worldwide_day,
                self.subject.checkpoint.block_hash,
            )
            .map_err(|e| e.to_string())?;
        nod.ocomp_source_state_root
            .write(
                &self.subject.worldwide_day,
                self.subject.checkpoint.state_root,
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn protect(
        &self,
        authority: &StateNodAuthority,
        batch: &NodMaterializationBatchV1,
    ) -> Result<ProtectedNodMaterializationV2, String> {
        if authority.source != self.subject {
            return Err("fixture source authority mismatch".into());
        }
        let _scope = enclave_scope();
        let leagues = batch
            .actions
            .iter()
            .map(|action| action.league_id)
            .collect::<BTreeSet<_>>();
        outbe_tee::nod_state_materialization::prepare_state_nods(
            PrepareStateNodsRequest {
                authority: authority.clone(),
                batch: batch
                    .encode_canonical(&poc_schema_limits())
                    .map_err(|e| e.to_string())?,
                league_fractions: leagues
                    .into_iter()
                    .map(|league_id| LeagueFractionInputV2 {
                        league_id,
                        fraction_fp: U256::from(1_000_000),
                    })
                    .collect(),
                sources: vec![],
            },
            &mut &*self,
        )
        .map_err(|e| e.to_string())
    }
}
impl TributeSourceReader for &MaterializationFixture {
    fn read_tribute(
        &mut self,
        subject: &TributeSourceSubject,
        id: WwdEntityId,
    ) -> Result<TributeStateSource, TransportError> {
        if *subject != self.subject {
            return Err(rejected("fixture checkpoint mismatch"));
        }
        let source = self
            .sources
            .get(&id)
            .ok_or_else(|| rejected("missing fixture Tribute"))?;
        let verified = source
            .account
            .verify(
                TRIBUTE_ADDRESS,
                subject.checkpoint.state_root,
                outbe_tee::tribute_source::proof_limits(),
            )
            .map_err(rejected)?;
        let mut provider =
            outbe_primitives::storage::readonly::ReadOnlyStorageProvider::new_with_chain_identity(
                verified,
                self.subject.chain_id,
                B256::ZERO,
            );
        StorageHandle::enter(&mut provider, |storage| {
            source.source_opening(&TributeContract::new(storage), &(*subject).into())
        })
        .map_err(rejected)
    }
}

fn source_body(action: &NodActionV1, chain_id: u64) -> Result<EncryptedTributeV2, String> {
    let id = WwdEntityId::from(action.tribute_id);
    let day = WorldwideDay::new(action.wwd);
    if derive_poseidon_entity_id(action.owner, day).map_err(|e| e.to_string())? != id {
        return Err("noncanonical fixture identity".into());
    }
    outbe_tee_enclave::tribute_encryption::encrypt_tribute(
        &NETWORK_SECRET,
        &outbe_tee_enclave::crypto::x25519_public(&[0x6b; 32]),
        TributeContextV2 {
            chain_id,
            tribute_id: id,
            owner: action.owner,
            worldwide_day: day,
            issuance_currency: action.issuance_currency,
            reference_currency: action.reference_currency,
            tribute_price_minor: U256::ONE,
            offer_input_hash: action.tribute_id,
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1000),
            nominal_amount_minor: U256::from(1000),
        },
    )
    .map_err(|e| e.to_string())
}
