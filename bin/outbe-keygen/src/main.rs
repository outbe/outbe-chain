//! Outbe BLS key generation utility.
//!
//! Offline tool for generating BLS12-381 MinPk keypairs and signing
//! registration messages for the ValidatorSet precompile.
//!
//! Supports key storage backends: plaintext, encrypted (AES-256-GCM),
//! OS keychain (macOS Keychain / Linux Secret Service).

use alloy_primitives::{keccak256, Address};
use clap::{Parser, Subcommand, ValueEnum};
use commonware_codec::Encode;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;
use eyre::{Result, WrapErr};
use outbe_consensus::bls::{self, KeyBackend};

#[cfg(not(test))]
fn exit_process(code: i32) -> ! {
    std::process::exit(code)
}

#[cfg(test)]
fn exit_process(code: i32) -> ! {
    panic!("exit({code})")
}
use rand_core::RngCore as _;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

/// BLS_SIG DST used for registration signatures.
/// Must match `verify_bls_registration_sig` in validatorset/logic.rs.
const REGISTER_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_outbe_REGISTER";

/// Key storage backend for BLS key files.
#[derive(Debug, Clone, ValueEnum)]
enum KeyBackendArg {
    /// Plaintext hex files (default).
    Plaintext,
    /// AES-256-GCM encrypted with passphrase (Argon2id KDF).
    Encrypted,
    /// OS keychain (macOS Keychain / Linux Secret Service).
    OsLevel,
}

#[derive(Parser)]
#[command(
    name = "outbe-keygen",
    about = "BLS key generation for Outbe validators"
)]
struct Cli {
    /// Key storage backend.
    #[arg(long, value_enum, default_value = "plaintext", global = true)]
    key_backend: KeyBackendArg,

