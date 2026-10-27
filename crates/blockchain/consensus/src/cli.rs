//! CLI subcommands for the Outbe consensus layer.
//!
//! Provides the `dkg` subcommand for bootstrapping BLS threshold key material,
//! and emergency DKG management commands (status, export-share, import-share).

use alloy_primitives::{keccak256, Address};
use commonware_codec::Encode;
use commonware_cryptography::Signer as _;
use commonware_math::algebra::Random as _;
use eyre::{Result, WrapErr};
use k256::ecdsa::{SigningKey, VerifyingKey};
use std::path::{Path, PathBuf};

use crate::{
    bls::{self, KeyBackend},
    dkg,
    errors::ConsensusCliError,
};

const LOCALNET_RETH_BOOTNODES_FILE: &str = "reth-bootnodes.txt";
const LOCALNET_RETH_P2P_SECRET_FILE: &str = "reth-p2p-secret.hex";
const LOCALNET_RETH_BOOTNODE_HOST: &str = "127.0.0.1";
const LOCALNET_RETH_P2P_BASE_PORT: u16 = 30303;

/// Independent custody selection for BLS, validator EVM, and Reth P2P.
#[derive(Clone, Copy)]
pub struct RoleKeyBackends<'a> {
    pub bls: &'a KeyBackend,
    pub evm: &'a KeyBackend,
    pub reth: &'a KeyBackend,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct ValidatorEntry {
    public_key: String,
    address: String,
    p2p_address: Option<String>,
}

fn reth_node_id_hex(verifying_key: &VerifyingKey) -> Result<String> {
    let point = verifying_key.to_encoded_point(false);
    let bytes = point.as_bytes();
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err(ConsensusCliError::UnexpectedSecp256k1Encoding.into());
    }
    Ok(hex::encode(&bytes[1..]))
}

/// Execute the DKG bootstrap command.
///
/// Generates BLS threshold key material for `num_validators` validators:
/// - Shared public polynomial
/// - Per-validator signing keys and threshold shares
/// - A `validators.json` tooling file for genesis seeding and localnet scripts
pub fn execute_dkg_bootstrap(
    output_dir: PathBuf,
    num_validators: u32,
    roles: &RoleKeyBackends<'_>,
) -> Result<()> {
    println!("bootstrapping DKG for {num_validators} validators...");

    let result = dkg::bootstrap_and_save(&output_dir, num_validators, roles.bls)
        .wrap_err("DKG bootstrap failed")?;

    // Generate validators.json and secp256k1 EVM/Reth keys.
    let entries = write_validator_identity_bundle(&output_dir, &result.validator_dirs, roles)?;

    println!("DKG bootstrap complete:");
    println!("  output dir:     {}", output_dir.display());
    println!(
        "  polynomial:     {}",
        output_dir.join(dkg::ROOT_POLYNOMIAL_FILE).display()
    );
    println!(
        "  dkg output:     {}",
        output_dir.join(dkg::KEYS_DIR_OUTPUT_FILE).display()
    );
    print_validator_identity_outputs(&output_dir, &result.validator_dirs, &entries);

    Ok(())
}

