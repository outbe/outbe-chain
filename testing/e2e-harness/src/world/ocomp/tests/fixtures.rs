use super::*;

pub(super) fn launch_identity_evidence(
    activation_height: u64,
    fork_install_hash: B256,
) -> OcompLaunchIdentityEvidenceV1 {
    OcompLaunchIdentityEvidenceV1 {
        chain_id: 1,
        genesis_hash: format!("{:#x}", B256::repeat_byte(9)),
        protocol_bundle_hash: format!("{:#x}", B256::repeat_byte(8)),
        fork_install_hash: format!("{fork_install_hash:#x}"),
        classification: "final".to_owned(),
        activation_height,
        metadosis_storage_layout_hash: METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX.to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn completed_job_topology() -> TestTopology {
    let mut topology = topology();
    topology.launch_identity = Some(OcompLaunchIdentityV1 {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x09),
        protocol_bundle_hash: B256::repeat_byte(0x08),
        fork_install_hash: B256::repeat_byte(0x07),
        classification: OcompForkInstallClassification::Final,
        activation_height: 1,
        metadosis_storage_layout_hash: METADOSIS_STORAGE_LAYOUT_V1_HASH,
    });
    topology
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn stage_completed_job_footprint(topology: &OcompTopology, job_id: B256) {
    let job_component = hex::encode(job_id);
    let bundle_hash = topology
        .launch_identity
        .expect("completed-job fixture has a launch identity")
        .protocol_bundle_hash;
    for validator_index in topology.validator_indices().unwrap() {
        let root = topology.domain_root(validator_index).unwrap();
        let admissions = root
            .join("supervisor-v1")
            .join("jobs")
            .join(&job_component)
            .join("admissions");
        let worker_outputs = root
            .join("worker-inbox-v1")
            .join(hex::encode(bundle_hash))
            .join("artifacts");
        let votes = root
            .join("supervisor-v1")
            .join("vote-submissions")
            .join(&job_component);
        fs::create_dir_all(&admissions).unwrap();
        fs::create_dir_all(&worker_outputs).unwrap();
        fs::create_dir_all(&votes).unwrap();
        fs::write(admissions.join("0000000000.admission"), b"same-admission").unwrap();
        fs::write(worker_outputs.join("unit.ocb1"), b"same-worker-output").unwrap();
        fs::write(
            votes.join(format!("{job_component}.vote.v1")),
            [validator_index],
        )
        .unwrap();
    }
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn canonical_artifact_fixture(
) -> (TestTopology, OcompCanonicalArtifactProof, BTreeMap<u8, u32>) {
    use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
    use outbe_ocomp_protocol::{
        hash::hash_framed,
        intent::DayType,
        result::{
            lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
            CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisResultV1,
            MetadosisCompletionSummaryV1, ResultRootsV1,
        },
        HashDomain,
    };

    let topology = completed_job_topology();
    let bundle_hash = topology.launch_identity.unwrap().protocol_bundle_hash;
    // A valid one-Tribute, zero-value result, using the same conservation
    // shape as the production result-attestation tests. No runtime output
    // or scenario directory is needed by these filesystem regressions.
    let mut result = LysisResultV1 {
        protocol_bundle_hash: bundle_hash,
        job_id: B256::repeat_byte(0x42),
        attempt: 0,
        input_manifest_hash: B256::repeat_byte(1),
        plan_hash: B256::repeat_byte(2),
        unit_artifact_root: B256::repeat_byte(3),
        fidelity_fraction_root: B256::repeat_byte(4),
        gratis_prefix_root: B256::repeat_byte(5),
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(6),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 7,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: 7,
            pending_nonce: 0,
            day_type: DayType::Green,
            tribute_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            status: CompletionStatus::Completed,
            logical_evaluation_height: 100,
            logical_evaluation_time: 1_000,
        },
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        roots: ResultRootsV1 {
            nod_root: B256::repeat_byte(10),
            bucket_root: B256::repeat_byte(11),
            contributor_root: B256::repeat_byte(12),
            output_manifest_root: B256::repeat_byte(13),
        },
        counts: ExactCountsV1 {
            tribute_count: 1,
            nod_count: 1,
            bucket_count: 0,
            contributor_count: 0,
            semantic_event_count: 0,
        },
        conservation: ConservationTotalsV1 {
            tribute_nominal_total: U256::ZERO,
            eligible_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            nod_cost_total: U256::ZERO,
        },
        arithmetic_commitment: B256::ZERO,
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    };
    let limits = poc_schema_limits();
    result.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &result
            .arithmetic_summary()
            .encode_canonical(&limits)
            .unwrap(),
    )
    .unwrap();
    let bytes = result.encode_canonical(&limits).unwrap();
    assert_eq!(
        LysisResultV1::decode_canonical(&bytes, &limits).unwrap(),
        result
    );
    stage_completed_job_footprint(&topology, result.job_id);
    let pids = topology
        .validator_indices()
        .unwrap()
        .into_iter()
        .map(|index| (index, 1_000 + u32::from(index)))
        .collect::<BTreeMap<_, _>>();
    for &index in pids.keys() {
        fs::write(
            topology
                .cfg
                .validator_dir(usize::from(index))
                .join("node.log"),
            b"",
        )
        .unwrap();
        let cas = FilesystemCas::open(
            topology.domain_root(index).unwrap().join("cas-v1"),
            CasWriterRole::Supervisor,
            CasLimits {
                max_object_bytes: bytes.len() as u64,
                max_total_bytes: u64::MAX,
            },
        )
        .unwrap();
        cas.publish_bytes(&bytes).unwrap();
    }
    let proof = OcompCanonicalArtifactProof {
        checkpoint: crate::world::rpc::FinalizedCheckpoint {
            height: 100,
            block_hash: B256::repeat_byte(90),
            state_root: B256::repeat_byte(91),
        },
        bundle_hash,
        result,
        voters: vec![0, 1, 2],
    };
    (topology, proof, pids)
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn artifact_fixture_vote_path(
    topology: &OcompTopology,
    index: u8,
    job: B256,
) -> PathBuf {
    let component = hex::encode(job);
    topology
        .domain_root(index)
        .unwrap()
        .join("supervisor-v1/vote-submissions")
        .join(&component)
        .join(format!("{component}.vote.v1"))
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn append_artifact_fixture_log(topology: &OcompTopology, index: u8, line: &str) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(
            topology
                .cfg
                .validator_dir(usize::from(index))
                .join("node.log"),
        )
        .unwrap();
    writeln!(file, "INFO outbe_chain::ocomp_exex: {line}").unwrap();
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn prepare_measurement_genesis_fixture(topology: &OcompTopology) {
    std::fs::create_dir_all(&topology.cfg.dir).unwrap();
    let spec = reth_chainspec::ChainSpec::<OutbeHeader>::default();
    let mut genesis = serde_json::to_value(&spec.genesis).unwrap();
    genesis["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY] =
        serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS);
    std::fs::write(
        topology.cfg.dir.join("genesis.json"),
        serde_json::to_vec_pretty(&genesis).unwrap(),
    )
    .unwrap();
    let validators = (0..4_u8)
        .map(|index| {
            serde_json::json!({
                "address": format!("{:#x}", Address::with_last_byte(index + 1)),
                "public_key": format!("0x{}", hex::encode([index + 11; 48])),
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        topology.cfg.dir.join("validators.json"),
        serde_json::to_vec_pretty(&validators).unwrap(),
    )
    .unwrap();
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn prepare_public_measurement_genesis_fixture(topology: &OcompTopology) {
    prepare_public_measurement_genesis_fixture_with_vote_window(topology, 120);
}

#[cfg(feature = "ocomp-integration")]
pub(super) fn prepare_public_measurement_genesis_fixture_with_vote_window(
    topology: &OcompTopology,
    vote_window_blocks: u64,
) {
    prepare_measurement_genesis_fixture(topology);
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    genesis["timestamp"] = serde_json::Value::String(format!("0x{now:x}"));
    genesis["config"][GENESIS_CONFIG_KEY] = serde_json::json!({
        "schemaVersion": 1,
        "metadosis": {
            "formingPeriodSeconds": 60,
            "lookbackDelaySeconds": 0,
            "offeringPeriodSeconds": 120,
            "waitingPeriodSeconds": 30,
            "bootstrapDurationSeconds": 300,
            "advanceIntervalSeconds": 10
        },
        "ocomp": { "computeVoteWindowBlocks": vote_window_blocks }
    });
    genesis["alloc"][format!("{METADOSIS_ADDRESS:#x}")] = serde_json::json!({
        "balance": "0x0",
        "code": "0xef",
        "storage": {},
    });
    let worldwide_day = WorldwideDay::from_timestamp(now);
    let day_pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let mut oracle_config = outbe_oracle::genesis::OracleGenesisConfig::default_config();
    oracle_config.initial_rates = vec![(
        day_pair.address1(),
        day_pair.address2(),
        U256::from(1_000_000_u64),
    )];
    oracle_config.scurve_entries = vec![outbe_oracle::genesis::GenesisScurveEntry {
        base: day_pair.address1(),
        quote: day_pair.address2(),
        peak_day: worldwide_day.to_timestamp_utc(),
        peak_price: U256::from(1_000_000_u64),
    }];
    let mut oracle_provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut oracle_provider, |storage| {
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
        outbe_oracle::genesis::init_from_genesis(&mut oracle, &oracle_config)
    })
    .unwrap();
    let oracle_storage = oracle_provider
        .storage
        .into_iter()
        .filter(|((address, _), value)| {
            *address == outbe_primitives::addresses::ORACLE_ADDRESS && !value.is_zero()
        })
        .map(|((_, slot), value)| {
            (
                format!("0x{slot:064x}"),
                serde_json::Value::String(format!("0x{value:064x}")),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    genesis["alloc"][format!("{:#x}", outbe_primitives::addresses::ORACLE_ADDRESS)] = serde_json::json!({
        "balance": "0x0",
        "code": "0xef",
        "storage": oracle_storage,
    });
    genesis["alloc"][format!("{TRIBUTE_ADDRESS:#x}")] = serde_json::json!({
        "balance": "0x0",
        "code": "0xef",
        "storage": {},
    });
    std::fs::write(genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
}