    /// Passphrase for encrypted backend (or env BLS_PASSPHRASE).
    #[arg(long, global = true, env = "BLS_PASSPHRASE")]
    passphrase: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new BLS12-381 MinPk keypair.
    Generate {
        /// Directory to save the signing key file. Defaults to current directory.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },

    /// Display the public key and pubkey hash for an existing signing key.
    ShowPubkey {
        /// Path to the signing-key.hex file.
        #[arg(long)]
        key: PathBuf,
    },

    /// Sign a registration message for the ValidatorSet precompile.
    SignRegistration {
        /// Path to the signing-key.hex file.
        #[arg(long)]
        key: PathBuf,

        /// Ethereum address of the validator being registered.
        #[arg(long)]
        validator_address: Address,
    },

    /// Verify integrity of a BLS key file.
    Verify {
        /// Path to the signing-key.hex file.
        #[arg(long)]
        key: PathBuf,
    },

    /// Generate both an ECDSA (secp256k1) private key and a BLS12-381 keypair.
    Hybrid {
        /// Directory to save both key files. Defaults to current directory.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let backend = resolve_backend(&cli)?;

    match cli.command {
        Commands::Generate { output_dir } => cmd_generate(output_dir, &backend),
        Commands::ShowPubkey { key } => cmd_show_pubkey(key, &backend),
        Commands::SignRegistration {
            key,
            validator_address,
        } => cmd_sign_registration(key, validator_address, &backend),
        Commands::Verify { key } => cmd_verify(key, &backend),
        Commands::Hybrid { output_dir } => cmd_hybrid(output_dir, &backend),
    }
}

/// Resolve the CLI args into a [`KeyBackend`].
fn resolve_backend(cli: &Cli) -> Result<KeyBackend> {
    match cli.key_backend {
        KeyBackendArg::Plaintext => Ok(KeyBackend::Plaintext),
        KeyBackendArg::Encrypted => {
            let passphrase = cli.passphrase.as_deref().ok_or_else(|| {
                eyre::eyre!("--passphrase (or BLS_PASSPHRASE env) required for encrypted backend")
            })?;
            Ok(KeyBackend::Encrypted(passphrase.to_string()))
        }
        KeyBackendArg::OsLevel => Ok(KeyBackend::OsLevel),
    }
}

/// Generate a new BLS keypair and save the private key.
fn cmd_generate(output_dir: PathBuf, backend: &KeyBackend) -> Result<()> {
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();
    let pk_bytes = pk.encode();
    let pubkey_hash = keccak256(&pk_bytes);

    let key_path = output_dir.join("signing-key.hex");
    bls::save_individual_key(&key_path, &key, backend)
        .wrap_err_with(|| format!("failed to write key: {}", key_path.display()))?;

    println!("BLS keypair generated");
    println!("  private key: {}", key_path.display());
    println!("  public key:  {}", hex::encode(&pk_bytes));
    println!("  pubkey hash: {pubkey_hash}");

    Ok(())
}

/// Display the public key for an existing signing key file.
fn cmd_show_pubkey(key_path: PathBuf, backend: &KeyBackend) -> Result<()> {
    let key = load_key(&key_path, backend)?;
    let pk = key.public_key();
    let pk_bytes = pk.encode();
    let pubkey_hash = keccak256(&pk_bytes);

    println!("public key:  {}", hex::encode(&pk_bytes));
    println!("pubkey hash: {pubkey_hash}");

    Ok(())
}

/// Sign a registration message proving BLS key ownership.
fn cmd_sign_registration(
    key_path: PathBuf,
    validator_address: Address,
    backend: &KeyBackend,
) -> Result<()> {
    let key = load_key(&key_path, backend)?;
    let pk = key.public_key();
    let pk_bytes = pk.encode();

    // Convert commonware PrivateKey (32-byte scalar) to blst SecretKey for signing.
    let sk_bytes = key.encode();
    let blst_sk = blst::min_pk::SecretKey::from_bytes(&sk_bytes)
        .map_err(|e| eyre::eyre!("failed to create blst SecretKey: {e:?}"))?;

    // Sign the validator address (20 bytes) with the registration DST.
    let sig = blst_sk.sign(validator_address.as_slice(), REGISTER_DST, &[]);
    let sig_bytes = sig.to_bytes();

    println!("registration signature for {validator_address}:");
    println!("  pubkey:    {}", hex::encode(&pk_bytes));
    println!("  signature: {}", hex::encode(sig_bytes));

    Ok(())
}

/// Verify the integrity of a BLS key file.
fn cmd_verify(key_path: PathBuf, backend: &KeyBackend) -> Result<()> {
    // 1. Read and parse the key.
    let key = match load_key(&key_path, backend) {
        Ok(k) => {
            println!("[OK]  key file readable and valid BLS12-381 scalar");
            k
        }
        Err(e) => {
            println!("[FAIL] key file invalid: {e}");
            exit_process(1);
        }
    };

    // 2. Derive public key.
    let pk = key.public_key();
    let pk_bytes = pk.encode();
    println!("[OK]  public key derivable: {}", hex::encode(&pk_bytes));

    // 3. Test sign + verify roundtrip.
    use commonware_cryptography::{Signer as _, Verifier as _};
    let sig = key.sign(b"outbe-verify", b"test-message");
    if pk.verify(b"outbe-verify", b"test-message", &sig) {
        println!("[OK]  sign/verify roundtrip passed");
    } else {
        println!("[FAIL] sign/verify roundtrip failed");
        std::process::exit(1);
    }

    println!("\nkey verification: PASSED");
    Ok(())
}

/// Generate both an ECDSA (secp256k1) private key and a BLS12-381 keypair.
fn cmd_hybrid(output_dir: PathBuf, backend: &KeyBackend) -> Result<()> {
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    // 1. Generate BLS12-381 keypair.
    let bls_key = bls12381::PrivateKey::random(rand_core::OsRng);
    let bls_pk = bls_key.public_key();
    let bls_pk_bytes = bls_pk.encode();
    let bls_pubkey_hash = keccak256(&bls_pk_bytes);

    let bls_key_path = output_dir.join("signing-key.hex");
    bls::save_individual_key(&bls_key_path, &bls_key, backend)
        .wrap_err_with(|| format!("failed to write BLS key: {}", bls_key_path.display()))?;

    // 2. Generate ECDSA secp256k1 private key (32 random bytes).
    // ECDSA key is always plaintext hex (not BLS, not affected by backend).
    let mut ecdsa_bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut ecdsa_bytes);
    if ecdsa_bytes.iter().all(|&b| b == 0) {
        eyre::bail!("generated zero ECDSA key — extremely unlikely, try again");
    }

    let ecdsa_key_path = output_dir.join("evm-key.hex");
    let ecdsa_key_hex = hex::encode(ecdsa_bytes);
    write_secret_hex_file(&ecdsa_key_path, &ecdsa_key_hex)
        .wrap_err_with(|| format!("failed to write EVM key: {}", ecdsa_key_path.display()))?;

    println!("hybrid keypair generated");
    println!();
    println!("BLS12-381:");
    println!("  private key:  {}", bls_key_path.display());
    println!("  public key:   {}", hex::encode(&bls_pk_bytes));
    println!("  pubkey hash:  {bls_pubkey_hash}");
    println!();
    println!("EVM artifact signer (secp256k1):");
    println!("  private key:  {}", ecdsa_key_path.display());
    println!("  use this path with --validator.evm-key");

    Ok(())
}

fn write_secret_hex_file(path: &Path, contents: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| eyre::eyre!("secret key path has no file name: {}", path.display()))?
        .to_string_lossy();
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp.{}", std::process::id()));

