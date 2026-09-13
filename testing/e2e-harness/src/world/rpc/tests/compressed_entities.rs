use crate::world::rpc::*;
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
};

fn package(root: B256) -> CompressedEntityAtHeader {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: 1,
            r_sealed: root,
        }),
        ..OutbeBlockArtifacts::default()
    })
    .unwrap();
    CompressedEntityAtHeader {
        result: PointReadResultV1::Unavailable,
        header: SelectedHeaderV1 {
            block_number: 42,
            block_hash: B256::repeat_byte(0x11),
            extra_data: extra_data.to_vec(),
        },
    }
}

#[test]
fn compressed_entity_evidence_binds_transport_hash_and_header_root() {
    let first = package(B256::repeat_byte(0x22));
    let second = package(B256::repeat_byte(0x33));

    let (first_root, first_proof) = first.evidence_identity().unwrap();
    let (second_root, second_proof) = second.evidence_identity().unwrap();

    assert_eq!(first_root, B256::repeat_byte(0x22).to_string());
    assert_eq!(second_root, B256::repeat_byte(0x33).to_string());
    assert_ne!(first_root, second_root);
    assert_eq!(first_proof, second_proof);
    assert_eq!(
        first_proof,
        sha256_hex(&serde_json::to_vec(&PointReadResultV1::Unavailable).unwrap())
    );
}

#[test]
fn compressed_entity_evidence_rejects_header_without_ce_root() {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
    let package = CompressedEntityAtHeader {
        result: PointReadResultV1::Unavailable,
        header: SelectedHeaderV1 {
            block_number: 42,
            block_hash: B256::repeat_byte(0x11),
            extra_data: extra_data.to_vec(),
        },
    };

    assert!(package.evidence_identity().is_err());
}
