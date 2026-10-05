use alloy_primitives::{Address, B256};
use outbe_primitives::signer::{OutbeEvmSigner, SharedOutbeEvmSigner, SignerError};
use std::sync::Arc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const fn const_address(signer: &OutbeEvmSigner) -> Address {
    signer.address()
}

#[test]
fn cloned_and_shared_signers_preserve_identity_and_fixed_signature() -> TestResult {
    let original = OutbeEvmSigner::from_secret_bytes([1; 32])?;
    let clone = original.clone();
    let shared: SharedOutbeEvmSigner = Arc::new(clone);
    let expected = "85374ecb6e0ea7cb84429448bf06ca12ea17d9ff80d8fe910f25f2f4fead5b3442c30ba1656842a610ff1da4bf1823604bf15d3493b9043af65b7dced9bbed4400";
    for signer in [&original, shared.as_ref()] {
        assert_eq!(const_address(signer), original.address());
        assert_eq!(
            hex::encode(signer.sign_hash(&B256::with_last_byte(42))?),
            expected
        );
        let debug = format!("{signer:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&"01".repeat(32)));
    }
    Ok(())
}

#[test]
fn in_memory_hex_constructor_preserves_prefix_case_and_whitespace_rules() -> TestResult {
    let expected = OutbeEvmSigner::from_secret_bytes([0xab; 32])?;
    for encoded in ["ab".repeat(32), format!(" \t0x{}\n", "AB".repeat(32))] {
        let signer = OutbeEvmSigner::from_hex(&encoded)?;
        assert_eq!(signer.address(), expected.address());
        assert_eq!(
            signer.sign_hash(&B256::ZERO)?,
            expected.sign_hash(&B256::ZERO)?
        );
    }
    Ok(())
}

#[test]
fn in_memory_constructors_preserve_error_variants() {
    assert!(matches!(
        OutbeEvmSigner::from_secret_bytes([0; 32]),
        Err(SignerError::InvalidSecret(_))
    ));
    assert!(matches!(
        OutbeEvmSigner::from_secret_bytes([0xff; 32]),
        Err(SignerError::InvalidSecret(_))
    ));
    assert!(matches!(
        OutbeEvmSigner::from_hex("0x1234"),
        Err(SignerError::InvalidSecretLength { len: 2 })
    ));
    assert!(matches!(
        OutbeEvmSigner::from_hex("invalid"),
        Err(SignerError::InvalidHex(_))
    ));
}