    let write_result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result
}

/// Load a BLS12-381 individual signing key using the configured backend.
fn load_key(path: &Path, backend: &KeyBackend) -> Result<bls12381::PrivateKey> {
    bls::load_individual_key(path, backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn make_cli(args: &[&str]) -> Cli {
        let mut full = vec!["keygen"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    #[test]
    fn test_resolve_backend_default_is_plaintext() {
        let cli = make_cli(&["generate", "--output-dir", "/tmp"]);
        let backend = resolve_backend(&cli).unwrap();
        assert!(matches!(backend, KeyBackend::Plaintext));
    }

    #[test]
    fn test_resolve_backend_plaintext_explicit() {
        let cli = make_cli(&[
            "--key-backend",
            "plaintext",
            "generate",
            "--output-dir",
            "/tmp",
        ]);
        let backend = resolve_backend(&cli).unwrap();
        assert!(matches!(backend, KeyBackend::Plaintext));
    }

    #[test]
    fn test_resolve_backend_encrypted_with_passphrase() {
        let cli = make_cli(&[
            "--key-backend",
            "encrypted",
            "--passphrase",
            "mypass",
            "generate",
            "--output-dir",
            "/tmp",
        ]);
        let backend = resolve_backend(&cli).unwrap();
        assert!(matches!(backend, KeyBackend::Encrypted(ref p) if p == "mypass"));
    }

    #[test]
    fn test_resolve_backend_encrypted_missing_passphrase() {
        let cli = make_cli(&[
            "--key-backend",
            "encrypted",
            "generate",
            "--output-dir",
            "/tmp",
        ]);
        assert!(resolve_backend(&cli).is_err());
    }

    #[test]
    fn test_resolve_backend_os_level() {
        let cli = make_cli(&[
            "--key-backend",
            "os-level",
            "generate",
            "--output-dir",
            "/tmp",
        ]);
        let backend = resolve_backend(&cli).unwrap();
        assert!(matches!(backend, KeyBackend::OsLevel));
    }

    #[test]
    fn test_resolve_backend_invalid_rejected_by_clap() {
        let result = Cli::try_parse_from([
            "keygen",
            "--key-backend",
            "bogus",
            "generate",
            "--output-dir",
            "/tmp",
        ]);
        assert!(result.is_err());
    }

    // --- TC-006: keygen command e2e tests ---

    #[test]
    fn test_cmd_generate_creates_key_file() {
        let dir = tempfile::tempdir().unwrap();
        cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        assert!(key_path.exists());
        let contents = std::fs::read_to_string(&key_path).unwrap();
        assert!(!contents.is_empty());
    }

    #[test]
    fn test_cmd_generate_encrypted_backend() {
        let dir = tempfile::tempdir().unwrap();
        let backend = KeyBackend::Encrypted("testpass".to_string());
        cmd_generate(dir.path().to_path_buf(), &backend).unwrap();
        assert!(dir.path().join("signing-key.hex").exists());
    }

    #[test]
    fn test_cmd_show_pubkey_after_generate() {
        let dir = tempfile::tempdir().unwrap();
        cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        cmd_show_pubkey(key_path, &KeyBackend::Plaintext).unwrap();
    }

    #[test]
    fn test_cmd_show_pubkey_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_show_pubkey(dir.path().join("nonexistent.hex"), &KeyBackend::Plaintext);
        assert!(result.is_err());
    }

    #[test]
    fn test_cmd_sign_registration_valid() {
        let dir = tempfile::tempdir().unwrap();
        cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        cmd_sign_registration(key_path, addr, &KeyBackend::Plaintext).unwrap();
    }

    #[test]
    fn test_cmd_sign_registration_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let result =
            cmd_sign_registration(dir.path().join("none.hex"), addr, &KeyBackend::Plaintext);
        assert!(result.is_err());
    }

    // TC-007: verify signature end-to-end with registration DST
    #[test]
    fn test_sign_registration_verifies_with_dst() {
        let dir = tempfile::tempdir().unwrap();
        cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        let key = load_key(&key_path, &KeyBackend::Plaintext).unwrap();
        let pk = key.public_key();
        let pk_bytes = pk.encode();

        let addr: Address = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            .parse()
            .unwrap();

        // Sign the same way cmd_sign_registration does
        let sk_bytes = key.encode();
        let blst_sk = blst::min_pk::SecretKey::from_bytes(&sk_bytes).unwrap();
        let sig = blst_sk.sign(addr.as_slice(), REGISTER_DST, &[]);

        // Verify
        let blst_pk = blst::min_pk::PublicKey::from_bytes(&pk_bytes).unwrap();
        let result = sig.verify(true, addr.as_slice(), REGISTER_DST, &[], &blst_pk, true);
        assert_eq!(result, blst::BLST_ERROR::BLST_SUCCESS);
    }

    #[test]
    fn test_cmd_verify_valid_key() {
        let dir = tempfile::tempdir().unwrap();
        cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        cmd_verify(key_path, &KeyBackend::Plaintext).unwrap();
    }

    #[test]
    #[should_panic(expected = "exit(1)")]
    fn test_cmd_verify_corrupted_file_exits() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("signing-key.hex");
        std::fs::write(&key_path, "not-a-valid-key-at-all").unwrap();
        // exit_process(1) in test mode panics with "exit(1)"
        let _ = cmd_verify(key_path, &KeyBackend::Plaintext);
    }

    // TC-006: hybrid
    #[test]
    fn test_cmd_hybrid_creates_both_keys() {
        let dir = tempfile::tempdir().unwrap();
        cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        assert!(dir.path().join("signing-key.hex").exists());
        assert!(dir.path().join("evm-key.hex").exists());
    }

    // TC-028: hybrid ECDSA key validity
    #[test]
    fn test_cmd_hybrid_ecdsa_key_is_valid_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext).unwrap();
        let ecdsa_hex = std::fs::read_to_string(dir.path().join("evm-key.hex")).unwrap();
        assert_eq!(ecdsa_hex.len(), 64); // 32 bytes = 64 hex chars
        let ecdsa_bytes = hex::decode(&ecdsa_hex).unwrap();
        assert!(!ecdsa_bytes.iter().all(|&b| b == 0)); // non-zero
    }

    // TC-008: encrypted save/load roundtrip
    #[test]
    fn test_encrypted_generate_and_show() {
        let dir = tempfile::tempdir().unwrap();
        let backend = KeyBackend::Encrypted("secret123".to_string());
        cmd_generate(dir.path().to_path_buf(), &backend).unwrap();
        let key_path = dir.path().join("signing-key.hex");
        cmd_show_pubkey(key_path.clone(), &backend).unwrap();
        // Wrong passphrase fails
        let wrong_backend = KeyBackend::Encrypted("wrong".to_string());
        assert!(cmd_show_pubkey(key_path, &wrong_backend).is_err());
    }
}
