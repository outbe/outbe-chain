use super::*;

#[test]
fn binding_id_is_exact_and_nonzero() {
    assert!(parse_nonzero_b256(&"11".repeat(32), "binding").is_ok());
    assert!(parse_nonzero_b256(&"00".repeat(32), "binding").is_err());
    assert!(parse_nonzero_b256(&"11".repeat(31), "binding").is_err());
}

#[test]
fn validator_node_binding_uses_the_global_transaction_signer() {
    let signer = crate::tx::TxSigner::new(
        "11d7b7a4b68f4f6a9f4ec50a4f3b1e6f6294e46147e37030262830716725f9a3",
    )
    .unwrap();
    let (binding, binding_hash, signature) = authorize_validator_node_binding(
        U256::from(676_u64).to_be_bytes(),
        B256::repeat_byte(0x31),
        B256::repeat_byte(0x42),
        &signer,
    )
    .unwrap();

    assert_eq!(binding.validator, signer.address().into_array());
    assert_eq!(binding.binding_hash().unwrap(), binding_hash);
    assert!(binding.verify_validator_signature(&signature));
}

#[test]
fn full_node_reth_identity_accepts_equivalent_raw_and_hex_key_files() {
    let directory = tempfile::tempdir().unwrap();
    let secret = [0x61; 32];
    let raw_path = directory.path().join("reth-p2p.raw");
    let hex_path = directory.path().join("reth-p2p.hex");
    fs::write(&raw_path, secret).unwrap();
    fs::write(&hex_path, format!("0x{}\n", hex::encode(secret))).unwrap();

    let raw = load_secp256k1_key_file(&raw_path).unwrap();
    let hex = load_secp256k1_key_file(&hex_path).unwrap();
    let expected = k256::ecdsa::SigningKey::from_bytes((&secret).into()).unwrap();
    assert_eq!(
        raw.verifying_key().to_encoded_point(true),
        expected.verifying_key().to_encoded_point(true)
    );
    assert_eq!(
        hex.verifying_key().to_encoded_point(true),
        expected.verifying_key().to_encoded_point(true)
    );
}

#[test]
fn full_node_reth_identity_rejects_invalid_secret_files() {
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty");
    let wrong_width = directory.path().join("wrong-width.hex");
    let zero = directory.path().join("zero.raw");
    fs::write(&empty, []).unwrap();
    fs::write(&wrong_width, "11".repeat(31)).unwrap();
    fs::write(&zero, [0_u8; 32]).unwrap();

    assert!(load_secp256k1_key_file(&empty).is_err());
    assert!(load_secp256k1_key_file(&wrong_width).is_err());
    assert!(load_secp256k1_key_file(&zero).is_err());
}
