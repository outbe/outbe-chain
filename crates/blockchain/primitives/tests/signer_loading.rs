#![cfg(unix)]

use alloy_primitives::{B256, address};
use outbe_primitives::signer::{OutbeEvmSigner, SignerError};
use std::{
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const KEY_ONE: &[u8] = b"0000000000000000000000000000000000000000000000000000000000000001";

fn key_file(encoded: &[u8], mode: u32) -> TestResult<(tempfile::TempDir, PathBuf, u32)> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("evm-key.hex");
    std::fs::write(&path, encoded)?;
    set_mode(&path, mode)?;
    let owner = std::fs::metadata(&path)?.uid();
    Ok((root, path, owner))
}

fn set_mode(path: &Path, mode: u32) -> TestResult {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn strict(path: &Path, owner: u32) -> Result<OutbeEvmSigner, SignerError> {
    outbe_primitives::signer::load::from_strict_file(path, owner)
}

fn permissive(path: &Path) -> Result<OutbeEvmSigner, SignerError> {
    outbe_primitives::signer::load::from_file(path)
}

fn assert_strict_encoded_address(encoded: &[u8]) -> TestResult {
    let (_root, path, owner) = key_file(encoded, 0o600)?;
    assert_eq!(
        strict(&path, owner)?.address(),
        address!("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
    );
    Ok(())
}

fn assert_unsafe(result: Result<OutbeEvmSigner, SignerError>, path: &Path, reason: &str) {
    assert!(matches!(result,
        Err(SignerError::UnsafeKeyFile { path: actual, reason: actual_reason })
        if actual == path && actual_reason == reason
    ));
}

#[test]
fn strict_role_key_loader_enforces_canonical_owner_bound_file() -> TestResult {
    let (root, path, owner) = key_file(KEY_ONE, 0o600)?;
    let signer = strict(&path, owner)?;
    assert_eq!(
        signer.address(),
        address!("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
    );
    set_mode(&path, 0o640)?;
    assert_unsafe(strict(&path, owner), &path, "key mode is not 0600");
    set_mode(&path, 0o600)?;
    let link = root.path().join("hard-link.hex");
    std::fs::hard_link(&path, &link)?;
    assert_unsafe(
        strict(&path, owner),
        &path,
        "key has an unexpected hard-link count",
    );
    std::fs::remove_file(link)?;
    let symlink = root.path().join("symlink.hex");
    std::os::unix::fs::symlink(&path, &symlink)?;
    assert_unsafe(
        strict(&symlink, owner),
        &symlink,
        "key is not a regular file",
    );
    Ok(())
}

#[test]
fn strict_role_key_loader_rejects_noncanonical_hex() -> TestResult {
    for encoded in [
        "A".repeat(64),
        format!("0x{}", "01".repeat(32)),
        "g".repeat(64),
    ] {
        let (_root, path, owner) = key_file(encoded.as_bytes(), 0o600)?;
        assert!(
            matches!(strict(&path, owner), Err(SignerError::NonCanonicalKeyFile { path: actual }) if actual == path)
        );
    }
    Ok(())
}

#[test]
fn strict_role_key_loader_ignores_surrounding_ascii_whitespace() -> TestResult {
    let encoded = format!(" \t{}\r\n", std::str::from_utf8(KEY_ONE)?);
    assert_strict_encoded_address(encoded.as_bytes())
}

#[test]
fn strict_owner_and_metadata_checks_precede_payload_decoding() -> TestResult {
    let (_root, path, owner) = key_file(b"invalid", 0o640)?;
    assert_unsafe(
        strict(&path, owner.wrapping_add(1)),
        &path,
        "key has the wrong owner",
    );
    assert_unsafe(strict(&path, owner), &path, "key mode is not 0600");
    set_mode(&path, 0o600)?;
    assert!(
        matches!(strict(&path, owner), Err(SignerError::NonCanonicalKeyFile { path: actual }) if actual == path)
    );
    Ok(())
}

#[test]
fn strict_loader_rejects_directory_and_encoded_length_boundaries() -> TestResult {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("directory");
    std::fs::create_dir(&directory)?;
    let owner = std::fs::metadata(&directory)?.uid();
    assert_unsafe(
        strict(&directory, owner),
        &directory,
        "key is not a regular file",
    );
    for size in [63, 129] {
        let (_root, path, owner) = key_file(&vec![b'1'; size], 0o600)?;
        assert!(
            matches!(strict(&path, owner), Err(SignerError::NonCanonicalKeyFile { path: actual }) if actual == path)
        );
    }
    let maximum = format!("{}{}", std::str::from_utf8(KEY_ONE)?, " ".repeat(64));
    assert_strict_encoded_address(maximum.as_bytes())
}

#[test]
fn loaders_preserve_io_error_paths_and_sources() -> TestResult {
    let root = tempfile::tempdir()?;
    let missing = root.path().join("missing.hex");
    assert!(
        matches!(strict(&missing, 0), Err(SignerError::InspectPermissions { path, source }) if path == missing && source.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(
        matches!(permissive(&missing), Err(SignerError::InspectPermissions { path, source }) if path == missing && source.kind() == std::io::ErrorKind::NotFound)
    );
    let (_root, path, owner) = key_file(&[0xff; 64], 0o600)?;
    assert!(
        matches!(strict(&path, owner), Err(SignerError::ReadKey { path: actual, source }) if actual == path && source.kind() == std::io::ErrorKind::InvalidData)
    );
    Ok(())
}

#[test]
fn permissive_loader_preserves_prefix_case_symlink_and_mode_policy() -> TestResult {
    let encoded = format!(" 0x{}\n", "AB".repeat(32));
    let (root, path, owner) = key_file(encoded.as_bytes(), 0o400)?;
    let expected = OutbeEvmSigner::from_secret_bytes([0xab; 32])?;
    assert_eq!(permissive(&path)?.address(), expected.address());
    assert_unsafe(strict(&path, owner), &path, "key mode is not 0600");
    let link = root.path().join("symlink.hex");
    std::os::unix::fs::symlink(&path, &link)?;
    assert_eq!(permissive(&link)?.address(), expected.address());
    set_mode(&path, 0o640)?;
    assert!(
        matches!(permissive(&path), Err(SignerError::UnsafeFilePermissions { path: actual, mode: 0o640 }) if actual == path)
    );
    Ok(())
}

#[test]
fn valid_file_loading_preserves_fixed_signature_vector() -> TestResult {
    let encoded = "01".repeat(32);
    let (_root, path, owner) = key_file(encoded.as_bytes(), 0o600)?;
    let expected = "85374ecb6e0ea7cb84429448bf06ca12ea17d9ff80d8fe910f25f2f4fead5b3442c30ba1656842a610ff1da4bf1823604bf15d3493b9043af65b7dced9bbed4400";
    for signer in [strict(&path, owner)?, permissive(&path)?] {
        assert_eq!(
            hex::encode(signer.sign_hash(&B256::with_last_byte(42))?),
            expected
        );
    }
    Ok(())
}

#[test]
fn strict_canonical_hex_still_rejects_invalid_curve_secret() -> TestResult {
    let (_root, path, owner) = key_file("00".repeat(32).as_bytes(), 0o600)?;
    assert!(matches!(
        strict(&path, owner),
        Err(SignerError::InvalidSecret(_))
    ));
    Ok(())
}
