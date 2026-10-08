use super::*;

#[test]
fn adversarial_mutations_and_saved_proof_after_advance_are_rejected_or_remain_historical() {
    let _guard = proof_test_guard();
    let (_dir, service, genesis_hash) = service();
    let (id, body, leaf) = bucket_body(7);
    let (marker, header) = finalize_one(&service, genesis_hash, EntityRef::NodBucket(id), leaf);
    let request = PointReadRequestV1 {
        domain_id: 3,
        raw_id: id,
    };
    let package = service
        .serve_point_read_v1(7, request, |_, _| Some(header.clone()), |_, _| Some(body))
        .unwrap();

    assert_metadata_mutations(request, &header, &package);
    assert_compiled_proof_mutations(request, &header, &package);
    assert_result_and_schema_mutations(request, &header, &package);
    assert_header_mutations(request, &header, &package, &marker);
    let next_hash = B256::repeat_byte(0x44);
    let provisional = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        })
        .unwrap()
        .prepare_seal(2, &[], &[])
        .unwrap();
    let next_root = provisional.new_root();
    service.publish_candidate(next_hash, provisional).unwrap();
    service.apply_finalized(2, next_hash, next_root).unwrap();
    assert_eq!(
        verify_point_read_v1(7, request, &header, &package).unwrap(),
        VerifiedPointReadV1::Present,
        "issued evidence remains valid for its independently supplied historical header"
    );
}

fn assert_metadata_mutations(
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    package: &PointReadResultV1,
) {
    let id = request.raw_id;
    let mut wrong_chain = (*package).clone();
    if let PointReadResultV1::Present { common, .. } = &mut wrong_chain {
        common.chain_id = 8;
    }
    assert!(verify_point_read_v1(7, request, header, &wrong_chain).is_err());
    let mut wrong_identity = (*package).clone();
    if let PointReadResultV1::Present { common, .. } = &mut wrong_identity {
        common.raw_id = WwdEntityId::from_day_and_digest(id.worldwide_day(), [0x77; 32]);
    }
    assert!(verify_point_read_v1(7, request, header, &wrong_identity).is_err());
    let mut wrong_body = (*package).clone();
    if let PointReadResultV1::Present { body_bytes, .. } = &mut wrong_body {
        let mut mutated = body_bytes.to_vec();
        mutated[0] ^= 1;
        *body_bytes = mutated.into();
    }
    assert!(verify_point_read_v1(7, request, header, &wrong_body).is_err());
    let mut wrong_sibling = (*package).clone();
    if let PointReadResultV1::Present { evidence, .. } = &mut wrong_sibling {
        evidence.shard_top_siblings[0] = B256::repeat_byte(0x66);
    }
    assert!(verify_point_read_v1(7, request, header, &wrong_sibling).is_err());
    let mut wrong_proof = (*package).clone();
    if let PointReadResultV1::Present { evidence, .. } = &mut wrong_proof {
        let mut mutated = evidence.shard_smt_proof.0.to_vec();
        mutated[0] ^= 1;
        evidence.shard_smt_proof.0 = mutated.into();
    }
    assert!(verify_point_read_v1(7, request, header, &wrong_proof).is_err());

    for mutate in [
        |common: &mut PointProofCommonV1| common.proof_encoding_version += 1,
        |common: &mut PointProofCommonV1| common.block_number += 1,
        |common: &mut PointProofCommonV1| common.block_hash = B256::repeat_byte(0x81),
        |common: &mut PointProofCommonV1| common.domain_id = CeDomain::NodItem.id(),
    ] {
        let mut candidate = (*package).clone();
        if let PointReadResultV1::Present { common, .. } = &mut candidate {
            mutate(common);
        }
        assert!(verify_point_read_v1(7, request, header, &candidate).is_err());
    }
}

fn assert_compiled_proof_mutations(
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    package: &PointReadResultV1,
) {
    let (shard_proof, catalog_proof, siblings) = match package {
        PointReadResultV1::Present { evidence, .. } => (
            evidence.shard_smt_proof.clone(),
            evidence.root_catalog_proof.clone(),
            evidence.shard_top_siblings,
        ),
        _ => unreachable!(),
    };
    for (is_catalog, proof) in [(false, shard_proof), (true, catalog_proof)] {
        assert_one_compiled_proof_mutations(request, header, package, is_catalog, &proof);
    }
    for level in 0..siblings.len() {
        let mut candidate = (*package).clone();
        if let PointReadResultV1::Present { evidence, .. } = &mut candidate {
            evidence.shard_top_siblings[level] = B256::repeat_byte(0x82 + level as u8);
        }
        assert!(verify_point_read_v1(7, request, header, &candidate).is_err());
    }
}

