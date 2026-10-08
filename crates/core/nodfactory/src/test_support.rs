//! Real cryptographic fixtures for encrypted NOD materialization.
use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{
    body_commitment, encode_tribute_v2, tribute_partition_root_from_leaves,
    BoundedTributePartitionVerifier, Commitment, StoredBody, TributePartitionExpectationV1,
    TributePartitionWorkConfig, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, TRIBUTE_BODY_SCHEMA_V2,
};
use outbe_ocomp_protocol::{
    nod_materialization::{NodMaterializationBatchV1, ProtectedNodMaterializationV2},
    profile::poc_schema_limits,
    result::NodActionV1,
};
use outbe_primitives::{
    storage::StorageHandle,
    time::WorldwideDay,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2, TributeContextV2},
};
use outbe_tee::{
    nod_materialization::{
        NodMaterializationAuthorityV2, NodSourceV2, PrepareEncryptedNodsRequestV2,
    },
    TransportError,
};
use std::collections::BTreeMap;
pub const NETWORK_SECRET: [u8; 32] = [0x5a; 32];

pub fn enclave_scope() -> outbe_tee::nod_materialization::test_support::Guard {
    outbe_tee::nod_materialization::test_support::scope(prepare, open)
}
fn prepare(
    request: &PrepareEncryptedNodsRequestV2,
) -> Result<ProtectedNodMaterializationV2, TransportError> {
    outbe_tee_enclave::nod_materialization::prepare(&NETWORK_SECRET, request)
        .map_err(|e| TransportError::NodMaterializationRejected(e.to_string()))
}
fn open(
    authority: &NodMaterializationAuthorityV2,
    carrier: &ProtectedNodMaterializationV2,
) -> Result<Vec<outbe_primitives::nod_encryption::EncryptedNodV2>, TransportError> {
    outbe_tee_enclave::nod_materialization::open(&NETWORK_SECRET, authority, carrier)
        .map_err(|e| TransportError::NodMaterializationRejected(e.to_string()))
}

