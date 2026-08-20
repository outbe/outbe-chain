//! Re-sign existing founder OCOMP registrations for a new genesis hash.
//!
//! This helper deliberately preserves each existing private/public key pair and
//! every validator identity field. It writes only new public registration files.

use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    str::FromStr as _,
};

use alloy_primitives::B256;
use clap::Parser;
use eyre::{ensure, Result, WrapErr as _};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_ocomp_protocol::{committee::OcompKeyRegistrationV1, profile::poc_schema_limits};
use zeroize::Zeroizing;

const KEY_FILE: &str = "ocomp-key-v1.hex";
const REGISTRATION_FILE: &str = "ocomp-registration-v1.ocb1";

#[derive(Debug, Parser)]
#[command(about = "Rebind existing founder OCOMP registrations to a new genesis hash")]
struct Args {
    /// Directory containing validator-N/{ocomp-key-v1.hex,ocomp-registration-v1.ocb1}.
    #[arg(long)]
    source_dir: PathBuf,

    /// New directory to create with validator-N/ocomp-registration-v1.ocb1 outputs.
    #[arg(long)]
    output_dir: PathBuf,

    /// New canonical genesis hash.
    #[arg(long)]
    genesis_hash: String,

    /// Exact founder count.
    #[arg(long, default_value_t = 4)]
    validator_count: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.validator_count > 0, "validator count must be non-zero");
    ensure!(
        !args.output_dir.exists(),
        "refusing to overwrite output directory {}",
        args.output_dir.display()
    );
    let genesis_hash = B256::from_str(&args.genesis_hash)
        .wrap_err("--genesis-hash must be one 32-byte hex value")?;
    ensure!(!genesis_hash.is_zero(), "new genesis hash must be non-zero");

    fs::create_dir(&args.output_dir)
        .wrap_err_with(|| format!("create output directory {}", args.output_dir.display()))?;
    let limits = poc_schema_limits();

    for index in 0..args.validator_count {
        let source = args.source_dir.join(format!("validator-{index}"));
        let old_registration_path = source.join(REGISTRATION_FILE);
        let old_registration = OcompKeyRegistrationV1::decode_canonical(
            &fs::read(&old_registration_path).wrap_err_with(|| {
                format!("read old registration {}", old_registration_path.display())
            })?,
            &limits,
        )
        .wrap_err_with(|| {
            format!(
                "decode old registration {}",
                old_registration_path.display()
            )
        })?;
        old_registration
            .validate_proof_of_possession(&limits)
            .wrap_err_with(|| format!("validate old validator-{index} registration"))?;

        let key_path = source.join(KEY_FILE);
        let secret_hex = Zeroizing::new(
            fs::read_to_string(&key_path)
                .wrap_err_with(|| format!("read existing key {}", key_path.display()))?,
        );
        let secret = Zeroizing::new(
            hex::decode(secret_hex.trim())
                .wrap_err_with(|| format!("decode existing key {}", key_path.display()))?,
        );
        ensure!(
            secret.len() == 32,
            "validator-{index} OCOMP private key is not 32 bytes"
        );
        let signing_key = SigningKey::from_slice(secret.as_slice())
            .wrap_err_with(|| format!("parse validator-{index} OCOMP private key"))?;
        let derived_public: [u8; 33] = signing_key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .map_err(|_| eyre::eyre!("validator-{index} OCOMP public key is not SEC1-33"))?;
        ensure!(
            derived_public == old_registration.core.ocomp_public_key_sec1,
            "validator-{index} private key does not match its existing public registration"
        );
        ensure!(
            old_registration.core.genesis_hash != genesis_hash,
            "validator-{index} registration is already bound to the requested genesis hash"
        );

        let old_core = old_registration.core.clone();
        let mut rebound = old_registration;
        rebound.core.genesis_hash = genesis_hash;
        rebound.proof_of_possession = [0; 64];
        let digest = rebound
            .proof_of_possession_digest(&limits)
            .wrap_err_with(|| format!("derive validator-{index} PoP digest"))?;
        let signature: Signature = signing_key
            .sign_prehash(digest.as_slice())
            .map_err(|_| eyre::eyre!("sign validator-{index} OCOMP PoP"))?;
        rebound.proof_of_possession = signature
            .normalize_s()
            .unwrap_or(signature)
            .to_bytes()
            .into();
        rebound
            .validate_proof_of_possession(&limits)
            .wrap_err_with(|| format!("validate rebound validator-{index} registration"))?;

        ensure!(
            rebound.core.chain_id == old_core.chain_id,
            "chain ID changed"
        );
        ensure!(
            rebound.core.validator_identity_hash == old_core.validator_identity_hash,
            "validator identity changed"
        );
        ensure!(
            rebound.core.ocomp_public_key_sec1 == old_core.ocomp_public_key_sec1,
            "OCOMP public key changed"
        );
        ensure!(
            rebound.core.key_epoch == old_core.key_epoch,
            "key epoch changed"
        );
        ensure!(
            rebound.core.allowed_purpose_bitmap == old_core.allowed_purpose_bitmap,
            "allowed purpose bitmap changed"
        );

        let encoded = rebound
            .encode_canonical(&limits)
            .wrap_err_with(|| format!("encode validator-{index} registration"))?;
        let output = args.output_dir.join(format!("validator-{index}"));
        fs::create_dir(&output)
            .wrap_err_with(|| format!("create output directory {}", output.display()))?;
        write_new(&output.join(REGISTRATION_FILE), &encoded, 0o644)?;
        println!(
            "validator-{index}: rebound registration to {genesis_hash} ({} bytes)",
            encoded.len()
        );
    }

    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .wrap_err_with(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .wrap_err_with(|| format!("write {}", path.display()))?;
    file.sync_all()
        .wrap_err_with(|| format!("fsync {}", path.display()))?;
    Ok(())
}
