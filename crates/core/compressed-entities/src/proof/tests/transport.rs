use super::*;

#[test]
fn present_golden_packages_cover_every_closed_domain() {
    let _guard = proof_test_guard();
    let mut transport_hashes = Vec::new();
    let day = WorldwideDay::new(20_260_717);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x31; 32]);
    let tribute = TributeBodyV1 {
        tribute_id,
        owner: Address::repeat_byte(0x41),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
    };
    let nod_id = WwdEntityId::from_day_and_digest(day, [0x32; 32]);
    let nod = NodItemBodyV1 {
        is_settled: false,
        nod_id,
        owner: Address::repeat_byte(0x42),
        gratis_load_minor: U256::from(1),
        worldwide_day: day,
        league_id: 7,
        bucket_key: B256::repeat_byte(0x43),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 123,
    };
    let (bucket_id, bucket_bytes, bucket_leaf) = bucket_body(0x33);
    let cases = [
        {
            let (bytes, leaf) = stored_body(tribute_id, encode_tribute_v1(&tribute).unwrap());
            (
                CeDomain::Tribute,
                EntityRef::Tribute(tribute_id),
                tribute_id,
                bytes,
                leaf,
            )
        },
        {
            let (bytes, leaf) = stored_body(nod_id, encode_nod_item_v1(&nod).unwrap());
            (
                CeDomain::NodItem,
                EntityRef::NodItem(nod_id),
                nod_id,
                bytes,
                leaf,
            )
        },
        (
            CeDomain::NodBucket,
            EntityRef::NodBucket(bucket_id),
            bucket_id,
            bucket_bytes,
            bucket_leaf,
        ),
        {
            let gem = crate::GemBodyV1 {
                gem_id: WwdEntityId::from([0xff; 32]),
                owner: Address::repeat_byte(0x44),
                gem_type: 1,
                promis_load_minor: U256::from(13),
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 123,
                bucket_key: B256::repeat_byte(0x45),
                state: 0,
                called_at: 0,
                settled_at: 0,
            };
            let id = gem.gem_id;
            let (bytes, leaf) = stored_body(id, crate::encode_gem_v1(&gem).unwrap());
            (CeDomain::Gem, EntityRef::Gem(id), id, bytes, leaf)
        },
        {
            let bucket = crate::GemBucketBodyV1 {
                bucket_key: B256::repeat_byte(0x46),
                worldwide_day: day,
                entry_price_minor: U256::from(15),
                reference_currency: 978,
                settled_gems: 1,
            };
            let id = bucket.entity_id();
            let (bytes, leaf) = stored_body(id, crate::encode_gem_bucket_v1(&bucket).unwrap());
            (
                CeDomain::GemBucket,
                EntityRef::GemBucket(id),
                id,
                bytes,
                leaf,
            )
        },
    ];
    for (domain, entity, id, body, leaf) in cases {
        let (_dir, node, genesis_hash) = service();
        let (_, header) = finalize_one(&node, genesis_hash, entity, leaf);
        let request = PointReadRequestV1 {
            domain_id: domain.id(),
            raw_id: id,
        };
        let package = node
            .serve_point_read_v1(7, request, |_, _| Some(header.clone()), |_, _| Some(body))
            .unwrap();
        assert_eq!(
            verify_point_read_v1(7, request, &header, &package).unwrap(),
            VerifiedPointReadV1::Present
        );
        let json = serde_json::to_string(&package).unwrap();
        transport_hashes.push(alloy_primitives::keccak256(json.as_bytes()));
        assert!(json.contains("\"present\""));
        assert!(json.contains("\"body_bytes\":\"0x"));
        assert!(json.contains("\"shard_smt_proof\":\"0x"));
        assert_eq!(
            serde_json::from_str::<PointReadResultV1>(&json).unwrap(),
            package
        );

        let missing_id = WwdEntityId::from_day_and_digest(id.worldwide_day(), [0x99; 32]);
        let missing_request = PointReadRequestV1 {
            domain_id: domain.id(),
            raw_id: missing_id,
        };
        let entity_absent = node
            .serve_point_read_v1(
                7,
                missing_request,
                |_, _| Some(header.clone()),
                |_, _| panic!("authenticated entity absence must not read Mongo"),
            )
            .unwrap();
        assert!(matches!(
            entity_absent,
            PointReadResultV1::Absent {
                evidence: AbsentEvidenceV1::EntityAbsentInCollection { .. },
                ..
            }
        ));
        assert_eq!(
            verify_point_read_v1(7, missing_request, &header, &entity_absent).unwrap(),
            VerifiedPointReadV1::Absent
        );
        transport_hashes.push(alloy_primitives::keccak256(
            serde_json::to_string(&entity_absent).unwrap().as_bytes(),
        ));

        let (_empty_dir, empty_service, empty_genesis) = service();
        let empty_header = finalize_empty(&empty_service, empty_genesis);
        let collection_absent = empty_service
            .serve_point_read_v1(
                7,
                request,
                |_, _| Some(empty_header.clone()),
                |_, _| panic!("authenticated collection absence must not read Mongo"),
            )
            .unwrap();
        assert!(matches!(
            collection_absent,
            PointReadResultV1::Absent {
                evidence: AbsentEvidenceV1::CollectionAbsent { .. },
                ..
            }
        ));
        assert_eq!(
            verify_point_read_v1(7, request, &empty_header, &collection_absent).unwrap(),
            VerifiedPointReadV1::Absent
        );
        transport_hashes.push(alloy_primitives::keccak256(
            serde_json::to_string(&collection_absent)
                .unwrap()
                .as_bytes(),
        ));
    }
    assert_eq!(
        transport_hashes,
        [
            alloy_primitives::b256!(
                "2243c4faf905de6b36a93fc9b8ed9304ed6f8c196b1ff1630627050694ed475f"
            ),
            alloy_primitives::b256!(
                "e96907502dd57735fa9b4cd2ef96667ef5d003d0be6cb3ac5e8270ff1df6acd6"
            ),
            alloy_primitives::b256!(
                "a975d4736ef74939d9eca29e329d6ab1aa2aad157cc971d6315e3dc1bdf5fe9b"
            ),
            alloy_primitives::b256!(
                "d093bdb2f07dd1e6dc521d72e023373b1ff7cd3abb56161cece628603db6167d"
            ),
            alloy_primitives::b256!(
                "52635bacaffd36df9aefffeff2b52fa58919a646f682fa78011999dff8db8b8c"
            ),
            alloy_primitives::b256!(
                "d4382908cd4da41da05e5df6cbe86c130c0609e7818077c7d38184b87d127748"
            ),
            alloy_primitives::b256!(
                "2c90c1c2a7a101694eefbef18d3bc790a5ad76eb7dcab4667cdf29e8f6f4c72a"
            ),
            alloy_primitives::b256!(
                "c6ece55406f799b0747036035d5d80c01ca975a57243a60058b12c217d3dc0f0"
            ),
            alloy_primitives::b256!(
                "c401b4fc8759510f0def36b6a90258d98bb4317347ecc84395a776fd5968b18f"
            ),
            alloy_primitives::b256!(
                "ff2fc5f57debedf4ae1068b91a1644fc1dd8ecc84e0555192fc11170e28c9901"
            ),
            alloy_primitives::b256!(
                "ee5ebceab732fe14b47e847a89167165ad6ef688c32014d3b01a8aa39a9737c2"
            ),
            alloy_primitives::b256!(
                "44a889b4fe93736c601b2b608dc3d0c85b11f63706157b6c0460888dedb7595c"
            ),
            alloy_primitives::b256!(
                "a009dae7d6cad0bbc08de61be8b583b5c017e00367869fa22c158e0b674a9efc"
            ),
            alloy_primitives::b256!(
                "38e10cf7025313fada39f7ee323f222e82d1b60a4bb2c248720a10c6119a4a67"
            ),
            alloy_primitives::b256!(
                "7f7386edd6b2911dbb64b0619a4ed06ad58786cca082bed0881ebf587e0b57ef"
            ),
        ],
        "exact present/entity-absent/collection-absent JSON transports for all domains"
    );
}