/// Prepares source bodies and proofs before a materialization test starts.
pub struct MaterializationFixtureBuilder<'a> {
    actions: &'a [NodActionV1],
    chain_id: u64,
}
impl<'a> MaterializationFixtureBuilder<'a> {
    pub fn new(actions: &'a [NodActionV1], chain_id: u64) -> Self {
        Self { actions, chain_id }
    }
    pub fn build(&self) -> Result<MaterializationFixture, String> {
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        self.build_at(&scratch.path().join("retained-proofs"))
    }
    pub fn build_at(
        &self,
        archive_path: &std::path::Path,
    ) -> Result<MaterializationFixture, String> {
        let actions = self.actions;
        let chain_id = self.chain_id;
        let day = WorldwideDay::new(actions.first().ok_or("empty materialization fixture")?.wwd);
        let bodies = actions
            .iter()
            .map(|action| source_body(action, chain_id))
            .collect::<Result<Vec<_>, _>>()?;
        let leaves = bodies
            .iter()
            .map(|(id, _, _, commitment)| (*id, *commitment))
            .collect::<Vec<_>>();
        let source_root = tribute_partition_root_from_leaves(day, leaves.iter().copied())
            .map_err(|e| e.to_string())?;
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut builder = BoundedTributePartitionVerifier::create(
            scratch.path().join("proofs"),
            TributePartitionExpectationV1 {
                day,
                exact_leaf_count: actions
                    .len()
                    .try_into()
                    .map_err(|_| "fixture count overflow")?,
                expected_collection_root: source_root,
                commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
            },
            TributePartitionWorkConfig::default(),
        )
        .map_err(|e| e.to_string())?;
        for (id, commitment) in leaves {
            builder.push(id, commitment).map_err(|e| e.to_string())?;
        }
        let archive = builder
            .finish_with_archive(|| {})
            .map_err(|e| e.to_string())?;
        let sources = bodies
            .into_iter()
            .map(|(id, tribute, _, _)| {
                let proof = archive.proof(id).map_err(|e| e.to_string())?;
                Ok((
                    B256::from_slice(id.as_slice()),
                    NodSourceV2 {
                        tribute,
                        proof: postcard::to_allocvec(&proof).map_err(|e| e.to_string())?,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        if let Some(parent) = archive_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::rename(archive.path(), archive_path).map_err(|e| e.to_string())?;
        Ok(MaterializationFixture {
            day,
            source_root,
            sources,
        })
    }
}

pub struct MaterializationFixture {
    day: WorldwideDay,
    source_root: B256,
    sources: BTreeMap<B256, NodSourceV2>,
}
impl MaterializationFixture {
    pub fn canonical_source_bodies(&self) -> Result<Vec<Vec<u8>>, String> {
        self.sources
            .values()
            .map(|source| encode_tribute_v2(&source.tribute).map_err(|e| e.to_string()))
            .collect()
    }
    pub fn source_root(&self) -> B256 {
        self.source_root
    }
    pub fn seed_source_root(&self, storage: &StorageHandle<'_>) -> Result<(), String> {
        let mut admission = outbe_tribute::DayPreAdmission::with_key(self.day);
        admission.initialized = true;
        admission.is_sealed = true;
        admission.sealed_collection_root = self.source_root;
        admission.sealed_tribute_count = self
            .sources
            .len()
            .try_into()
            .map_err(|_| "fixture count overflow")?;
        outbe_tribute::enclave_client::test_enclave::seed_pre_admission(
            &mut outbe_tribute::TributeContract::new(storage.clone()),
            &admission,
        )
        .map_err(|e| e.to_string())
    }
    pub fn protect(
        &self,
        authority: &NodMaterializationAuthorityV2,
        batch: &NodMaterializationBatchV1,
    ) -> Result<ProtectedNodMaterializationV2, String> {
        if authority.sealed_tribute_root != self.source_root {
            return Err("fixture source authority mismatch".into());
        }
        let sources = batch
            .actions
            .iter()
            .map(|action| {
                self.sources
                    .get(&action.tribute_id)
                    .cloned()
                    .ok_or_else(|| "fixture source missing".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request = PrepareEncryptedNodsRequestV2 {
            authority: authority.clone(),
            batch: batch
                .encode_canonical(&poc_schema_limits())
                .map_err(|e| e.to_string())?,
            sources,
        };
        prepare(&request).map_err(|e| e.to_string())
    }
}
fn source_body(
    action: &NodActionV1,
    chain_id: u64,
) -> Result<(WwdEntityId, EncryptedTributeV2, StoredBody, Commitment), String> {
    let id = WwdEntityId::try_from(action.tribute_id.as_slice()).map_err(|e| e.to_string())?;
    let canonical = outbe_compressed_entities::derive_poseidon_entity_id(
        action.owner,
        WorldwideDay::new(action.wwd),
    )
    .map_err(|e| e.to_string())?;
    if id != canonical {
        return Err("fixture Tribute identity is not canonical for owner/day".into());
    }
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([0x6b; 32]));
    let tribute = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
        &NETWORK_SECRET,
        public.as_bytes(),
        TributeContextV2 {
            chain_id,
            tribute_id: id,
            owner: action.owner,
            worldwide_day: WorldwideDay::new(action.wwd),
            issuance_currency: action.issuance_currency,
            reference_currency: action.reference_currency,
            tribute_price_minor: U256::ONE,
            exclude_from_intex_issuance: false,
            offer_input_hash: action.tribute_id,
        },
        &TributeAmountsV2 {
            issuance_amount_minor: action.gratis_load_minor,
            nominal_amount_minor: action.gratis_load_minor,
        },
    )
    .map_err(|e| e.to_string())?;
    let stored = StoredBody::new(
        TRIBUTE_BODY_SCHEMA_V2,
        encode_tribute_v2(&tribute).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        TRIBUTE_BODY_SCHEMA_V2,
        id,
        stored.payload(),
    )
    .map_err(|e| e.to_string())?;
    Ok((id, tribute, stored, commitment))
}
