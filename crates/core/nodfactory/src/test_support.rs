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
        LeagueFractionInputV2, NodMaterializationAuthorityV2, NodSourceV2,
        PrepareEncryptedNodsRequestV2,
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

/// Errors returned while constructing or using encrypted NOD materialization test fixtures.
#[derive(Debug, thiserror::Error)]
pub enum MaterializationFixtureError {
    #[error("empty materialization fixture")]
    EmptyFixture,
    #[error("fixture count overflow")]
    CountOverflow,
    #[error("fixture source authority mismatch")]
    SourceAuthorityMismatch,
    #[error("fixture source missing")]
    SourceMissing,
    #[error("fixture Tribute identity is not canonical for owner/day")]
    NonCanonicalTributeIdentity,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Collection(#[from] outbe_compressed_entities::CollectionError),
    #[error(transparent)]
    Reconstruction(#[from] outbe_compressed_entities::TributePartitionReconstructionError),
    #[error(transparent)]
    CanonicalBody(#[from] outbe_compressed_entities::CanonicalBodyError),
    #[error(transparent)]
    Commitment(#[from] outbe_compressed_entities::CommitmentError),
    #[error(transparent)]
    Postcard(#[from] postcard::Error),
    #[error(transparent)]
    Precompile(#[from] outbe_primitives::error::PrecompileError),
    #[error(transparent)]
    Protocol(#[from] outbe_ocomp_protocol::error::ProtocolError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    EntityId(#[from] core::array::TryFromSliceError),
    #[error(transparent)]
    TeeEnclave(#[from] outbe_tee_enclave::errors::TeeError),
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
    pub fn build(&self) -> Result<MaterializationFixture, MaterializationFixtureError> {
        let scratch = tempfile::tempdir()?;
        self.build_at(&scratch.path().join("retained-proofs"))
    }
    pub fn build_at(
        &self,
        archive_path: &std::path::Path,
    ) -> Result<MaterializationFixture, MaterializationFixtureError> {
        let actions = self.actions;
        let chain_id = self.chain_id;
        let day = WorldwideDay::new(
            actions
                .first()
                .ok_or(MaterializationFixtureError::EmptyFixture)?
                .wwd,
        );
        let bodies = actions
            .iter()
            .map(|action| source_body(action, chain_id))
            .collect::<Result<Vec<_>, _>>()?;
        let leaves = bodies
            .iter()
            .map(|(id, _, _, commitment)| (*id, *commitment))
            .collect::<Vec<_>>();
        let source_root = tribute_partition_root_from_leaves(day, leaves.iter().copied())?;
        let scratch = tempfile::tempdir()?;
        let mut builder = BoundedTributePartitionVerifier::create(
            scratch.path().join("proofs"),
            TributePartitionExpectationV1 {
                day,
                exact_leaf_count: actions
                    .len()
                    .try_into()
                    .map_err(|_| MaterializationFixtureError::CountOverflow)?,
                expected_collection_root: source_root,
                commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
            },
            TributePartitionWorkConfig::default(),
        )?;
        for (id, commitment) in leaves {
            builder.push(id, commitment)?;
        }
        let archive = builder.finish_with_archive(|| {})?;
        let sources = bodies
            .into_iter()
            .map(|(id, tribute, _, _)| {
                let proof = archive.proof(id)?;
                Ok((
                    B256::from_slice(id.as_slice()),
                    NodSourceV2 {
                        tribute,
                        proof: postcard::to_allocvec(&proof)?,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, MaterializationFixtureError>>()?;
        if let Some(parent) = archive_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(archive.path(), archive_path)?;
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
    pub fn canonical_source_bodies(&self) -> Result<Vec<Vec<u8>>, MaterializationFixtureError> {
        self.sources
            .values()
            .map(|source| Ok(encode_tribute_v2(&source.tribute)?))
            .collect()
    }
    pub fn source_root(&self) -> B256 {
        self.source_root
    }
    pub fn seed_source_root(
        &self,
        storage: &StorageHandle<'_>,
    ) -> Result<(), MaterializationFixtureError> {
        let mut admission = outbe_tribute::DayPreAdmission::with_key(self.day);
        admission.initialized = true;
        admission.is_sealed = true;
        admission.sealed_collection_root = self.source_root;
        admission.sealed_tribute_count = self
            .sources
            .len()
            .try_into()
            .map_err(|_| MaterializationFixtureError::CountOverflow)?;
        outbe_tribute::enclave_client::test_enclave::seed_pre_admission(
            &mut outbe_tribute::TributeContract::new(storage.clone()),
            &admission,
        )?;
        Ok(())
    }
    pub fn protect(
        &self,
        authority: &NodMaterializationAuthorityV2,
        batch: &NodMaterializationBatchV1,
    ) -> Result<ProtectedNodMaterializationV2, MaterializationFixtureError> {
        if authority.sealed_tribute_root != self.source_root {
            return Err(MaterializationFixtureError::SourceAuthorityMismatch);
        }
        let sources = batch
            .actions
            .iter()
            .map(|action| {
                self.sources
                    .get(&action.tribute_id)
                    .cloned()
                    .ok_or(MaterializationFixtureError::SourceMissing)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut leagues = std::collections::BTreeSet::new();
        for action in &batch.actions {
            leagues.insert(action.league_id);
        }
        let request = PrepareEncryptedNodsRequestV2 {
            authority: authority.clone(),
            batch: batch.encode_canonical(&poc_schema_limits())?,
            sources,
            league_fractions: leagues
                .into_iter()
                .map(|league_id| LeagueFractionInputV2 {
                    league_id,
                    fraction_fp: U256::from(1_000_000),
                })
                .collect(),
        };
        Ok(prepare(&request)?)
    }
}
fn source_body(
    action: &NodActionV1,
    chain_id: u64,
) -> Result<(WwdEntityId, EncryptedTributeV2, StoredBody, Commitment), MaterializationFixtureError>
{
    let id = WwdEntityId::try_from(action.tribute_id.as_slice())?;
    let canonical = outbe_compressed_entities::derive_poseidon_entity_id(
        action.owner,
        WorldwideDay::new(action.wwd),
    )?;
    if id != canonical {
        return Err(MaterializationFixtureError::NonCanonicalTributeIdentity);
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
            offer_input_hash: action.tribute_id,
        },
        &TributeAmountsV2 {
            issuance_amount_minor: U256::from(1_000),
            nominal_amount_minor: U256::from(1_000),
        },
    )?;
    let stored = StoredBody::new(TRIBUTE_BODY_SCHEMA_V2, encode_tribute_v2(&tribute)?)?;
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        TRIBUTE_BODY_SCHEMA_V2,
        id,
        stored.payload(),
    )?;
    Ok((id, tribute, stored, commitment))
}