/// Generate the long-lived validator identities needed by the interactive
/// round-0 genesis DKG without creating a second, incompatible centralized
/// threshold polynomial.
pub fn execute_validator_identities(
    output_dir: PathBuf,
    num_validators: u32,
    roles: &RoleKeyBackends<'_>,
) -> Result<()> {
    if num_validators == 0 {
        return Err(ConsensusCliError::ZeroValidatorCount.into());
    }
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let mut keys: Vec<commonware_cryptography::bls12381::PrivateKey> = (0..num_validators)
        .map(|_| {
            commonware_cryptography::bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    keys.sort_by_key(|key| key.public_key().encode());

    let mut validator_dirs = Vec::with_capacity(num_validators as usize);
    for (index, key) in keys.iter().enumerate() {
        let directory = output_dir.join(format!("validator-{index}"));
        std::fs::create_dir_all(&directory).wrap_err_with(|| {
            format!(
                "failed to create validator directory: {}",
                directory.display()
            )
        })?;
        bls::save_individual_key(&directory.join("signing-key.hex"), key, roles.bls)
            .wrap_err_with(|| format!("failed to save validator-{index} signing key"))?;
        validator_dirs.push(directory);
    }

    let entries = write_validator_identity_bundle(&output_dir, &validator_dirs, roles)?;
    println!("validator identities generated for fresh interactive genesis DKG:");
    println!("  output dir:     {}", output_dir.display());
    print_validator_identity_outputs(&output_dir, &validator_dirs, &entries);
    Ok(())
}

/// Verify that imported founder private keys reproduce the public identities
/// committed by validators.json before network preparation consumes them.
pub fn execute_validator_identity_verification(
    validators_path: &Path,
    material_dir: &Path,
    roles: &RoleKeyBackends<'_>,
) -> Result<()> {
    let validators: Vec<ValidatorEntry> = serde_json::from_slice(
        &std::fs::read(validators_path)
            .wrap_err_with(|| format!("failed to read: {}", validators_path.display()))?,
    )
    .wrap_err_with(|| format!("failed to parse: {}", validators_path.display()))?;
    if validators.is_empty() {
        return Err(ConsensusCliError::EmptyValidatorsJson.into());
    }

    for (index, validator) in validators.iter().enumerate() {
        let expected_public_key = hex::decode(
            validator
                .public_key
                .strip_prefix("0x")
                .unwrap_or(&validator.public_key),
        )
        .wrap_err_with(|| format!("validator-{index} public_key is not valid hex"))?;
        if expected_public_key.len() != 48 {
            return Err(ConsensusCliError::InvalidPublicKeyLength(index).into());
        }

        let signing_key_path = material_dir
            .join(format!("validator-{index}"))
            .join("signing-key.hex");
        let signing_key = bls::load_individual_key(&signing_key_path, roles.bls)
            .wrap_err_with(|| format!("failed to load validator-{index} BLS signing key"))?;
        if signing_key.public_key().encode() != expected_public_key {
            return Err(ConsensusCliError::BlsSigningKeyMismatch(index).into());
        }

        let expected_address: Address = validator
            .address
            .parse()
            .wrap_err_with(|| format!("validator-{index} address is invalid"))?;
        let evm_key_path = material_dir
            .join(format!("validator-{index}"))
            .join("evm-key.hex");
        let evm_signer = bls::load_validator_evm_signer(&evm_key_path, roles.evm)
            .wrap_err_with(|| format!("failed to load validator-{index} EVM signing key"))?;
        if evm_signer.address() != expected_address {
            return Err(ConsensusCliError::EvmSigningKeyMismatch(index).into());
        }
    }

    println!(
        "verified {} imported founder identities against {}",
        validators.len(),
        validators_path.display()
    );
    Ok(())
}

fn write_validator_identity_bundle(
    output_dir: &Path,
    validator_dirs: &[PathBuf],
    roles: &RoleKeyBackends<'_>,
) -> Result<Vec<ValidatorEntry>> {
    let mut entries = Vec::with_capacity(validator_dirs.len());
    let mut reth_bootnodes = Vec::with_capacity(validator_dirs.len());
    for (i, dir) in validator_dirs.iter().enumerate() {
        let key = bls::load_individual_key(&dir.join("signing-key.hex"), roles.bls)
            .wrap_err_with(|| format!("failed to load key for validator {i}"))?;
        let pk = key.public_key();
        let pk_hex = hex::encode(pk.encode());

        // Generate secp256k1 EVM key and derive Ethereum address.
        let evm_key = SigningKey::random(&mut rand_core::OsRng);
        let evm_pubkey = evm_key.verifying_key().to_encoded_point(false);
        let addr_hash = keccak256(&evm_pubkey.as_bytes()[1..]);
        let address = Address::from_slice(&addr_hash[12..]);

        // Save the EVM private key with the selected EVM backend.
        let mut evm_secret = [0_u8; 32];
        evm_secret.copy_from_slice(evm_key.to_bytes().as_slice());
        let evm_key_path = dir.join("evm-key.hex");
        bls::save_secp256k1_secret(&evm_key_path, &evm_secret, roles.evm)
            .wrap_err_with(|| format!("failed to write EVM key: {}", evm_key_path.display()))?;

        // Generate a stable Reth RLPx identity for localnet bootnode wiring.
        // This key is not consensus-critical. It only lets scripts know each
        // node's enode before the first Reth startup.
        let reth_p2p_key = SigningKey::random(&mut rand_core::OsRng);
        let mut reth_secret = [0_u8; 32];
        reth_secret.copy_from_slice(reth_p2p_key.to_bytes().as_slice());
        let reth_p2p_key_path = dir.join(LOCALNET_RETH_P2P_SECRET_FILE);
        bls::save_secp256k1_secret(&reth_p2p_key_path, &reth_secret, roles.reth).wrap_err_with(
            || {
                format!(
                    "failed to write Reth p2p secret: {}",
                    reth_p2p_key_path.display()
                )
            },
        )?;
        let reth_node_id = reth_node_id_hex(reth_p2p_key.verifying_key())?;
        let reth_p2p_port = LOCALNET_RETH_P2P_BASE_PORT
            .checked_add(i as u16)
            .ok_or(ConsensusCliError::TooManyLocalnetValidators)?;
        reth_bootnodes.push(format!(
            "enode://{reth_node_id}@{LOCALNET_RETH_BOOTNODE_HOST}:{reth_p2p_port}"
        ));

        let p2p_port = 30400 + i as u16;
        entries.push(ValidatorEntry {
            public_key: pk_hex,
            address: format!("{address}"),
            p2p_address: Some(format!("127.0.0.1:{p2p_port}")),
        });
    }

    let validators_path = output_dir.join("validators.json");
    let json =
        serde_json::to_string_pretty(&entries).wrap_err("failed to serialize validators.json")?;
    std::fs::write(&validators_path, json)
        .wrap_err_with(|| format!("failed to write: {}", validators_path.display()))?;

    let reth_bootnodes_path = output_dir.join(LOCALNET_RETH_BOOTNODES_FILE);
    std::fs::write(
        &reth_bootnodes_path,
        format!("{}\n", reth_bootnodes.join("\n")),
    )
    .wrap_err_with(|| format!("failed to write: {}", reth_bootnodes_path.display()))?;

    Ok(entries)
}

fn print_validator_identity_outputs(
    output_dir: &Path,
    validator_dirs: &[PathBuf],
    entries: &[ValidatorEntry],
) {
    let validators_path = output_dir.join("validators.json");
    let reth_bootnodes_path = output_dir.join(LOCALNET_RETH_BOOTNODES_FILE);
    println!("  validators:     {}", validators_path.display());
    println!("  reth bootnodes: {}", reth_bootnodes_path.display());
    for (i, dir) in validator_dirs.iter().enumerate() {
        let evm_address = entries
            .get(i)
            .map(|entry| entry.address.as_str())
            .unwrap_or("<unknown>");
        println!(
            "  validator-{i}:    {}  (evm-address: {evm_address}, evm-key: {} <redacted>)",
            dir.display(),
            dir.join("evm-key.hex").display()
        );
    }
}

/// DKG state file names (must match the `outbe-engine` `stack::dkg::persistence` constants).
const DKG_SHARE_FILE: &str = "dkg_share.hex";
const DKG_POLYNOMIAL_FILE: &str = "dkg_polynomial.hex";
const DKG_OUTPUT_FILE: &str = "dkg_output.hex";

/// Show the status of DKG key material in a storage directory.
///
/// Checks for the existence and validity of the signing share and public
/// polynomial files. Exits with a human-readable summary.
pub fn execute_dkg_status(storage_dir: &Path, backend: &KeyBackend) -> Result<()> {
    println!("DKG status for: {}", storage_dir.display());
    println!();

    let share_path = storage_dir.join(DKG_SHARE_FILE);
    let poly_path = storage_dir.join(DKG_POLYNOMIAL_FILE);
    let output_path = storage_dir.join(DKG_OUTPUT_FILE);

    let share_ok = report_dkg_file_status(&share_path, "  signing share:  ", || {
        bls::load_signing_share(&share_path, backend)
    });
    let poly_ok = report_dkg_file_status(&poly_path, "  polynomial:     ", || {
        bls::load_public_polynomial(&poly_path, backend)
    });

    println!();
    let output_ok = report_dkg_file_status(&output_path, "  DKG output:     ", || {
        bls::load_dkg_output(&output_path, backend)
    });

    let triplet_ok = if share_ok && poly_ok && output_ok {
        let share = bls::load_signing_share(&share_path, backend)
            .wrap_err("failed to reload signing share for triplet validation")?;
        let polynomial = bls::load_public_polynomial(&poly_path, backend)
            .wrap_err("failed to reload polynomial for triplet validation")?;
        let output = bls::load_dkg_output(&output_path, backend)
            .wrap_err("failed to reload DKG output for triplet validation")?;
        match bls::validate_dkg_triplet(&share, &polynomial, &output) {
            Ok(()) => true,
            Err(e) => {
                println!("  DKG triplet:    INVALID  ({e})");
                false
            }
        }
    } else {
        false
    };

    if triplet_ok {
        println!("  status: READY - threshold material is valid");
    } else {
        println!("  status: NOT READY - missing or invalid key material");
    }

    Ok(())
}

fn report_dkg_file_status<T>(path: &Path, label: &str, load: impl FnOnce() -> Result<T>) -> bool {
    if !path.exists() {
        println!("{label}MISSING");
        return false;
    }
    match load() {
        Ok(_) => {
            println!("{label}OK  ({})", path.display());
            true
        }
        Err(error) => {
            println!("{label}INVALID  ({error})");
            false
        }
    }
}

/// Export the DKG signing share from the storage directory to a specified path.
pub fn execute_dkg_export_share(
    storage_dir: &Path,
    output: &Path,
    backend: &KeyBackend,
) -> Result<()> {
    let share_path = storage_dir.join(DKG_SHARE_FILE);
    let poly_path = storage_dir.join(DKG_POLYNOMIAL_FILE);
    let output_path = storage_dir.join(DKG_OUTPUT_FILE);

    if !share_path.exists() {
        return Err(
            ConsensusCliError::SigningShareNotFound(share_path.display().to_string()).into(),
        );
    }
    if !poly_path.exists() {
        return Err(ConsensusCliError::PolynomialNotFound(poly_path.display().to_string()).into());
    }
    if !output_path.exists() {
        return Err(ConsensusCliError::DkgOutputNotFound(output_path.display().to_string()).into());
    }

    // Validate before exporting.
    let share =
        bls::load_signing_share(&share_path, backend).wrap_err("signing share is corrupted")?;
    let polynomial =
        bls::load_public_polynomial(&poly_path, backend).wrap_err("polynomial is corrupted")?;
    let output_artifact =
        bls::load_dkg_output(&output_path, backend).wrap_err("DKG output is corrupted")?;
    bls::validate_dkg_triplet(&share, &polynomial, &output_artifact)
        .wrap_err("DKG triplet is inconsistent")?;

    std::fs::create_dir_all(output)
        .wrap_err_with(|| format!("failed to create output dir: {}", output.display()))?;

    std::fs::copy(&share_path, output.join(DKG_SHARE_FILE))
        .wrap_err("failed to copy signing share")?;
    std::fs::copy(&poly_path, output.join(DKG_POLYNOMIAL_FILE))
        .wrap_err("failed to copy polynomial")?;
    std::fs::copy(&output_path, output.join(DKG_OUTPUT_FILE))
        .wrap_err("failed to copy DKG output")?;

    println!("exported DKG material to: {}", output.display());
    println!(
        "  {}  ->  {}",
        DKG_SHARE_FILE,
        output.join(DKG_SHARE_FILE).display()
    );
    println!(
        "  {}  ->  {}",
        DKG_POLYNOMIAL_FILE,
        output.join(DKG_POLYNOMIAL_FILE).display()
    );
    println!(
        "  {}  ->  {}",
        DKG_OUTPUT_FILE,
        output.join(DKG_OUTPUT_FILE).display()
    );

    Ok(())
}

/// Force-restart DKG by removing saved threshold material.
///
/// Deletes only the consensus signing share, polynomial, and DKG output files.
///
/// This maintenance command never touches enclave state or the permanent offer
/// key. The next node startup still passes the normal founding or existing-chain
/// gates. An existing identity without its exact offer key remains terminal.
pub fn execute_dkg_force_restart(storage_dir: &Path) -> Result<()> {
    let material_paths = [
        storage_dir.join(DKG_SHARE_FILE),
        storage_dir.join(DKG_POLYNOMIAL_FILE),
        storage_dir.join(DKG_OUTPUT_FILE),
    ];

    let mut removed = false;

    for material_path in material_paths {
        if material_path.exists() {
            std::fs::remove_file(&material_path)
                .wrap_err_with(|| format!("failed to remove: {}", material_path.display()))?;
            println!("  removed: {}", material_path.display());
            removed = true;
        }
    }

    if removed {
        println!();
        println!(
            "Consensus DKG material removed. Permanent TEE offer-key state was not modified; normal startup gates still apply."
        );
    } else {
        println!("No DKG material found in: {}", storage_dir.display());
    }

    Ok(())
}

/// Import DKG signing share and polynomial into the storage directory.
pub fn execute_dkg_import_share(
    share_file: &Path,
    polynomial_file: &Path,
    output_file: Option<&Path>,
    storage_dir: &Path,
    backend: &KeyBackend,
) -> Result<()> {
    let inferred_output_file;
    let output_file = match output_file {
        Some(path) => path,
        None => {
            inferred_output_file = share_file
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(DKG_OUTPUT_FILE);
            inferred_output_file.as_path()
        }
    };

    // Validate the source files before importing.
    let share = bls::load_signing_share(share_file, backend)
        .wrap_err_with(|| format!("invalid signing share: {}", share_file.display()))?;
    let polynomial = bls::load_public_polynomial(polynomial_file, backend)
        .wrap_err_with(|| format!("invalid polynomial: {}", polynomial_file.display()))?;
    let output_artifact = bls::load_dkg_output(output_file, backend)
        .wrap_err_with(|| format!("invalid DKG output: {}", output_file.display()))?;
    bls::validate_dkg_triplet(&share, &polynomial, &output_artifact)
        .wrap_err("DKG import triplet is inconsistent")?;

    std::fs::create_dir_all(storage_dir)
        .wrap_err_with(|| format!("failed to create storage dir: {}", storage_dir.display()))?;

    std::fs::copy(share_file, storage_dir.join(DKG_SHARE_FILE))
        .wrap_err("failed to copy signing share")?;
    std::fs::copy(polynomial_file, storage_dir.join(DKG_POLYNOMIAL_FILE))
        .wrap_err("failed to copy polynomial")?;
    std::fs::copy(output_file, storage_dir.join(DKG_OUTPUT_FILE))
        .wrap_err("failed to copy DKG output")?;

    println!("imported DKG material into: {}", storage_dir.display());
    println!(
        "  share:       {} -> {}",
        share_file.display(),
        storage_dir.join(DKG_SHARE_FILE).display()
    );
    println!(
        "  polynomial:  {} -> {}",
        polynomial_file.display(),
        storage_dir.join(DKG_POLYNOMIAL_FILE).display()
    );
    println!(
        "  DKG output:  {} -> {}",
        output_file.display(),
        storage_dir.join(DKG_OUTPUT_FILE).display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reth_node_id_is_uncompressed_public_key_without_prefix() {
        let key = SigningKey::from_bytes((&[7u8; 32]).into()).unwrap();
        let node_id = reth_node_id_hex(key.verifying_key()).unwrap();
        let expected_point = key.verifying_key().to_encoded_point(false);
        let expected_bytes = expected_point.as_bytes();
        assert_eq!(expected_bytes[0], 0x04);
        assert_eq!(node_id, hex::encode(&expected_bytes[1..]));
    }

    #[test]
    fn dkg_bootstrap_writes_reth_p2p_secrets_and_bootnodes() {
        let tmp = tempfile::tempdir().unwrap();

        let plain = KeyBackend::Plaintext;
        execute_dkg_bootstrap(
            tmp.path().to_path_buf(),
            3,
            &RoleKeyBackends {
                bls: &plain,
                evm: &plain,
                reth: &plain,
            },
        )
        .unwrap();

        let bootnodes_path = tmp.path().join(LOCALNET_RETH_BOOTNODES_FILE);
        let bootnodes = std::fs::read_to_string(bootnodes_path).unwrap();
        let bootnodes: Vec<_> = bootnodes.lines().collect();
        assert_eq!(bootnodes.len(), 3);

        for (i, bootnode) in bootnodes.iter().enumerate() {
            let port = LOCALNET_RETH_P2P_BASE_PORT + i as u16;
            let Some(rest) = bootnode.strip_prefix("enode://") else {
                panic!("bootnode does not start with enode://: {bootnode}");
            };
            let Some((node_id, addr)) = rest.split_once('@') else {
                panic!("bootnode missing @ separator: {bootnode}");
            };
            assert_eq!(node_id.len(), 128);
            assert_eq!(hex::decode(node_id).unwrap().len(), 64);
            assert_eq!(addr, format!("{LOCALNET_RETH_BOOTNODE_HOST}:{port}"));

            let secret_path = tmp
                .path()
                .join(format!("validator-{i}"))
                .join(LOCALNET_RETH_P2P_SECRET_FILE);
            let secret = std::fs::read_to_string(secret_path).unwrap();
            assert_eq!(secret.len(), 64);
            let secret_bytes = hex::decode(secret).unwrap();
            assert_eq!(secret_bytes.len(), 32);
            let signing_key = SigningKey::from_slice(&secret_bytes).unwrap();
            let expected_node_id = reth_node_id_hex(signing_key.verifying_key()).unwrap();
            assert_eq!(
                node_id, expected_node_id,
                "bootnode node_id must match validator-{i} secret key"
            );
        }
    }

    #[test]
    fn encrypted_evm_custody_round_trips_without_a_plaintext_copy() {
        use crate::errors::BlsKeyError;

        let tmp = tempfile::tempdir().unwrap();
        let plain = KeyBackend::Plaintext;
        let evm = KeyBackend::Encrypted("evm-passphrase-value".to_owned());
        let roles = RoleKeyBackends {
            bls: &plain,
            evm: &evm,
            reth: &plain,
        };
        execute_validator_identities(tmp.path().to_path_buf(), 1, &roles).unwrap();
        execute_validator_identity_verification(
            &tmp.path().join("validators.json"),
            tmp.path(),
            &roles,
        )
        .unwrap();

        let validator = tmp.path().join("validator-0");
        let evm_path = validator.join("evm-key.hex");
        let evm_bytes = std::fs::read(&evm_path).unwrap();
        assert!(evm_bytes.starts_with(b"OUTBE_ENC"));
        let loaded = bls::load_secp256k1_secret(&evm_path, &evm).unwrap();
        let loaded_hex = hex::encode(loaded);
        assert!(!evm_bytes
            .windows(loaded_hex.len())
            .any(|window| window == loaded_hex.as_bytes()));
        assert!(!validator.join("evm-key.hex.plaintext").exists());

        let wrong = KeyBackend::Encrypted("other-passphrase-value".to_owned());
        let error = bls::load_secp256k1_secret(&evm_path, &wrong).unwrap_err();
        assert!(error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<BlsKeyError>(),
                Some(BlsKeyError::DecryptionFailed)
            )
        }));
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("evm-passphrase-value"), "secret leaked");
        assert!(
            !rendered.contains("other-passphrase-value"),
            "secret leaked"
        );

        let mut corrupt = evm_bytes;
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        std::fs::write(&evm_path, &corrupt).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&evm_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let corrupt_error = bls::load_secp256k1_secret(&evm_path, &evm).unwrap_err();
        assert!(corrupt_error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<BlsKeyError>(),
                Some(BlsKeyError::DecryptionFailed)
            )
        }));

        let reth_path = validator.join(LOCALNET_RETH_P2P_SECRET_FILE);
        let reth_text = std::fs::read_to_string(&reth_path).unwrap();
        assert_eq!(reth_text.len(), 64);
        let reth_loaded = bls::load_secp256k1_secret(&reth_path, &plain).unwrap();
        assert_eq!(hex::encode(reth_loaded), reth_text);
        let bls_bytes = std::fs::read(validator.join("signing-key.hex")).unwrap();
        assert!(!bls_bytes.starts_with(b"OUTBE_ENC"));
    }
}
