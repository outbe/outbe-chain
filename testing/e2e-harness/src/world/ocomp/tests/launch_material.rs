use super::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn release_ocomp_layout_uses_the_scenario_base_and_never_the_debug_override() {
    let mut command = Command::new("outbe-ocomp");
    configure_release_layout(&mut command, Path::new("/tmp/release-e2e"), 7);
    configure_snapshot_exporter_command(&mut command, "127.0.0.1:30471".parse().unwrap());

    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        vec![
            "snapshot-exporter",
            "--supervisor-address",
            "127.0.0.1:30471"
        ]
    );
    assert!(!args.iter().any(|arg| arg == "--development-root"));

    let environment = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        environment.get(OCOMP_BASE_PATH_ENV),
        Some(&Some("/tmp/release-e2e".to_owned()))
    );
    assert_eq!(
        environment.get(OCOMP_VALIDATOR_INDEX_ENV),
        Some(&Some("7".to_owned()))
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn snapshot_exporter_uses_the_exact_node_projection_identity() {
    let topology = topology_with_validators(4);
    let validator_index = 2;
    let mut command = Command::new("outbe-ocomp");

    crate::world::projection::ensure_node_config(&topology.cfg, validator_index).unwrap();
    configure_snapshot_exporter_projection(&mut command, &topology.cfg, validator_index).unwrap();

    let environment = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        environment.get("OUTBE_OCOMP_STORAGE_CONFIG"),
        Some(&Some(
            topology
                .cfg
                .projection_storage_config(validator_index)
                .display()
                .to_string()
        ))
    );
    assert_eq!(environment.len(), 1);
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn staged_joiner_domain_uses_its_registration_key_and_the_pinned_bundle() {
    let topology = topology_with_validators(4);
    let founder_bundle = topology
        .domain_root(0)
        .unwrap()
        .join("protocol-bundle-v1.ocb1");
    fs::create_dir_all(founder_bundle.parent().unwrap()).unwrap();
    fs::write(&founder_bundle, b"pinned-bundle").unwrap();
    let joiner_key = topology.cfg.validator_dir(4).join("ocomp-key-v1.hex");
    fs::create_dir_all(joiner_key.parent().unwrap()).unwrap();
    fs::write(&joiner_key, b"joiner-registration-secret\n").unwrap();

    topology.stage_joiner_domain_material(4).unwrap();

    let staged = topology
        .cfg
        .validator_dir(4)
        .join("ocomp")
        .join("domain-v1");
    assert_eq!(
        fs::read(staged.join("protocol-bundle-v1.ocb1")).unwrap(),
        b"pinned-bundle"
    );
    assert_eq!(
        fs::read(staged.join("ocomp-key-v1.hex")).unwrap(),
        b"joiner-registration-secret\n"
    );
    let operational_key = fs::read(staged.join("ocomp-evm-key.hex")).unwrap();
    assert_eq!(operational_key.len(), 65);
    assert_eq!(operational_key[64], b'\n');
    assert!(operational_key[..64]
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)));
    assert!(topology.domain_root(4).is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn direct_joiner_domain_uses_validator_evm_fallback_without_a_delegate() {
    let topology = topology_with_validators(4);
    let founder_bundle = topology
        .domain_root(0)
        .unwrap()
        .join("protocol-bundle-v1.ocb1");
    fs::create_dir_all(founder_bundle.parent().unwrap()).unwrap();
    fs::write(&founder_bundle, b"pinned-bundle").unwrap();
    let joiner = topology.cfg.validator_dir(4);
    fs::create_dir_all(&joiner).unwrap();
    fs::write(
        joiner.join("ocomp-key-v1.hex"),
        b"joiner-registration-secret\n",
    )
    .unwrap();
    let validator_evm_key = format!("0x{}\n", "ab".repeat(32));
    fs::write(joiner.join("evm-key.hex"), validator_evm_key).unwrap();

    stage_direct_joiner_domain_material(&topology.cfg, 4).unwrap();

    let staged = joiner.join("ocomp").join("domain-v1");
    assert_eq!(
        fs::read(staged.join("protocol-bundle-v1.ocb1")).unwrap(),
        b"pinned-bundle"
    );
    assert_eq!(
        fs::read(staged.join("ocomp-key-v1.hex")).unwrap(),
        b"joiner-registration-secret\n"
    );
    assert_eq!(
        fs::read(staged.join("ocomp-evm-key.hex")).unwrap(),
        format!("{}\n", "ab".repeat(32)).as_bytes()
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn cold_history_catalog_contains_both_public_bundles_without_runtime_state() {
    let mut topology = topology_with_validators(4);
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    topology.launch_identity = Some(prepared.launch_identity());
    let first = prepared.install.protocol_bundle.clone();
    let first_bytes = first.encode_canonical(&poc_schema_limits()).unwrap();
    publish_bundle_catalog_entry(
        topology.domain_root(0).unwrap(),
        prepared.launch_identity().protocol_bundle_hash,
        &first_bytes,
    )
    .unwrap();
    let mut second = first;
    second.protocol_version += 1;
    second.fork_id = B256::repeat_byte(0xa1);
    let successor = topology.stage_successor_bundle(&second).unwrap();
    topology.successor_identity = Some(successor);
    topology.stage_cold_history_follower_bundles(14).unwrap();
    let node = topology.cfg.validator_dir(14);
    let root = node.join("ocomp/domain-v1");
    assert_eq!(
        fs::read(root.join("protocol-bundle-v1.ocb1")).unwrap(),
        first_bytes
    );
    assert_eq!(
        fs::read_dir(root.join("protocol-bundles-v1"))
            .unwrap()
            .count(),
        2
    );
    for path in ["data", "node.log"] {
        assert!(!node.join(path).exists());
    }
    for path in [
        "cas-v1",
        "node-v1",
        "supervisor-v1",
        "exporter-v1",
        "ocomp-key-v1.hex",
        "ocomp-evm-key.hex",
    ] {
        assert!(!root.join(path).exists());
    }
    assert!(topology.stage_cold_history_follower_bundles(14).is_err());
    assert!(topology.stage_cold_history_follower_bundles(0).is_err());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn cold_history_staging_rejects_existing_data_and_dangling_links() {
    let topology = topology_with_validators(4);
    let node = topology.cfg.validator_dir(14);
    fs::create_dir_all(node.join("data")).unwrap();
    assert!(topology
        .stage_cold_history_follower_bundles(14)
        .unwrap_err()
        .to_string()
        .contains("not cold"));
    assert!(!node.join("ocomp").exists());
    let other = topology.cfg.validator_dir(15);
    fs::create_dir_all(&other).unwrap();
    std::os::unix::fs::symlink(other.join("missing"), other.join("data")).unwrap();
    assert!(topology
        .stage_cold_history_follower_bundles(15)
        .unwrap_err()
        .to_string()
        .contains("not cold"));
    assert!(!other.join("ocomp").exists());
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn bootstrapped_runtime_preserves_the_exact_genesis_result_signing_keys() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let canonical_bundle = prepared
        .install
        .protocol_bundle
        .encode_canonical(&limits)
        .unwrap();
    fs::write(
        topology.cfg.dir.join("protocol-bundle-v1.ocb1"),
        canonical_bundle,
    )
    .unwrap();

    let mut expected_keys = Vec::new();
    for (index, registration) in prepared.install.founder_registrations.iter().enumerate() {
        let domain_key = fs::read(
            topology
                .domain_root(u8::try_from(index).unwrap())
                .unwrap()
                .join("ocomp-key-v1.hex"),
        )
        .unwrap();
        expected_keys.push(domain_key.clone());
        fs::write(
            topology.cfg.validator_dir(index).join("ocomp-key-v1.hex"),
            domain_key,
        )
        .unwrap();
        fs::write(
            topology
                .cfg
                .validator_dir(index)
                .join("ocomp-registration-v1.ocb1"),
            registration.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        fs::remove_dir_all(topology.domain_root(u8::try_from(index).unwrap()).unwrap()).unwrap();
    }

    topology
        .ensure_validator_domain_material_before_node_start()
        .unwrap();
    let identity = topology.prepare_bootstrapped_runtime().unwrap();
    assert_eq!(identity, prepared.launch_identity());
    for (index, expected_key) in expected_keys.iter().enumerate() {
        assert_eq!(
            fs::read(
                topology
                    .domain_root(u8::try_from(index).unwrap())
                    .unwrap()
                    .join("ocomp-key-v1.hex")
            )
            .unwrap(),
            *expected_key
        );
    }
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn bootstrapped_runtime_rejects_a_key_substituted_after_genesis() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    let prepared = topology.prepare_measurement_fork_install().unwrap();
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    fs::write(
        topology.cfg.dir.join("protocol-bundle-v1.ocb1"),
        prepared
            .install
            .protocol_bundle
            .encode_canonical(&limits)
            .unwrap(),
    )
    .unwrap();
    fs::write(
        topology
            .cfg
            .validator_dir(0)
            .join("ocomp-registration-v1.ocb1"),
        prepared.install.founder_registrations[0]
            .encode_canonical(&limits)
            .unwrap(),
    )
    .unwrap();
    let substituted = SigningKey::from_bytes((&[99_u8; 32]).into()).unwrap();
    fs::write(
        topology.cfg.validator_dir(0).join("ocomp-key-v1.hex"),
        format!("{}\n", hex::encode(substituted.to_bytes())),
    )
    .unwrap();
    fs::remove_dir_all(topology.domain_root(0).unwrap()).unwrap();

    let error = topology.prepare_bootstrapped_runtime().unwrap_err();
    assert!(error
        .to_string()
        .contains("result-signing key does not match its genesis registration"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn node_start_rejects_partially_staged_validator_domain_material() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    topology.prepare_measurement_fork_install().unwrap();
    fs::remove_file(topology.domain_root(0).unwrap().join("ocomp-evm-key.hex")).unwrap();

    let error = topology
        .ensure_validator_domain_material_before_node_start()
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("partial OCOMP validator domain material"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn measurement_fork_install_rejects_layout_mismatch_before_node_start() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    topology.prepare_measurement_fork_install().unwrap();
    let genesis_path = topology.cfg.dir.join("genesis.json");
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
    genesis["config"][outbe_node::ocomp::fork::METADOSIS_STORAGE_LAYOUT_GENESIS_KEY]
        ["layoutHash"] = serde_json::json!(alloy_primitives::B256::repeat_byte(0x44));
    replace_json_atomically(&genesis_path, &genesis).unwrap();

    let mismatched = parse_outbe_chain_spec(&genesis_path).unwrap();
    let error =
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&mismatched).unwrap_err();
    assert!(error.to_string().contains("layout hash mismatch"));
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn mismatched_fork_manifest_is_valid_but_has_a_distinct_install_identity() {
    let topology = topology();
    prepare_measurement_genesis_fixture(&topology);
    let canonical = topology.prepare_measurement_fork_install().unwrap();
    let mismatched = topology.prepare_mismatched_fork_manifest(0).unwrap();

    assert_eq!(mismatched.canonical_install_hash, canonical.install_hash);
    assert_ne!(mismatched.mismatched_install_hash, canonical.install_hash);
    let canonical_spec = parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
    let mismatched_spec = parse_outbe_chain_spec(&mismatched.path).unwrap();
    assert_eq!(
        canonical_spec.genesis_hash(),
        mismatched_spec.genesis_hash()
    );
    let loaded =
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&mismatched_spec).unwrap();
    assert_eq!(
        loaded.activation_height,
        OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
    );
    assert_ne!(
        loaded.request_profile.source_availability_policy_id,
        canonical
            .install
            .request_profile
            .source_availability_policy_id
    );
}
