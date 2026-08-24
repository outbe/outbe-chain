//! Outbe BLS key generation utility.
//!
//! Offline tool for generating BLS12-381 MinPk keypairs and signing
//! registration messages for the ValidatorSet precompile.
//!
//! Supports key storage backends: plaintext, encrypted (AES-256-GCM),
//! OS keychain (macOS Keychain / Linux Secret Service).

use alloy_primitives::{keccak256, Address, B256};
use clap::{Parser, Subcommand, ValueEnum};
use commonware_codec::Encode;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;
use eyre::{Result, WrapErr};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_consensus::bls::{self, KeyBackend};
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    profile::poc_schema_limits,
};
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};

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
    fs::{File, OpenOptions},
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const OCOMP_KEY_FILENAME: &str = "ocomp-key-v1.hex";
const OCOMP_REGISTRATION_FILENAME: &str = "ocomp-registration-v1.ocb1";

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

        /// Chain ID on which this registration proof will be accepted.
        #[arg(long)]
        chain_id: u64,

        /// Radicle Ed25519 NodeId bound atomically to this validator.
        #[arg(long)]
        radicle_node_id: B256,
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

    /// Generate a Heartwood-compatible Radicle Ed25519 identity.
    Radicle {
        /// Radicle home directory that will receive keys/radicle and keys/radicle.pub.
        #[arg(long)]
        output_dir: PathBuf,
    },

    /// Generate a dedicated OCOMP result-signing key and PoP registration.
    Ocomp {
        /// Directory for the immutable secret and public registration artifacts.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
        #[arg(long)]
        chain_id: u64,
        #[arg(long)]
        genesis_hash: B256,
        /// Validator EVM address used by the canonical identity preimage.
        #[arg(long)]
        validator_address: Address,
        /// Exact 48-byte consensus BLS MinPk public key, encoded as hex.
        #[arg(long, value_parser = parse_consensus_bls_min_pk)]
        consensus_bls_min_pk: [u8; 48],
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
            chain_id,
            radicle_node_id,
        } => cmd_sign_registration(key, validator_address, chain_id, radicle_node_id, &backend),
        Commands::Verify { key } => cmd_verify(key, &backend),
        Commands::Hybrid { output_dir } => cmd_hybrid(output_dir, &backend),
        Commands::Radicle { output_dir } => cmd_radicle(output_dir),
        Commands::Ocomp {
            output_dir,
            chain_id,
            genesis_hash,
            validator_address,
            consensus_bls_min_pk,
        } => cmd_ocomp(
            output_dir,
            OcompRegistrationArgs {
                chain_id,
                genesis_hash,
                validator_address,
                consensus_bls_min_pk,
            },
        ),
    }
}

#[derive(Clone, Copy)]
struct OcompRegistrationArgs {
    chain_id: u64,
    genesis_hash: B256,
    validator_address: Address,
    consensus_bls_min_pk: [u8; 48],
}

fn parse_consensus_bls_min_pk(value: &str) -> std::result::Result<[u8; 48], String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let mut public_key = [0u8; 48];
    hex::decode_to_slice(value, &mut public_key).map_err(|error| {
        format!("consensus BLS MinPk key must be exactly 48 bytes of hex: {error}")
    })?;
    Ok(public_key)
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
    chain_id: u64,
    radicle_node_id: B256,
    backend: &KeyBackend,
) -> Result<()> {
    let key = load_key(&key_path, backend)?;
    let pk = key.public_key();
    let pk_bytes = pk.encode();

    // Convert commonware PrivateKey (32-byte scalar) to blst SecretKey for signing.
    let sk_bytes = key.encode();
    let blst_sk = blst::min_pk::SecretKey::from_bytes(&sk_bytes)
        .map_err(|e| eyre::eyre!("failed to create blst SecretKey: {e:?}"))?;

    let message = validator_registration_message(chain_id, validator_address, radicle_node_id);
    let sig = blst_sk.sign(&message, VALIDATOR_REGISTRATION_DST, &[]);
    let sig_bytes = sig.to_bytes();

    println!(
        "registration signature for {validator_address} and Radicle NodeId {radicle_node_id} on chain {chain_id}:"
    );
    println!("  pubkey:    {}", hex::encode(&pk_bytes));
    println!("  signature: {}", hex::encode(sig_bytes));

    Ok(())
}