#[test]
fn json_request_and_decoder_bounds_are_pinned() {
    let _guard = proof_test_guard();
    let id = WwdEntityId::from_day_and_digest(WorldwideDay::new(20_260_717), [0x55; 32]);
    let request = PointReadRequestV1 {
        domain_id: 2,
        raw_id: id,
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        format!(
            "{{\"domain_id\":2,\"raw_id\":\"0x{}\"}}",
            hex::encode(id.as_slice())
        )
    );
    assert_eq!(
        serde_json::from_str::<PointReadRequestV1>(&serde_json::to_string(&request).unwrap())
            .unwrap(),
        request
    );
    assert!(
        serde_json::from_str::<PointReadRequestV1>("{\"domain_id\":2,\"raw_id\":\"55\"}").is_err()
    );
    assert!(
        serde_json::from_value::<CkbCompiledProofV1>(serde_json::json!(format!(
            "0x{}",
            "00".repeat(MAX_COMPILED_PROOF_BYTES + 1)
        )))
        .is_err()
    );
    let oversized = serde_json::json!({
        "present": {
            "common": {
                "proof_encoding_version": 1,
                "chain_id": 7,
                "block_number": 1,
                "block_hash": B256::ZERO,
                "domain_id": 2,
                "raw_id": format!("0x{}", hex::encode(id.as_slice()))
            },
            "body_bytes": format!("0x{}", "00".repeat(MAX_CANONICAL_BODY_BYTES + 1)),
            "evidence": {
                "shard_smt_proof": "0x01",
                "shard_top_siblings": vec![B256::ZERO; 8],
                "root_catalog_proof": "0x01"
            }
        }
    });
    assert!(serde_json::from_value::<PointReadResultV1>(oversized).is_err());
}
