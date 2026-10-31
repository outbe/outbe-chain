//! Outbe BLS key generation utility.
//!
//! Offline tool for generating BLS12-381 MinPk keypairs and signing
//! registration messages for the ValidatorSet precompile.
//!
//! Supports key storage backends: plaintext, encrypted (AES-256-GCM),
//! OS keychain (macOS Keychain / Linux Secret Service).

mod errors;

use alloy_primitives::{keccak256, Address, B256};
use clap::{Parser, Subcommand, ValueEnum};
use commonware_codec::Encode;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;
use eyre::{Result, WrapErr};
use k256::ecdsa::SigningKey;
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};

use crate::errors::KeygenError;

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
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const RETH_P2P_SECRET_FILENAME: &str = "reth-p2p-secret.hex";

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

    /// Passphrase for encrypted key backend (can also be set via BLS_PASSPHRASE env var)
    #[arg(long, global = true, env = "BLS_PASSPHRASE")]
    passphrase: Option<String>,

    /// Path to file containing the passphrase for encrypted key backend (takes precedence over --passphrase / BLS_PASSPHRASE)
    #[arg(long, global = true)]
    passphrase_file: Option<PathBuf>,

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

        /// Overwrite existing key files without prompting
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Display the public key and pubkey hash for an existing signing key.
    ShowPubkey {
        /// Path to the signing-key.hex file.
        #[arg(long)]
        key: PathBuf,
    },

    /// Display the EVM address of an existing secp256k1 private key (offline).
    ShowAddress {
        /// Private key as 32-byte hex, with or without the 0x prefix.
        #[arg(long)]
        private_key: String,
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

        /// Overwrite existing key files without prompting
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Generate a Heartwood-compatible Radicle Ed25519 identity.
    Radicle {
        /// Radicle home directory that will receive keys/radicle and keys/radicle.pub.
        #[arg(long)]
        output_dir: PathBuf,
    },

    /// Generates BLS, EVM, Reth P2P, and Radicle keys with the validator registration signature.
    Validator {
        /// Directory that receives every generated key artifact.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Chain ID bound by the registration signature.
        #[arg(long)]
        chain_id: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let backend = if matches!(&cli.command, Commands::ShowAddress { .. }) {
        KeyBackend::Plaintext
    } else {
        resolve_backend(&cli)?
    };

    match cli.command {
        Commands::Generate { output_dir, force } => cmd_generate(output_dir, &backend, force),
        Commands::ShowPubkey { key } => cmd_show_pubkey(key, &backend),
        Commands::ShowAddress { private_key } => {
            let address = show_address(private_key)?;
            println!("{address}");
            Ok(())
        }
        Commands::SignRegistration {
            key,
            validator_address,
            chain_id,
            radicle_node_id,
        } => cmd_sign_registration(key, validator_address, chain_id, radicle_node_id, &backend),
        Commands::Verify { key } => cmd_verify(key, &backend),
        Commands::Hybrid { output_dir, force } => cmd_hybrid(output_dir, &backend, force),
        Commands::Radicle { output_dir } => cmd_radicle(output_dir),
        Commands::Validator {
            output_dir,
            chain_id,
        } => cmd_validator(output_dir, chain_id, &backend),
    }
}

/// Resolve the CLI args into a [`KeyBackend`].
fn resolve_backend(cli: &Cli) -> Result<KeyBackend> {
    match cli.key_backend {
        KeyBackendArg::Plaintext => Ok(KeyBackend::Plaintext),
        KeyBackendArg::Encrypted => resolve_encrypted_backend(cli),
        KeyBackendArg::OsLevel => Ok(KeyBackend::OsLevel),
    }
}

/// Resolve the encrypted backend.
///
/// `--passphrase-file` wins over `--passphrase` and `BLS_PASSPHRASE`.
/// An empty CLI or env passphrase keeps its previous meaning.
fn resolve_encrypted_backend(cli: &Cli) -> Result<KeyBackend> {
    if let Some(path) = cli.passphrase_file.as_deref() {
        return Ok(KeyBackend::Encrypted(read_passphrase_file(path)?));
    }
    if let Some(passphrase) = cli.passphrase.as_deref() {
        return Ok(KeyBackend::Encrypted(passphrase.to_string()));
    }
    Err(KeygenError::MissingPassphrase.into())
}

/// Read a passphrase file into a `String` the encrypted backend can store.
///
/// On Unix the file mode must have no group or world bits. Only trailing
/// newlines are stripped, so the three passphrase channels accept the same text.
fn read_passphrase_file(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        let mode = std::fs::metadata(path)
            .wrap_err_with(|| format!("failed to inspect passphrase file: {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            return Err(KeygenError::InsecurePassphraseFileMode {
                path: path.to_path_buf(),
                mode,
            }
            .into());
        }
    }

    let raw = Zeroizing::new(
        std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read passphrase file: {}", path.display()))?,
    );
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err(KeygenError::EmptyPassphraseFile(path.to_path_buf()).into());
    }
    Ok(trimmed.to_owned())
}