/// Generate the native unencrypted OpenSSH Ed25519 identity expected by Heartwood.
fn cmd_radicle(output_dir: PathBuf) -> Result<()> {
    use radicle_crypto::{ssh::Keystore, Seed};

    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create Radicle home: {}", output_dir.display()))?;
    std::fs::set_permissions(&output_dir, std::fs::Permissions::from_mode(0o700))
        .wrap_err_with(|| format!("failed to protect Radicle home: {}", output_dir.display()))?;

    let keys = output_dir.join("keys");
    std::fs::create_dir_all(&keys)
        .wrap_err_with(|| format!("failed to create Radicle key directory: {}", keys.display()))?;
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).wrap_err_with(
        || {
            format!(
                "failed to protect Radicle key directory: {}",
                keys.display()
            )
        },
    )?;

    let mut seed = [0u8; Seed::BYTES];
    rand_core::OsRng.fill_bytes(&mut seed);
    let keystore = Keystore::new(&keys);
    let node_id = keystore
        .init("outbe-radicle", None, Seed::new(seed))
        .wrap_err("failed to create Heartwood-compatible Radicle identity")?;
    std::fs::set_permissions(
        keystore.secret_key_path(),
        std::fs::Permissions::from_mode(0o600),
    )
    .wrap_err_with(|| {
        format!(
            "failed to protect Radicle secret: {}",
            keystore.secret_key_path().display()
        )
    })?;

    println!("Radicle identity generated");
    println!("  private key: {}", keystore.secret_key_path().display());
    if let Some(public_path) = keystore.public_key_path() {
        println!("  public key:  {}", public_path.display());
    }
    println!("  node id:     {node_id}");
    println!("  node id hex: {}", hex::encode(node_id.into_inner()));
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

fn cmd_ocomp(output_dir: PathBuf, args: OcompRegistrationArgs) -> Result<()> {
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;
    let directory_metadata = std::fs::symlink_metadata(&output_dir)
        .wrap_err_with(|| format!("failed to inspect output dir: {}", output_dir.display()))?;
    if !directory_metadata.file_type().is_dir() {
        eyre::bail!(
            "OCOMP output path is not a regular directory: {}",
            output_dir.display()
        );
    }
    let key_path = output_dir.join(OCOMP_KEY_FILENAME);
    let registration_path = output_dir.join(OCOMP_REGISTRATION_FILENAME);
    for path in [&key_path, &registration_path] {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                eyre::bail!("OCOMP artifact already exists: {}", path.display());
            }
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!("failed to inspect OCOMP artifact path: {}", path.display())
                });
            }
        }
    }

    let signing_key = SigningKey::random(&mut rand_core::OsRng);
    let public_key = signing_key.verifying_key().to_encoded_point(true);
    let public_key_sec1: [u8; 33] = public_key
        .as_bytes()
        .try_into()
        .map_err(|_| eyre::eyre!("generated OCOMP public key is not compressed SEC1-33"))?;
    let core = OcompKeyRegistrationCoreV1 {
        chain_id: args.chain_id,
        genesis_hash: args.genesis_hash,
        validator_identity_hash: validator_identity_hash_v1(
            args.validator_address,
            &args.consensus_bls_min_pk,
        )?,
        ocomp_public_key_sec1: public_key_sec1,
        key_epoch: POC_KEY_EPOCH,
        allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
    };
    let limits = poc_schema_limits();
    let mut registration = OcompKeyRegistrationV1 {
        core,
        proof_of_possession: [0; 64],
    };
    let pop_digest = registration.proof_of_possession_digest(&limits)?;
    let proof: Signature = signing_key
        .sign_prehash(pop_digest.as_slice())
        .map_err(|_| eyre::eyre!("failed to sign OCOMP proof of possession"))?;
    let proof = proof.normalize_s().unwrap_or(proof);
    registration.proof_of_possession = proof.to_bytes().into();
    registration.validate_proof_of_possession(&limits)?;

    let mut secret = Zeroizing::new(hex::encode(signing_key.to_bytes()));
    secret.push('\n');
    let registration_bytes = registration.encode_canonical(&limits)?;
    write_immutable_file(&registration_path, &registration_bytes, 0o644).wrap_err_with(|| {
        format!(
            "failed to install OCOMP public registration: {}",
            registration_path.display()
        )
    })?;
    if let Err(error) = write_immutable_file(&key_path, secret.as_bytes(), 0o600) {
        let cleanup = std::fs::remove_file(&registration_path)
            .and_then(|()| File::open(&output_dir)?.sync_all());
        if let Err(cleanup_error) = cleanup {
            eyre::bail!(
                "OCOMP secret installation failed at {}: {error}; \
                 cleanup of the new public registration at {} also failed: {cleanup_error}",
                key_path.display(),
                registration_path.display()
            );
        }
        return Err(error)
            .wrap_err_with(|| format!("failed to install OCOMP key: {}", key_path.display()));
    }

    println!("OCOMP result-signing artifacts generated");
    println!("  private key:  {}", key_path.display());
    println!("  public key:   {}", hex::encode(public_key_sec1));
    println!("  key epoch:    {POC_KEY_EPOCH}");
    println!("  registration: {}", registration_path.display());
    Ok(())
}