fn assert_result_and_schema_mutations(
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    package: &PointReadResultV1,
) {
    let (shard_proof, catalog_proof, siblings) = match package {
        PointReadResultV1::Present { evidence, .. } => (
            evidence.shard_smt_proof.clone(),
            evidence.root_catalog_proof.clone(),
            evidence.shard_top_siblings,
        ),
        _ => unreachable!(),
    };
    let common = match package {
        PointReadResultV1::Present { common, .. } => common.clone(),
        _ => unreachable!(),
    };
    let wrong_result_variants = [
        PointReadResultV1::Unavailable,
        PointReadResultV1::Absent {
            common: common.clone(),
            evidence: AbsentEvidenceV1::CollectionAbsent {
                root_catalog_proof: catalog_proof.clone(),
            },
        },
        PointReadResultV1::Absent {
            common,
            evidence: AbsentEvidenceV1::EntityAbsentInCollection {
                shard_smt_proof: shard_proof,
                shard_top_siblings: siblings,
                root_catalog_proof: catalog_proof,
            },
        },
    ];
    for candidate in wrong_result_variants {
        assert!(verify_point_read_v1(7, request, header, &candidate).is_err());
    }

    let stored = crate::decode_stored_body(match package {
        PointReadResultV1::Present { body_bytes, .. } => body_bytes,
        _ => unreachable!(),
    })
    .unwrap();
    let wrong_schema = StoredBody::new(stored.schema_version() + 1, stored.payload().to_vec())
        .unwrap()
        .encode();
    let mut candidate = (*package).clone();
    if let PointReadResultV1::Present { body_bytes, .. } = &mut candidate {
        *body_bytes = wrong_schema.into();
    }
    assert!(verify_point_read_v1(7, request, header, &candidate).is_err());
}

fn assert_header_mutations(
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    package: &PointReadResultV1,
    marker: &FinalizedMarker,
) {
    let mut bad_headers = Vec::new();
    let mut wrong_number = (*header).clone();
    wrong_number.block_number += 1;
    bad_headers.push(wrong_number);
    let mut wrong_hash = (*header).clone();
    wrong_hash.block_hash = B256::repeat_byte(0x83);
    bad_headers.push(wrong_hash);
    let mut malformed_artifacts = (*header).clone();
    malformed_artifacts.extra_data.push(0);
    bad_headers.push(malformed_artifacts);
    let mut missing_artifact = (*header).clone();
    missing_artifact.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default())
        .unwrap()
        .to_vec();
    bad_headers.push(missing_artifact);
    for (scheme, root) in [
        (ACTIVE_COMMITMENT_SCHEME + 1, marker.new_root),
        (ACTIVE_COMMITMENT_SCHEME, B256::repeat_byte(0x84)),
    ] {
        let mut wrong_artifact = (*header).clone();
        wrong_artifact.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: scheme,
                r_sealed: root,
            }),
            ..Default::default()
        })
        .unwrap()
        .to_vec();
        bad_headers.push(wrong_artifact);
    }
    for candidate_header in bad_headers {
        assert!(verify_point_read_v1(7, request, &candidate_header, package).is_err());
    }
}

fn assert_one_compiled_proof_mutations(
    request: PointReadRequestV1,
    header: &SelectedHeaderV1,
    package: &PointReadResultV1,
    is_catalog: bool,
    proof: &CkbCompiledProofV1,
) {
    for byte in 0..proof.0.len() {
        let mut candidate = (*package).clone();
        if let PointReadResultV1::Present { evidence, .. } = &mut candidate {
            let target = if is_catalog {
                &mut evidence.root_catalog_proof
            } else {
                &mut evidence.shard_smt_proof
            };
            let mut bytes = target.0.to_vec();
            bytes[byte] ^= 1;
            target.0 = bytes.into();
        }
        assert!(
            verify_point_read_v1(7, request, header, &candidate).is_err(),
            "every byte of each compiled proof is authenticated: catalog={is_catalog}, byte={byte}"
        );
    }
    for malformed in [proof.0[..proof.0.len() - 1].to_vec(), {
        let mut trailing = proof.0.to_vec();
        trailing.push(0);
        trailing
    }] {
        let mut candidate = (*package).clone();
        if let PointReadResultV1::Present { evidence, .. } = &mut candidate {
            if is_catalog {
                evidence.root_catalog_proof.0 = malformed.into();
            } else {
                evidence.shard_smt_proof.0 = malformed.into();
            }
        }
        assert!(verify_point_read_v1(7, request, header, &candidate).is_err());
    }
}
