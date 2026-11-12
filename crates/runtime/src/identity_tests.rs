#[test]
fn full_node_identity_uses_reth_secret_resolver_and_persists_exact_key() {
    let root = tempfile::tempdir().unwrap();
    let explicit_secret = root.path().join("operator-p2p.key");
    let unused_default = root.path().join("default-discovery-secret");
    let network = reth_node_core::args::NetworkArgs {
        p2p_secret_key: Some(explicit_secret.clone()),
        ..Default::default()
    };

    let (first_signer, first_public) =
        super::load_reth_p2p_node_host_signer(&network, unused_default.clone()).unwrap();
    assert!(explicit_secret.is_file());
    assert!(!unused_default.exists());
    assert_eq!(
        first_signer
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes(),
        first_public
    );

    drop(first_signer);
    let (_, restored_public) =
        super::load_reth_p2p_node_host_signer(&network, unused_default).unwrap();
    assert_eq!(restored_public, first_public);
}