fn write_immutable_file(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("artifact path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| eyre::eyre!("artifact path has no file name: {}", path.display()))?
        .to_string_lossy();
    let pending_path = parent.join(format!(".{file_name}.pending.{}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut pending = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&pending_path)?;
        pending.write_all(contents)?;
        pending.sync_all()?;
        std::fs::hard_link(&pending_path, path)?;
        File::open(parent)?.sync_all()?;
        std::fs::remove_file(&pending_path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&pending_path);
    }
    result
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
    fn radicle_subcommand_parses_without_bls_backend_inputs() {
        let cli = make_cli(&["radicle", "--output-dir", "/tmp/radicle-home"]);
        assert!(matches!(cli.command, Commands::Radicle { .. }));
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
    fn radicle_command_creates_native_heartwood_identity_with_strict_modes() {
        use radicle_crypto::Signer as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        cmd_radicle(dir.path().to_path_buf()).unwrap();

        let keys = dir.path().join("keys");
        let secret = keys.join("radicle");
        let public = keys.join("radicle.pub");
        assert_eq!(
            std::fs::metadata(&keys).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(public.is_file());

        let keystore = radicle_crypto::ssh::Keystore::new(&keys);
        let secret_key = keystore.secret_key(None).unwrap().unwrap();
        let public_key = keystore.public_key().unwrap().unwrap();
        assert_eq!(secret_key.public_key(), &public_key);
    }

    #[test]
    fn radicle_command_refuses_to_overwrite_an_existing_identity() {
        let dir = tempfile::tempdir().unwrap();
        cmd_radicle(dir.path().to_path_buf()).unwrap();
        let first_secret = std::fs::read(dir.path().join("keys/radicle")).unwrap();

        assert!(cmd_radicle(dir.path().to_path_buf()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("keys/radicle")).unwrap(),
            first_secret
        );
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
        cmd_sign_registration(
            key_path,
            addr,
            1,
            B256::repeat_byte(0x11),
            &KeyBackend::Plaintext,
        )
        .unwrap();
    }

    #[test]
    fn test_cmd_sign_registration_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let result = cmd_sign_registration(
            dir.path().join("none.hex"),
            addr,
            1,
            B256::repeat_byte(0x11),
            &KeyBackend::Plaintext,
        );
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
        let chain_id = 54322345;

        // Sign the same way cmd_sign_registration does
        let sk_bytes = key.encode();
        let blst_sk = blst::min_pk::SecretKey::from_bytes(&sk_bytes).unwrap();
        let node_id = B256::repeat_byte(0x12);
        let message = validator_registration_message(chain_id, addr, node_id);
        let sig = blst_sk.sign(&message, VALIDATOR_REGISTRATION_DST, &[]);

        // Verify
        let blst_pk = blst::min_pk::PublicKey::from_bytes(&pk_bytes).unwrap();
        let result = sig.verify(
            true,
            &message,
            VALIDATOR_REGISTRATION_DST,
            &[],
            &blst_pk,
            true,
        );
        assert_eq!(result, blst::BLST_ERROR::BLST_SUCCESS);

        let other_chain_message = validator_registration_message(chain_id + 1, addr, node_id);
        let replay_result = sig.verify(
            true,
            &other_chain_message,
            VALIDATOR_REGISTRATION_DST,
            &[],
            &blst_pk,
            true,
        );
        assert_ne!(replay_result, blst::BLST_ERROR::BLST_SUCCESS);

        let other_node_message =
            validator_registration_message(chain_id, addr, B256::repeat_byte(0x13));
        let node_replay_result = sig.verify(
            true,
            &other_node_message,
            VALIDATOR_REGISTRATION_DST,
            &[],
            &blst_pk,
            true,
        );
        assert_ne!(node_replay_result, blst::BLST_ERROR::BLST_SUCCESS);
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

    #[test]
    fn ocm_sig_001_ocomp_command_writes_immutable_key_and_valid_pop_registration() {
        use std::os::unix::fs::PermissionsExt as _;

        use alloy_primitives::{Address, B256};
        use outbe_ocomp_protocol::committee::{
            validator_identity_hash_v1, OcompKeyRegistrationV1, POC_KEY_EPOCH,
            RESULT_SIGNATURE_PURPOSE_BITMAP,
        };
        use outbe_ocomp_protocol::profile::poc_schema_limits;

        let directory = tempfile::tempdir().unwrap();
        let validator_address = Address::repeat_byte(0x44);
        let consensus_bls_min_pk = [0x55; 48];
        let args = OcompRegistrationArgs {
            chain_id: 42,
            genesis_hash: B256::repeat_byte(0x11),
            validator_address,
            consensus_bls_min_pk,
        };

        cmd_ocomp(directory.path().to_path_buf(), args).expect("generate OCOMP artifacts");
        let key_path = directory.path().join(OCOMP_KEY_FILENAME);
        let registration_path = directory.path().join(OCOMP_REGISTRATION_FILENAME);
        let secret = std::fs::read(&key_path).expect("read OCOMP secret");
        assert_eq!(secret.len(), 65);
        assert_eq!(secret[64], b'\n');
        assert!(secret[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)));
        assert_eq!(
            std::fs::metadata(&key_path)
                .expect("OCOMP key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let registration = OcompKeyRegistrationV1::decode_canonical(
            &std::fs::read(&registration_path).expect("read registration"),
            &poc_schema_limits(),
        )
        .expect("decode registration");
        registration
            .validate_proof_of_possession(&poc_schema_limits())
            .expect("validate proof of possession");
        assert_eq!(registration.core.key_epoch, POC_KEY_EPOCH);
        assert_eq!(
            registration.core.allowed_purpose_bitmap,
            RESULT_SIGNATURE_PURPOSE_BITMAP
        );
        assert_eq!(
            registration.core.validator_identity_hash,
            validator_identity_hash_v1(validator_address, &consensus_bls_min_pk)
                .expect("validator identity")
        );

        let original_secret = secret;
        assert!(cmd_ocomp(directory.path().to_path_buf(), args).is_err());
        assert_eq!(
            std::fs::read(&key_path).expect("read unchanged OCOMP secret"),
            original_secret
        );
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
