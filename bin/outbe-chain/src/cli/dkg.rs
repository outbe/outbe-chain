use crate::*;

/// DKG bootstrap subcommand, parsed separately from reth's CLI.
#[derive(clap::Parser)]
#[command(name = "outbe-chain-dkg")]
pub(crate) struct DkgCli {
    /// BLS key storage backend: plaintext, encrypted, or os-level.
    #[arg(long = "bls-key-backend", default_value = "plaintext", global = true)]
    bls_key_backend: String,

    /// Passphrase for the encrypted BLS key backend.
    #[arg(long = "bls-passphrase", env = "BLS_PASSPHRASE", global = true)]
    bls_passphrase: Option<String>,

    /// Validator EVM key storage backend: plaintext, encrypted, or os-level.
    #[arg(long = "evm-key-backend", default_value = "plaintext", global = true)]
    evm_key_backend: String,

    /// Passphrase for the encrypted validator EVM key backend.
    #[arg(long = "evm-passphrase", env = "EVM_PASSPHRASE", global = true)]
    evm_passphrase: Option<String>,

    /// Reth P2P key storage backend: plaintext, encrypted, or os-level.
    #[arg(long = "reth-key-backend", default_value = "plaintext", global = true)]
    reth_key_backend: String,

    /// Passphrase for the encrypted Reth P2P key backend.
    #[arg(long = "reth-passphrase", env = "RETH_P2P_PASSPHRASE", global = true)]
    reth_passphrase: Option<String>,

    #[command(subcommand)]
    command: DkgCommand,
}

#[derive(clap::Subcommand)]
enum DkgCommand {
    /// Generate only the validator identity keys used by a fresh interactive genesis DKG.
    Identities {
        /// Output directory for generated identity key material.
        #[arg(long)]
        output_dir: std::path::PathBuf,

        /// Number of validator identities to generate.
        #[arg(long)]
        validators: u32,
    },
    /// Verify that imported founder private keys match their public validator manifest.
    VerifyIdentities {
        /// Public validators.json manifest.
        #[arg(long)]
        validators: std::path::PathBuf,

        /// Directory containing validator-N/signing-key.hex and evm-key.hex.
        #[arg(long)]
        material_dir: std::path::PathBuf,
    },
    /// Bootstrap DKG material for a validator set.
    Bootstrap {
        /// Output directory for generated key material.
        #[arg(long)]
        output_dir: std::path::PathBuf,

        /// Number of validators to bootstrap.
        #[arg(long)]
        validators: u32,
    },
    /// Show status of DKG key material in a storage directory.
    Status {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
    /// Export DKG signing share, polynomial, and output to a directory.
    ExportShare {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,

        /// Output directory for exported files.
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Import DKG signing share, polynomial, and output into a storage directory.
    ImportShare {
        /// Path to the signing share file.
        #[arg(long)]
        share: std::path::PathBuf,

        /// Path to the public polynomial file.
        #[arg(long)]
        polynomial: std::path::PathBuf,

        /// Path to the DKG output file. Defaults to dkg_output.hex next to --share.
        #[arg(long)]
        output: Option<std::path::PathBuf>,

        /// Storage directory to import into.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
    /// Delete only the local consensus threshold material.
    /// This never modifies or recovers the permanent TEE offer key. Normal
    /// genesis or live-join gates still decide whether startup may proceed.
    ForceRestart {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
}

#[derive(Debug)]
pub(crate) struct ParsedRoleBackends {
    pub(crate) bls: outbe_consensus::bls::KeyBackend,
    pub(crate) evm: outbe_consensus::bls::KeyBackend,
    pub(crate) reth: outbe_consensus::bls::KeyBackend,
}

fn parse_backend(
    name: &str,
    passphrase: Option<String>,
    missing: crate::errors::CliError,
    unknown: impl FnOnce(String) -> crate::errors::CliError,
) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
    use outbe_consensus::bls::{parse_key_backend_name, KeyBackendNameError};
    parse_key_backend_name(name, passphrase).map_err(|error| match error {
        KeyBackendNameError::MissingPassphrase => missing.into(),
        KeyBackendNameError::Unknown(name) => unknown(name).into(),
    })
}

/// Parse DKG CLI's --bls-key-backend into a KeyBackend.
pub(crate) fn parse_dkg_key_backend(
    cli: &DkgCli,
) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
    parse_backend(
        &cli.bls_key_backend,
        cli.bls_passphrase.clone(),
        crate::errors::CliError::MissingBlsPassphrase,
        |backend| crate::errors::CliError::UnknownBlsKeyBackend { backend },
    )
}

/// Parse the three role backends. A missing passphrase fails every subcommand.
pub(crate) fn parse_role_key_backends(cli: &DkgCli) -> eyre::Result<ParsedRoleBackends> {
    Ok(ParsedRoleBackends {
        bls: parse_dkg_key_backend(cli)?,
        evm: parse_backend(
            &cli.evm_key_backend,
            cli.evm_passphrase.clone(),
            crate::errors::CliError::MissingEvmPassphrase,
            |backend| crate::errors::CliError::UnknownEvmKeyBackend { backend },
        )?,
        reth: parse_backend(
            &cli.reth_key_backend,
            cli.reth_passphrase.clone(),
            crate::errors::CliError::MissingRethPassphrase,
            |backend| crate::errors::CliError::UnknownRethKeyBackend { backend },
        )?,
    })
}

/// Handle the `dkg` subcommand.
pub(crate) fn run_dkg_command(args: &[String]) -> eyre::Result<()> {
    // Rebuild args as: "outbe-chain-dkg" "bootstrap" ...remaining...
    let mut dkg_args = vec![args[0].clone()];
    dkg_args.extend_from_slice(&args[2..]);
    let dkg_cli = DkgCli::parse_from(dkg_args);

    let roles = parse_role_key_backends(&dkg_cli)?;
    let view = outbe_consensus::cli::RoleKeyBackends {
        bls: &roles.bls,
        evm: &roles.evm,
        reth: &roles.reth,
    };

    match dkg_cli.command {
        DkgCommand::Identities {
            output_dir,
            validators,
        } => outbe_consensus::cli::execute_validator_identities(output_dir, validators, &view),
        DkgCommand::VerifyIdentities {
            validators,
            material_dir,
        } => outbe_consensus::cli::execute_validator_identity_verification(
            &validators,
            &material_dir,
            &view,
        ),
        DkgCommand::Bootstrap {
            output_dir,
            validators,
        } => outbe_consensus::cli::execute_dkg_bootstrap(output_dir, validators, &view),
        DkgCommand::Status { storage_dir } => {
            outbe_consensus::cli::execute_dkg_status(&storage_dir, &roles.bls)
        }
        DkgCommand::ExportShare {
            storage_dir,
            output,
        } => outbe_consensus::cli::execute_dkg_export_share(&storage_dir, &output, &roles.bls),
        DkgCommand::ImportShare {
            share,
            polynomial,
            output,
            storage_dir,
        } => outbe_consensus::cli::execute_dkg_import_share(
            &share,
            &polynomial,
            output.as_deref(),
            &storage_dir,
            &roles.bls,
        ),
        DkgCommand::ForceRestart { storage_dir } => {
            outbe_consensus::cli::execute_dkg_force_restart(&storage_dir)
        }
    }
}
