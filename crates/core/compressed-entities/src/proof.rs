#[cfg(test)]
use crate::StoredBody;

mod frozen;

use alloy_primitives::{Bytes, B256};
use ark_bn254::Fr;
use ark_ff::PrimeField;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    body_commitment, collection_key, collection_root,
    commitment::{field_to_be32, TAG_TOP_NODE},
    protobuf::{decode_stored_nod_bucket_v1, decode_stored_nod_item_v1, decode_stored_tribute_v1},
    schema::Collection,
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::{derive_tree_key, PoseidonSmt, TreeKey, TreeLeaf, TreeProof, TreeRoot},
    staging::{AuthenticatedCatalogView, StagingCkbStore},
    CeDomain, CompressedTreeService, ExactParentIdentity, FinalizedMarker, TreeNamespace,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME,
};

pub const PROOF_ENCODING_VERSION_V1: u32 = 1;
const MAX_COMPILED_PROOF_BYTES: usize = 64 * 1024;
const MAX_CANONICAL_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PointReadRequestV1 {
    pub domain_id: u16,
    #[serde(with = "entity_id_hex")]
    pub raw_id: WwdEntityId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PointProofCommonV1 {
    pub proof_encoding_version: u32,
    pub chain_id: u64,
    pub block_number: u64,
    pub block_hash: B256,
    pub domain_id: u16,
    #[serde(with = "entity_id_hex")]
    pub raw_id: WwdEntityId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CkbCompiledProofV1(Bytes);

impl CkbCompiledProofV1 {
    fn from_tree(proof: &TreeProof) -> Result<Self, PointReadServiceError> {
        Self::new(proof.as_bytes().to_vec())
    }

    pub fn new(bytes: Vec<u8>) -> Result<Self, PointReadServiceError> {
        if bytes.is_empty() || bytes.len() > MAX_COMPILED_PROOF_BYTES {
            return Err(PointReadServiceError::MalformedProofLength {
                actual: bytes.len(),
            });
        }
        Ok(Self(bytes.into()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn tree_proof(&self) -> Result<TreeProof, PointReadServiceError> {
        if self.0.is_empty() || self.0.len() > MAX_COMPILED_PROOF_BYTES {
            return Err(PointReadServiceError::MalformedProofLength {
                actual: self.0.len(),
            });
        }
        Ok(TreeProof::from_bytes(self.0.to_vec()))
    }
}

impl Serialize for CkbCompiledProofV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("0x{}", hex::encode(self.as_bytes())))
    }
}

impl<'de> Deserialize<'de> for CkbCompiledProofV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let encoded = String::deserialize(deserializer)?;
        let encoded = encoded
            .strip_prefix("0x")
            .ok_or_else(|| D::Error::custom("compiled proof must have a 0x prefix"))?;
        if encoded.len() > MAX_COMPILED_PROOF_BYTES * 2 {
            return Err(D::Error::custom("compiled proof exceeds v1 bound"));
        }
        let bytes = hex::decode(encoded).map_err(D::Error::custom)?;
        Self::new(bytes).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresentEvidenceV1 {
    pub shard_smt_proof: CkbCompiledProofV1,
    #[serde(deserialize_with = "top_sibling_bytes::deserialize")]
    pub shard_top_siblings: Vec<B256>,
    pub root_catalog_proof: CkbCompiledProofV1,
}

/// Membership against an independently authenticated frozen collection root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectionBodyProofV1 {
    pub shard_smt_proof: CkbCompiledProofV1,
    #[serde(deserialize_with = "top_sibling_bytes::deserialize")]
    pub shard_top_siblings: Vec<B256>,
}
impl From<&PresentEvidenceV1> for CollectionBodyProofV1 {
    fn from(e: &PresentEvidenceV1) -> Self {
        Self {
            shard_smt_proof: e.shard_smt_proof.clone(),
            shard_top_siblings: e.shard_top_siblings.clone(),
        }
    }
}
pub fn verify_body_in_collection(
    expected_root: B256,
    domain: CeDomain,
    raw_id: WwdEntityId,
    stored_body: &[u8],
    evidence: &CollectionBodyProofV1,
) -> Result<(), PointReadServiceError> {
    let leaf = canonical_body_leaf(domain, raw_id, stored_body)?;
    let collection = collection_key(domain, raw_id)
        .map_err(|_| PointReadServiceError::InvalidPackage("collection derivation"))?;
    if verify_collection(domain, raw_id, collection, leaf, evidence)? != expected_root {
        return Err(PointReadServiceError::InvalidPackage(
            "collection root mismatch",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsentEvidenceV1 {
    CollectionAbsent {
        root_catalog_proof: CkbCompiledProofV1,
    },
    EntityAbsentInCollection {
        shard_smt_proof: CkbCompiledProofV1,
        #[serde(deserialize_with = "top_sibling_bytes::deserialize")]
        shard_top_siblings: Vec<B256>,
        root_catalog_proof: CkbCompiledProofV1,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointReadResultV1 {
    Present {
        common: PointProofCommonV1,
        #[serde(with = "body_bytes_hex")]
        body_bytes: Bytes,
        evidence: PresentEvidenceV1,
    },
    Absent {
        common: PointProofCommonV1,
        evidence: AbsentEvidenceV1,
    },
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedHeaderV1 {
    pub block_number: u64,
    pub block_hash: B256,
    pub extra_data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifiedPointReadV1 {
    Present,
    Absent,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PointReadRequestError {
    #[error("unknown compressed-entity domain ID {0}")]
    UnknownDomain(u16),
}

#[derive(Debug, Error)]
pub enum PointReadServiceError {
    #[error("compressed-entity proof service is not ready at genesis")]
    GenesisUnavailable,
    #[error("compressed-entity proof materialization is inconsistent: {0}")]
    Materialization(String),
    #[error("compiled proof length {actual} is outside v1 bounds")]
    MalformedProofLength { actual: usize },
    #[error("point proof package is invalid: {0}")]
    InvalidPackage(&'static str),
}

impl PointReadServiceError {
    pub(crate) fn materialization(error: impl core::fmt::Display) -> Self {
        Self::Materialization(error.to_string())
    }
}

#[derive(Clone, Debug)]
struct FrozenPointReadV1 {
    marker: FinalizedMarker,
    result: FrozenResultV1,
}

#[derive(Clone, Debug)]
enum FrozenResultV1 {
    Present {
        common: PointProofCommonV1,
        expected_leaf: B256,
        evidence: PresentEvidenceV1,
    },
    Absent {
        common: PointProofCommonV1,
        evidence: AbsentEvidenceV1,
    },
}

impl CompressedTreeService {
    /// Freezes tree evidence first. Both callbacks run only after the sole MDBX
    /// read transaction has been dropped.
    pub fn serve_point_read_v1<H, B>(
        &self,
        chain_id: u64,
        request: PointReadRequestV1,
        header_lookup: H,
        body_lookup: B,
    ) -> Result<PointReadResultV1, PointReadRequestError>
    where
        H: FnOnce(u64, B256) -> Option<SelectedHeaderV1>,
        B: FnOnce(CeDomain, WwdEntityId) -> Option<Vec<u8>>,
    {
        let domain = domain(request.domain_id)?;
        let frozen = match self.freeze_point_read(chain_id, request, domain) {
            Ok(frozen) => frozen,
            Err(_) => return Ok(PointReadResultV1::Unavailable),
        };
        let Some(header) = header_lookup(frozen.marker.height, frozen.marker.block_hash) else {
            return Ok(PointReadResultV1::Unavailable);
        };
        if validate_header(&frozen.marker, &header).is_err() {
            return Ok(PointReadResultV1::Unavailable);
        }
        match frozen.result {
            FrozenResultV1::Absent { common, evidence } => {
                Ok(PointReadResultV1::Absent { common, evidence })
            }
            FrozenResultV1::Present {
                common,
                expected_leaf,
                evidence,
            } => {
                let Some(body_bytes) = body_lookup(domain, request.raw_id) else {
                    return Ok(PointReadResultV1::Unavailable);
                };
                match canonical_body_leaf(domain, request.raw_id, &body_bytes) {
                    Ok(actual) if actual == expected_leaf => Ok(PointReadResultV1::Present {
                        common,
                        body_bytes: body_bytes.into(),
                        evidence,
                    }),
                    _ => Ok(PointReadResultV1::Unavailable),
                }
            }
        }
    }

    fn freeze_point_read(
        &self,
        chain_id: u64,
        request: PointReadRequestV1,
        domain: CeDomain,
    ) -> Result<FrozenPointReadV1, PointReadServiceError> {
        let (marker, view) = frozen::finalized_view(self)?;
        let common = PointProofCommonV1 {
            proof_encoding_version: PROOF_ENCODING_VERSION_V1,
            chain_id,
            block_number: marker.height,
            block_hash: marker.block_hash,
            domain_id: request.domain_id,
            raw_id: request.raw_id,
        };
        let collection = collection_key(domain, request.raw_id)
            .map_err(PointReadServiceError::materialization)?;
        let (catalog_leaf, root_catalog_proof) = frozen::catalog_entry(&view, collection)?;
        if catalog_leaf == TreeLeaf::ZERO {
            if view
                .collection_has_records(collection)
                .map_err(PointReadServiceError::materialization)?
            {
                return Err(PointReadServiceError::Materialization(
                    "orphan records behind absent catalog leaf".into(),
                ));
            }
            return Ok(FrozenPointReadV1 {
                marker,
                result: FrozenResultV1::Absent {
                    common,
                    evidence: AbsentEvidenceV1::CollectionAbsent { root_catalog_proof },
                },
            });
        }
        let (leaf, evidence) = frozen::point_evidence(
            &view,
            (collection, catalog_leaf),
            domain,
            request.raw_id,
            root_catalog_proof,
        )?;
        let result = frozen::point_result(common, leaf, evidence);
        Ok(FrozenPointReadV1 { marker, result })
    }
}

pub fn verify_point_read_v1(
    expected_chain_id: u64,
    expected_request: PointReadRequestV1,
    selected_header: &SelectedHeaderV1,
    result: &PointReadResultV1,
) -> Result<VerifiedPointReadV1, PointReadServiceError> {
    let domain = domain(expected_request.domain_id)
        .map_err(|_| PointReadServiceError::InvalidPackage("unknown domain"))?;
    let common = match result {
        PointReadResultV1::Unavailable => {
            return Err(PointReadServiceError::InvalidPackage(
                "unavailable is not a proof package",
            ))
        }
        PointReadResultV1::Present { common, .. } | PointReadResultV1::Absent { common, .. } => {
            common
        }
    };
    validate_common(expected_chain_id, expected_request, selected_header, common)?;
    let (expected_leaf, verdict) = match result {
        PointReadResultV1::Unavailable => unreachable!(),
        PointReadResultV1::Present { body_bytes, .. } => {
            if body_bytes.is_empty() {
                return Err(PointReadServiceError::InvalidPackage("empty present body"));
            }
            (
                canonical_body_leaf(domain, expected_request.raw_id, body_bytes.as_ref())?,
                VerifiedPointReadV1::Present,
            )
        }
        PointReadResultV1::Absent { .. } => (B256::ZERO, VerifiedPointReadV1::Absent),
    };
    let artifact = header_artifact(selected_header)?;
    let collection = collection_key(domain, expected_request.raw_id)
        .map_err(|_| PointReadServiceError::InvalidPackage("collection derivation"))?;
    let catalog_key = TreeKey::from_be_bytes(*collection.as_bytes())
        .map_err(|_| PointReadServiceError::InvalidPackage("catalog key"))?;
    let (computed_collection, catalog_proof) = match result {
        PointReadResultV1::Present { evidence, .. } => (
            verify_collection(
                domain,
                expected_request.raw_id,
                collection,
                expected_leaf,
                &CollectionBodyProofV1::from(evidence),
            )?,
            &evidence.root_catalog_proof,
        ),
        PointReadResultV1::Absent { evidence, .. } => match evidence {
            AbsentEvidenceV1::CollectionAbsent { root_catalog_proof } => {
                (B256::ZERO, root_catalog_proof)
            }
            AbsentEvidenceV1::EntityAbsentInCollection {
                shard_smt_proof,
                shard_top_siblings,
                root_catalog_proof,
            } => {
                let e = PresentEvidenceV1 {
                    shard_smt_proof: shard_smt_proof.clone(),
                    shard_top_siblings: shard_top_siblings.clone(),
                    root_catalog_proof: root_catalog_proof.clone(),
                };
                (
                    verify_collection(
                        domain,
                        expected_request.raw_id,
                        collection,
                        B256::ZERO,
                        &CollectionBodyProofV1::from(&e),
                    )?,
                    root_catalog_proof,
                )
            }
        },
        PointReadResultV1::Unavailable => unreachable!(),
    };
    let catalog_root = catalog_proof
        .tree_proof()?
        .compute_root(
            catalog_key,
            TreeLeaf::from_be_bytes(computed_collection.0)
                .map_err(|_| PointReadServiceError::InvalidPackage("catalog leaf"))?,
        )
        .map_err(|_| PointReadServiceError::InvalidPackage("catalog proof"))?;
    let sealed = crate::sealed_root(B256::from(catalog_root.as_bytes()))
        .map_err(|_| PointReadServiceError::InvalidPackage("sealed root"))?;
    if sealed != artifact.r_sealed {
        return Err(PointReadServiceError::InvalidPackage(
            "header root mismatch",
        ));
    }
    Ok(verdict)
}

fn verify_collection(
    domain: CeDomain,
    raw_id: WwdEntityId,
    collection: crate::CollectionKey,
    leaf: B256,
    evidence: &CollectionBodyProofV1,
) -> Result<B256, PointReadServiceError> {
    let key = derive_tree_key(collection_for_domain(domain), raw_id)
        .map_err(|_| PointReadServiceError::InvalidPackage("tree key"))?;
    let shard = shard_index(key, domain.shard_count())
        .map_err(|_| PointReadServiceError::InvalidPackage("shard index"))?;
    if evidence.shard_top_siblings.len() != domain.shard_count().trailing_zeros() as usize {
        return Err(PointReadServiceError::InvalidPackage("top sibling count"));
    }
    let mut current = evidence
        .shard_smt_proof
        .tree_proof()?
        .compute_root(
            key,
            TreeLeaf::from_be_bytes(leaf.0)
                .map_err(|_| PointReadServiceError::InvalidPackage("leaf field"))?,
        )
        .map_err(|_| PointReadServiceError::InvalidPackage("shard proof"))?
        .as_bytes();
    for (level, sibling) in evidence.shard_top_siblings.iter().enumerate() {
        TreeRoot::from_be_bytes(sibling.0)
            .map_err(|_| PointReadServiceError::InvalidPackage("non-canonical top sibling"))?;
        current = top_hash(level as u32, shard, current, sibling.0)?;
        TreeRoot::from_be_bytes(current)
            .map_err(|_| PointReadServiceError::InvalidPackage("invalid top root"))?;
    }
    collection_root(domain, collection, B256::from(current))
        .map_err(|_| PointReadServiceError::InvalidPackage("collection root"))
}

fn validate_common(
    chain_id: u64,
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    common: &PointProofCommonV1,
) -> Result<(), PointReadServiceError> {
    let request_bound = (common.chain_id, common.domain_id, common.raw_id)
        == (chain_id, request.domain_id, request.raw_id);
    let header_bound =
        (common.block_number, common.block_hash) == (header.block_number, header.block_hash);
    if common.proof_encoding_version != PROOF_ENCODING_VERSION_V1 || !request_bound {
        return Err(PointReadServiceError::InvalidPackage("common binding"));
    }
    if common.block_number == 0 || !header_bound {
        return Err(PointReadServiceError::InvalidPackage("common binding"));
    }
    Ok(())
}

fn validate_header(
    marker: &FinalizedMarker,
    header: &SelectedHeaderV1,
) -> Result<(), PointReadServiceError> {
    if marker.height != header.block_number || marker.block_hash != header.block_hash {
        return Err(PointReadServiceError::InvalidPackage(
            "marker/header identity",
        ));
    }
    let artifact = header_artifact(header)?;
    if artifact.commitment_scheme_version != marker.commitment_scheme_version
        || artifact.r_sealed != marker.new_root
    {
        return Err(PointReadServiceError::InvalidPackage("marker/header root"));
    }
    Ok(())
}

fn header_artifact(
    header: &SelectedHeaderV1,
) -> Result<outbe_primitives::reshare_artifact::CompressedEntitiesRootArtifact, PointReadServiceError>
{
    if header.block_number == 0 {
        return Err(PointReadServiceError::InvalidPackage("genesis header"));
    }
    let decoded =
        outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(&header.extra_data)
            .map_err(|_| PointReadServiceError::InvalidPackage("header artifacts"))?;
    let artifact = decoded
        .compressed_entities_root
        .ok_or(PointReadServiceError::InvalidPackage("missing tag 0x08"))?;
    if artifact.commitment_scheme_version != ACTIVE_COMMITMENT_SCHEME {
        return Err(PointReadServiceError::InvalidPackage("fork scheme"));
    }
    Ok(artifact)
}

pub(crate) fn canonical_body_leaf(
    domain: CeDomain,
    raw_id: WwdEntityId,
    bytes: &[u8],
) -> Result<B256, PointReadServiceError> {
    let stored = crate::decode_stored_body(bytes)
        .map_err(|_| PointReadServiceError::InvalidPackage("stored body envelope"))?;
    let body_id = match domain {
        CeDomain::Tribute if stored.schema_version() == crate::TRIBUTE_BODY_SCHEMA_V2 => {
            crate::decode_stored_tribute_v2(bytes)
                .map(|body| body.context.tribute_id)
                .map_err(|_| PointReadServiceError::InvalidPackage("encrypted tribute body"))?
        }
        CeDomain::Tribute => decode_stored_tribute_v1(bytes)
            .map(|b| b.tribute_id)
            .map_err(|_| PointReadServiceError::InvalidPackage("tribute body"))?,
        CeDomain::NodItem if stored.schema_version() == crate::NOD_BODY_SCHEMA_V2 => {
            crate::decode_stored_nod_item_v2(bytes)
                .map(|body| body.encrypted.terms.nod_id)
                .map_err(|_| PointReadServiceError::InvalidPackage("encrypted nod body"))?
        }
        CeDomain::NodItem => decode_stored_nod_item_v1(bytes)
            .map(|b| b.nod_id)
            .map_err(|_| PointReadServiceError::InvalidPackage("nod item body"))?,
        CeDomain::NodBucket => decode_stored_nod_bucket_v1(bytes)
            .map(|b| b.entity_id())
            .map_err(|_| PointReadServiceError::InvalidPackage("nod bucket body"))?,
        CeDomain::Gem => crate::decode_stored_gem_v1(bytes)
            .map(|b| b.gem_id)
            .map_err(|_| PointReadServiceError::InvalidPackage("gem body"))?,
        CeDomain::GemBucket => crate::decode_stored_gem_bucket_v1(bytes)
            .map(|b| b.entity_id())
            .map_err(|_| PointReadServiceError::InvalidPackage("gem bucket body"))?,
    };
    if body_id != raw_id {
        return Err(PointReadServiceError::InvalidPackage("body identity"));
    }
    body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored.schema_version(),
        raw_id,
        stored.payload(),
    )
    .map(|leaf| B256::from(*leaf.as_bytes()))
    .map_err(|_| PointReadServiceError::InvalidPackage("body commitment"))
}

/// Single-leaf fixture using the production Poseidon/CKB proof implementation.
#[cfg(feature = "test-utils")]
pub(crate) fn single_body_proof(
    domain: CeDomain,
    id: WwdEntityId,
    body: &[u8],
) -> Result<(B256, CollectionBodyProofV1), PointReadServiceError> {
    let leaf = canonical_body_leaf(domain, id, body)?;
    let key = derive_tree_key(collection_for_domain(domain), id)
        .map_err(PointReadServiceError::materialization)?;
    let shard =
        shard_index(key, domain.shard_count()).map_err(PointReadServiceError::materialization)?;
    let mut tree = PoseidonSmt::empty();
    tree.update(
        key,
        TreeLeaf::from_be_bytes(leaf.0).map_err(PointReadServiceError::materialization)?,
    )
    .map_err(PointReadServiceError::materialization)?;
    let root = tree
        .root()
        .map_err(PointReadServiceError::materialization)?;
    let mut roots = vec![B256::ZERO; domain.shard_count() as usize];
    roots[shard as usize] = B256::from(root.as_bytes());
    let top = aggregate_b256_shard_roots(&roots).map_err(PointReadServiceError::materialization)?;
    let collection = collection_key(domain, id).map_err(PointReadServiceError::materialization)?;
    let collection_root =
        collection_root(domain, collection, top).map_err(PointReadServiceError::materialization)?;
    let proof = tree
        .prove(vec![key])
        .map_err(PointReadServiceError::materialization)?;
    Ok((
        collection_root,
        CollectionBodyProofV1 {
            shard_smt_proof: CkbCompiledProofV1::from_tree(&proof)?,
            shard_top_siblings: top_siblings(&roots, shard)?,
        },
    ))
}

pub(crate) fn top_siblings(
    roots: &[B256],
    selected: u32,
) -> Result<Vec<B256>, PointReadServiceError> {
    if roots.is_empty()
        || roots.len() > 256
        || !roots.len().is_power_of_two()
        || selected as usize >= roots.len()
    {
        return Err(PointReadServiceError::InvalidPackage(
            "invalid shard-top envelope",
        ));
    }
    let mut level_roots: Vec<[u8; 32]> = roots.iter().map(|r| r.0).collect();
    let mut position = selected as usize;
    let mut siblings = Vec::with_capacity(roots.len().trailing_zeros() as usize);
    let mut level = 0_u32;
    while level_roots.len() > 1 {
        siblings.push(B256::from(level_roots[position ^ 1]));
        let mut parents = Vec::with_capacity(level_roots.len() / 2);
        for pair in level_roots.as_chunks::<2>().0 {
            parents.push(top_hash(level, 0, pair[0], pair[1])?);
        }
        level_roots = parents;
        position /= 2;
        level += 1;
    }
    Ok(siblings)
}

fn top_hash(
    level: u32,
    shard_index: u32,
    current: [u8; 32],
    sibling: [u8; 32],
) -> Result<[u8; 32], PointReadServiceError> {
    let (left, right) = if ((shard_index >> level) & 1) == 0 {
        (current, sibling)
    } else {
        (sibling, current)
    };
    let inputs = [
        Fr::from(level),
        Fr::from_be_bytes_mod_order(&left),
        Fr::from_be_bytes_mod_order(&right),
    ];
    crate::commitment::poseidon(TAG_TOP_NODE, &inputs)
        .map(field_to_be32)
        .map_err(|_| PointReadServiceError::InvalidPackage("top hash"))
}

fn domain(id: u16) -> Result<CeDomain, PointReadRequestError> {
    match id {
        1 => Ok(CeDomain::Tribute),
        2 => Ok(CeDomain::NodItem),
        3 => Ok(CeDomain::NodBucket),
        4 => Ok(CeDomain::Gem),
        5 => Ok(CeDomain::GemBucket),
        other => Err(PointReadRequestError::UnknownDomain(other)),
    }
}

const fn collection_for_domain(domain: CeDomain) -> Collection {
    match domain {
        CeDomain::Tribute => Collection::Tribute,
        CeDomain::NodItem => Collection::NodItem,
        CeDomain::NodBucket => Collection::NodBucket,
        CeDomain::Gem => Collection::Gem,
        CeDomain::GemBucket => Collection::GemBucket,
    }
}

mod entity_id_hex {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use crate::WwdEntityId;

    pub fn serialize<S>(value: &WwdEntityId, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{}", hex::encode(value.as_slice())))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<WwdEntityId, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let encoded = encoded
            .strip_prefix("0x")
            .ok_or_else(|| D::Error::custom("entity ID must have a 0x prefix"))?;
        let bytes = hex::decode(encoded).map_err(D::Error::custom)?;
        WwdEntityId::try_from(bytes.as_slice()).map_err(D::Error::custom)
    }
}

mod body_bytes_hex {
    use alloy_primitives::Bytes;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    use super::MAX_CANONICAL_BODY_BYTES;

    pub fn serialize<S>(value: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{}", hex::encode(value)))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let encoded = encoded
            .strip_prefix("0x")
            .ok_or_else(|| D::Error::custom("body bytes must have a 0x prefix"))?;
        if encoded.len() > MAX_CANONICAL_BODY_BYTES * 2 {
            return Err(D::Error::custom("body bytes exceed v1 bound"));
        }
        hex::decode(encoded)
            .map(Bytes::from)
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests;

mod top_sibling_bytes {
    use alloy_primitives::B256;
    use serde::{
        de::{Error, SeqAccess, Visitor},
        Deserializer,
    };
    use std::fmt;

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<B256>, D::Error> {
        struct Siblings;
        impl<'de> Visitor<'de> for Siblings {
            type Value = Vec<B256>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most eight shard-top siblings")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut siblings = Vec::with_capacity(8);
                while let Some(sibling) = sequence.next_element()? {
                    if siblings.len() == 8 {
                        return Err(A::Error::custom("too many shard-top siblings"));
                    }
                    siblings.push(sibling);
                }
                Ok(siblings)
            }
        }
        deserializer.deserialize_seq(Siblings)
    }
}