fn is_affirmative_answer(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Preflight guard before key generation. `symlink_metadata` treats any
/// existing path, including a symlink, as occupied. `force` skips the check.
fn ensure_can_write_paths<F>(paths: &[&Path], force: bool, mut confirm: F) -> Result<()>
where
    F: FnMut(&Path) -> Result<bool>,
{
    if force {
        return Ok(());
    }
    for &path in paths {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                if !confirm(path)? {
                    return Err(KeygenError::KeyFileAlreadyExists(path.to_path_buf()).into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e)
                    .wrap_err_with(|| format!("failed to inspect key path: {}", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn prompt_overwrite_if_tty(path: &Path) -> Result<bool> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(false);
    }
    eprint!(
        "Key file already exists: {}. Overwrite? [y/N]: ",
        path.display()
    );
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .wrap_err("failed to read confirmation from stdin")?;
    Ok(is_affirmative_answer(&answer))
}

#[cfg(test)]
fn prompt_overwrite_if_tty(_path: &Path) -> Result<bool> {
    Ok(false)
}

fn ensure_can_write(paths: &[&Path], force: bool) -> Result<()> {
    ensure_can_write_paths(paths, force, prompt_overwrite_if_tty)
}

/// Generate a new BLS keypair and save the private key.
fn cmd_generate(output_dir: PathBuf, backend: &KeyBackend, force: bool) -> Result<()> {
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let key_path = output_dir.join("signing-key.hex");
    ensure_can_write(&[&key_path], force)?;

    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();
    let pk_bytes = pk.encode();
    let pubkey_hash = keccak256(&pk_bytes);

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
    let blst_sk =
        blst::min_pk::SecretKey::from_bytes(&sk_bytes).map_err(KeygenError::BlstSecretKey)?;

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

/// A freshly generated Heartwood-compatible Radicle identity.
struct RadicleIdentity {
    key_path: PathBuf,
    public_path: Option<PathBuf>,
    node_id: String,
    node_id_bytes: B256,
}

/// Generate the native unencrypted OpenSSH Ed25519 identity expected by Heartwood.
fn generate_radicle_identity(output_dir: &Path) -> Result<RadicleIdentity> {
    use radicle_crypto::{ssh::Keystore, Seed};

    std::fs::create_dir_all(output_dir)
        .wrap_err_with(|| format!("failed to create Radicle home: {}", output_dir.display()))?;
    std::fs::set_permissions(output_dir, std::fs::Permissions::from_mode(0o700))
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

    let node_id_display = node_id.to_string();
    let inner = node_id.into_inner();
    let node_id_slice: &[u8] = inner.as_ref();
    if node_id_slice.len() != 32 {
        return Err(KeygenError::InvalidRadicleNodeIdLength.into());
    }
    Ok(RadicleIdentity {
        key_path: keystore.secret_key_path().to_path_buf(),
        public_path: keystore.public_key_path().map(Path::to_path_buf),
        node_id: node_id_display,
        node_id_bytes: B256::from_slice(node_id_slice),
    })
}

fn cmd_radicle(output_dir: PathBuf) -> Result<()> {
    let identity = generate_radicle_identity(&output_dir)?;

    println!("Radicle identity generated");
    println!("  private key: {}", identity.key_path.display());
    if let Some(public_path) = &identity.public_path {
        println!("  public key:  {}", public_path.display());
    }
    println!("  node id:     {}", identity.node_id);
    println!("  node id hex: {}", hex::encode(identity.node_id_bytes));
    Ok(())
}

/// Native keys and registration signature for one validator.
struct ValidatorBundle {
    bls_key_path: PathBuf,
    bls_public_key: [u8; 48],
    bls_pubkey_hash: B256,
    evm_key_path: PathBuf,
    validator_address: Address,
    reth_p2p_key_path: PathBuf,
    radicle: RadicleIdentity,
    registration_signature: [u8; 96],
}

fn show_address(private_key: String) -> Result<Address> {
    let encoded = Zeroizing::new(private_key);
    let encoded = encoded.trim();
    let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
    let mut bytes = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(encoded, bytes.as_mut())
        .map_err(|_| KeygenError::InvalidEvmPrivateKeyHex)?;
    let signing_key =
        SigningKey::from_slice(bytes.as_ref()).wrap_err("invalid secp256k1 private key")?;
    evm_address(&signing_key)
}

fn evm_address(signing_key: &SigningKey) -> Result<Address> {
    let public_key = signing_key.verifying_key().to_encoded_point(false);
    let public_key_raw = public_key
        .as_bytes()
        .get(1..)
        .filter(|raw| raw.len() == 64)
        .ok_or(KeygenError::InvalidEvmPublicKeyPoint)?;
    Ok(Address::from_raw_public_key(public_key_raw))
}

fn generate_distinct_evm_key(existing: &[&SigningKey]) -> SigningKey {
    loop {
        let candidate = SigningKey::random(&mut rand_core::OsRng);
        if existing
            .iter()
            .all(|key| candidate.to_bytes() != key.to_bytes())
        {
            return candidate;
        }
    }
}

/// Generate all validator-owned runtime keys and the ValidatorSet registration
/// signature binding the consensus, validator EVM, and Radicle identities.
fn generate_validator_bundle(
    output_dir: &Path,
    chain_id: u64,
    backend: &KeyBackend,
) -> Result<ValidatorBundle> {
    std::fs::create_dir_all(output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let bls_key_path = output_dir.join("signing-key.hex");
    let evm_key_path = output_dir.join("evm-key.hex");
    let reth_p2p_key_path = output_dir.join(RETH_P2P_SECRET_FILENAME);
    let radicle_home = output_dir.join("radicle");
    let radicle_key_path = radicle_home.join("keys").join("radicle");
    let preexisting: Vec<String> = [
        &bls_key_path,
        &evm_key_path,
        &reth_p2p_key_path,
        &radicle_key_path,
    ]
    .into_iter()
    .filter(|path| path.exists())
    .map(|path| path.display().to_string())
    .collect();
    if !preexisting.is_empty() {
        return Err(KeygenError::ValidatorKeyArtifactsExist(preexisting.join(", ")).into());
    }

    // 1. BLS12-381 MinPk consensus key.
    let bls_key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let bls_pk_bytes = bls_key.public_key().encode();
    let bls_public_key: [u8; 48] = bls_pk_bytes
        .as_ref()
        .try_into()
        .map_err(|_| KeygenError::InvalidBlsPublicKeyLength)?;
    let bls_pubkey_hash = keccak256(bls_public_key);
    bls::save_individual_key(&bls_key_path, &bls_key, backend)
        .wrap_err_with(|| format!("failed to write BLS key: {}", bls_key_path.display()))?;

    // 2. ECDSA secp256k1 EVM signer. The validator address derives from it.
    let evm_signing_key = SigningKey::random(&mut rand_core::OsRng);
    let evm_key_hex = Zeroizing::new(hex::encode(evm_signing_key.to_bytes()));
    write_secret_hex_file(&evm_key_path, &evm_key_hex)
        .wrap_err_with(|| format!("failed to write EVM key: {}", evm_key_path.display()))?;
    let validator_address = evm_address(&evm_signing_key)?;

    // 3. Stable Reth transport identity. Reth parses the file verbatim, so the
    // canonical source artifact has no prefix or trailing line feed.
    let reth_p2p_key = generate_distinct_evm_key(&[&evm_signing_key]);
    let reth_p2p_key_hex = Zeroizing::new(hex::encode(reth_p2p_key.to_bytes()));
    write_secret_hex_file(&reth_p2p_key_path, &reth_p2p_key_hex).wrap_err_with(|| {
        format!(
            "failed to write Reth P2P key: {}",
            reth_p2p_key_path.display()
        )
    })?;

    // 4. Radicle Ed25519 identity. The registration binds it atomically.
    let radicle = generate_radicle_identity(&radicle_home)?;

    // 5. Registration signature for the ValidatorSet precompile.
    let sk_bytes = bls_key.encode();
    let blst_sk =
        blst::min_pk::SecretKey::from_bytes(&sk_bytes).map_err(KeygenError::BlstSecretKey)?;
    let message =
        validator_registration_message(chain_id, validator_address, radicle.node_id_bytes);
    let registration_signature = blst_sk
        .sign(&message, VALIDATOR_REGISTRATION_DST, &[])
        .to_bytes();

    Ok(ValidatorBundle {
        bls_key_path,
        bls_public_key,
        bls_pubkey_hash,
        evm_key_path,
        validator_address,
        reth_p2p_key_path,
        radicle,
        registration_signature,
    })
}

/// Generate all key material one validator needs, in one pass.
fn cmd_validator(output_dir: PathBuf, chain_id: u64, backend: &KeyBackend) -> Result<()> {
    let bundle = generate_validator_bundle(&output_dir, chain_id, backend)?;

    println!("validator key bundle generated (chain {chain_id})");
    println!();
    println!("BLS12-381 consensus key:");
    println!("  private key:  {}", bundle.bls_key_path.display());
    println!("  public key:   {}", hex::encode(bundle.bls_public_key));
    println!("  pubkey hash:  {}", bundle.bls_pubkey_hash);
    println!();
    println!("EVM artifact signer (secp256k1):");
    println!("  private key:  {}", bundle.evm_key_path.display());
    println!("  address:      {}", bundle.validator_address);
    println!();
    println!("Reth P2P identity (secp256k1):");
    println!("  private key:  {}", bundle.reth_p2p_key_path.display());
    println!();
    println!("Radicle identity:");
    println!("  private key:  {}", bundle.radicle.key_path.display());
    if let Some(public_path) = &bundle.radicle.public_path {
        println!("  public key:   {}", public_path.display());
    }
    println!("  node id:      {}", bundle.radicle.node_id);
    println!(
        "  node id hex:  {}",
        hex::encode(bundle.radicle.node_id_bytes)
    );
    println!();

    println!("registration (fund {} first):", bundle.validator_address);
    match backend {
        KeyBackend::Plaintext => {
            println!("  outbe-cli --private-key <evm-key> staking stake \\");
            println!("    --validator {} \\", bundle.validator_address);
            println!("    --amount <amount> \\");
            println!("    --keys-dir {}", output_dir.display());
        }
        KeyBackend::Encrypted(_) => {
            println!("  BLS_PASSPHRASE=<bls-passphrase> outbe-cli --private-key <evm-key> staking stake \\");
            println!("    --validator {} \\", bundle.validator_address);
            println!("    --amount <amount> \\");
            println!("    --keys-dir {} \\", output_dir.display());
            println!("    --bls-key-backend encrypted");
        }
        KeyBackend::OsLevel => {
            println!("  outbe-cli --private-key <evm-key> staking stake \\");
            println!("    --validator {} \\", bundle.validator_address);
            println!("    --amount <amount> \\");
            println!("    --keys-dir {} \\", output_dir.display());
            println!("    --bls-key-backend os-level");
        }
    }
    println!(
        "  registration signature: {}",
        hex::encode(bundle.registration_signature)
    );
    println!();
    println!("node startup flags:");
    println!(
        "  --validator --consensus.signing-key {} --validator.evm-key {}",
        bundle.bls_key_path.display(),
        bundle.evm_key_path.display()
    );

    Ok(())
}

/// Verify the integrity of a BLS key file.
fn cmd_verify(key_path: PathBuf, backend: &KeyBackend) -> Result<()> {
    cmd_verify_with_roundtrip(key_path, backend, |private_key, public_key| {
        use commonware_cryptography::Verifier as _;
        let signature = private_key.sign(b"outbe-verify", b"test-message");
        public_key.verify(b"outbe-verify", b"test-message", &signature)
    })
}

fn cmd_verify_with_roundtrip(
    key_path: PathBuf,
    backend: &KeyBackend,
    roundtrip: impl FnOnce(&bls12381::PrivateKey, &bls12381::PublicKey) -> bool,
) -> Result<()> {
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
    let public_key = key.public_key();
    let pk_bytes = public_key.encode();
    println!("[OK]  public key derivable: {}", hex::encode(&pk_bytes));

    // 3. Test sign + verify roundtrip.
    if roundtrip(&key, &public_key) {
        println!("[OK]  sign/verify roundtrip passed");
    } else {
        println!("[FAIL] sign/verify roundtrip failed");
        exit_process(1);
    }

    println!("\nkey verification: PASSED");
    Ok(())
}

/// Generate both an ECDSA (secp256k1) private key and a BLS12-381 keypair.
fn cmd_hybrid(output_dir: PathBuf, backend: &KeyBackend, force: bool) -> Result<()> {
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let bls_key_path = output_dir.join("signing-key.hex");
    let evm_key_path = output_dir.join("evm-key.hex");
    ensure_can_write(&[&bls_key_path, &evm_key_path], force)?;

    // 1. Generate BLS12-381 keypair.
    let bls_key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let bls_pk = bls_key.public_key();
    let bls_pk_bytes = bls_pk.encode();
    let bls_pubkey_hash = keccak256(&bls_pk_bytes);

    bls::save_individual_key(&bls_key_path, &bls_key, backend)
        .wrap_err_with(|| format!("failed to write BLS key: {}", bls_key_path.display()))?;

    // 2. Generate ECDSA secp256k1 private key (32 random bytes).
    // ECDSA key is always plaintext hex (not BLS, not affected by backend).
    // `as_slice` borrows the secret. `*ecdsa_bytes` would copy the Copy array
    // out of `Zeroizing`.
    let mut ecdsa_bytes = Zeroizing::new([0u8; 32]);
    rand_core::OsRng.fill_bytes(ecdsa_bytes.as_mut());
    if ecdsa_bytes.iter().all(|&b| b == 0) {
        return Err(KeygenError::ZeroEcdsaKey.into());
    }

    let ecdsa_key_hex = Zeroizing::new(hex::encode(ecdsa_bytes.as_slice()));
    write_secret_hex_file(&evm_key_path, &ecdsa_key_hex)
        .wrap_err_with(|| format!("failed to write EVM key: {}", evm_key_path.display()))?;

    println!("hybrid keypair generated");
    println!();
    println!("BLS12-381:");
    println!("  private key:  {}", bls_key_path.display());
    println!("  public key:   {}", hex::encode(&bls_pk_bytes));
    println!("  pubkey hash:  {bls_pubkey_hash}");
    println!();
    println!("EVM artifact signer (secp256k1):");
    println!("  private key:  {}", evm_key_path.display());
    println!("  use this path with --validator.evm-key");

    Ok(())
}

fn write_secret_hex_file(path: &Path, contents: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| KeygenError::SecretKeyPathMissingFileName(path.to_path_buf()))?
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
        }
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .wrap_err_with(|| format!("failed to sync parent directory: {}", parent.display()))?;
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
mod tests;
