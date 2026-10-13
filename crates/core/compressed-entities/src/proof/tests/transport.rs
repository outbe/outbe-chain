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
        exclude_from_intex_issuance: false,
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
                floor_price_minor: U256::from(14),
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
                "122835a3da15ee4370013c6f90edcaa3bbaa636530d341b6d605d5a68ec889c6"
            ),
            alloy_primitives::b256!(
                "157bbc65cbc812a2d6d3c376f666f245721d40f10abb844589c32b4ffe1886da"
            ),
            alloy_primitives::b256!(
                "a975d4736ef74939d9eca29e329d6ab1aa2aad157cc971d6315e3dc1bdf5fe9b"
            ),
            alloy_primitives::b256!(
                "e3586b76f5dc9808805bc4a705a0fc97426720f556024c1b7ae45163026561b4"
            ),
            alloy_primitives::b256!(
                "89e69ead9380bf582eef3a5eb299e0e7681a18b74fc13f4e802cc715fce3d53c"
            ),
            alloy_primitives::b256!(
                "d4382908cd4da41da05e5df6cbe86c130c0609e7818077c7d38184b87d127748"
            ),
            alloy_primitives::b256!(
                "f4211b9d73a3f44c2411834b39252a3724f3d30fb723448e50b6e85cc267bfd0"
            ),
            alloy_primitives::b256!(
                "e09c0bfe4e83af3f2329c29f7aaea95b72311880d00fb8f92b33071ff87b9bad"
            ),
            alloy_primitives::b256!(
                "c401b4fc8759510f0def36b6a90258d98bb4317347ecc84395a776fd5968b18f"
            ),
            alloy_primitives::b256!(
                "8cbe859a66ffeeb803147f12ca0cb4083705223290cc559c5e20aa3cd73340d2"
            ),
            alloy_primitives::b256!(
                "412ff24f62dc1a8b04104286e8c51b6e58f3aa368ec3011cc77aee38840476f7"
            ),
            alloy_primitives::b256!(
                "44a889b4fe93736c601b2b608dc3d0c85b11f63706157b6c0460888dedb7595c"
            ),
            alloy_primitives::b256!(
                "0cc534b919e4a93be44e461a4ab2af6d3ad4f7a459172727164f098cb8f92e74"
            ),
            alloy_primitives::b256!(
                "69c5fef763649f698c192ef09c01b310ce68928fcc81972024eb1925cf6cf7e6"
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
