//! Consensus CLI arguments.

use std::{net::SocketAddr, path::PathBuf};

/// CLI arguments for the Outbe consensus layer.
#[derive(Debug, Clone, clap::Args)]
pub struct ConsensusArgs {
    /// Run as active consensus participant (validator).
    /// When false, runs as full node (sync + RPC only, no block production).
    #[arg(long = "validator", default_value_t = false)]
    pub is_validator: bool,

    /// Path to the BLS12-381 individual signing key file (32-byte scalar, hex-encoded).
    #[arg(long = "consensus.signing-key", value_name = "PATH")]
    pub signing_key: Option<PathBuf>,

    /// Path to the secp256k1 EVM key used to sign system transaction artifacts.
    /// Defaults to sibling `evm-key.hex` next to `--consensus.signing-key`.
    #[arg(long = "validator.evm-key", value_name = "PATH")]
    pub validator_evm_key: Option<PathBuf>,

    /// Path to the BLS12-381 signing share file (hex-encoded).
    /// Generated via centralized DKG bootstrap and distributed to validators.
    #[arg(long = "consensus.signing-share", value_name = "PATH")]
    pub signing_share: Option<PathBuf>,

    /// Path to the BLS12-381 public polynomial file (hex-encoded).
    /// Used to verify partial signatures from other validators.
    #[arg(long = "consensus.public-polynomial", value_name = "PATH")]
    pub public_polynomial: Option<PathBuf>,

    /// Path to the full DKG output artifact (hex-encoded).
    /// Required with manual share + polynomial provisioning for fresh bootstrap or true reshare continuity.
    #[arg(long = "consensus.dkg-output", value_name = "PATH")]
    pub dkg_output: Option<PathBuf>,

    /// P2P listen address for consensus network.
    #[arg(long = "consensus.listen-addr", default_value = "127.0.0.1:30400")]
    pub listen_address: SocketAddr,

    /// Directory for consensus data storage.
    /// Defaults to `<datadir>/consensus` if not set.
    #[arg(long = "consensus.storage-dir", value_name = "PATH")]
    pub storage_dir: Option<PathBuf>,

    /// Directory for validator key material (DKG shares, polynomials, output).
    /// Defaults to `<datadir>/keys` if not set.
    /// Kept separate from consensus storage so operators can snapshot `data/`
    /// without overwriting per-validator key material.
    #[arg(long = "consensus.keys-dir", value_name = "PATH")]
    pub keys_dir: Option<PathBuf>,

    /// Trust the existing EL head when consensus-finalized height is 0.
    /// Allows DKG re-bootstrap on a node with existing execution state
    /// after a consensus storage wipe. Use only for disaster recovery.
    /// Only allowed on testnet/devnet chains (rejected on mainnet chain_id).
    #[arg(long = "testnet.trust-el-head", default_value_t = false)]
    pub trust_el_head: bool,

    /// Force a fresh DKG ceremony even when execution history exists.
    /// Disaster recovery: use when all validators lost DKG key material.
    /// Requires --testnet.trust-el-head. Only allowed on testnet/devnet chains.
    #[arg(long = "testnet.force-dkg", default_value_t = false)]
    pub force_dkg: bool,

    /// Comma-separated list of bootstrap peers for P2P discovery.
    /// Format: `<hex_bls_pubkey>@<host:port>` (e.g. `aabb...ff@1.2.3.4:30400`).
    /// Used only as a bootstrap/discovery hint. Validator membership and target
    /// P2P addresses are read from chain state.
    #[arg(long = "consensus.peers", value_delimiter = ',', value_name = "PEER")]
    pub consensus_peers: Vec<String>,

    /// Use P2P defaults optimized for local network environments.
    ///
    /// Production/default mode uses Commonware's recommended authenticated lookup
    /// settings. Local testnets should pass this flag to allow private IPs and
    /// faster peer redial/ping timings.
    #[arg(long = "consensus.use-local-defaults", default_value_t = false)]
    pub use_local_defaults: bool,

    /// Time (ms) to prepare proposal transactions before resolving payload.
    /// Mirrors Tempo's `--consensus.time-to-prepare-proposal-transactions`.
    #[arg(long = "consensus.payload-resolve-time-ms", default_value_t = 200)]
    pub payload_resolve_time_ms: u64,

    /// Minimum time (ms) before sending a proposal (keeps block times stable).
    /// Mirrors Tempo's `--consensus.minimum-time-before-propose`.
    #[arg(long = "consensus.payload-return-time-ms", default_value_t = 450)]
    pub payload_return_time_ms: u64,

    // Simplex leader / certification timeouts are NOT CLI flags. They are
    // consensus-critical and must be identical across all validators, so the
    // only sources of truth are the `outbe_consensus::timing` defaults and
    // `genesis.json` (`leaderTimeoutMs` / `certificationTimeoutMs`). A per-node
    // CLI override could desync timings and fork the network.
    /// Number of worker threads for the consensus runtime.
    #[arg(long = "consensus.worker-threads", default_value_t = 3)]
    pub worker_threads: usize,

    /// BLS key storage backend: plaintext, encrypted, or os-level.
    /// - `plaintext`: hex files on disk (default, suitable for development)
    /// - `encrypted`: AES-256-GCM + Argon2id; requires --bls-passphrase
    /// - `os-level`: macOS Keychain / Linux Secret Service
    #[arg(
        long = "bls-key-backend",
        default_value = "plaintext",
        value_name = "BACKEND"
    )]
    pub bls_key_backend: String,

    /// Passphrase for the `encrypted` BLS key backend.
    /// Can also be provided via the BLS_PASSPHRASE environment variable.
    #[arg(long = "bls-passphrase", env = "BLS_PASSPHRASE", value_name = "SECRET")]
    pub bls_passphrase: Option<String>,

    /// Path to the TEE enclave Unix socket (the `outbe-tee-enclave` sidecar).
    /// When set, the node connects to the enclave at startup, verifies its
    /// attested quote, and pins its Noise-IK static key. A validator with this
    /// flag set requires a healthy, attested enclave to start (fail-fast).
    /// When unset, the node runs with the in-process TEE stub (dev only).
    #[arg(long = "tee-enclave-socket", value_name = "PATH")]
    pub tee_enclave_socket: Option<PathBuf>,

    /// Local liveness deadline (seconds) for the one-time TEE DKG + bootstrap on a
    /// fresh chain (block 0). The whole ceremony must finish before block 1; if it
    /// times out (or fails), node startup fails fast and the node halts rather than
    /// proceeding into a permanently un-bootstrapped chain. Local only — not a
    /// consensus rule.
    #[arg(
        long = "tee-bootstrap-timeout-secs",
        value_name = "SECS",
        default_value_t = 60
    )]
    pub tee_bootstrap_timeout_secs: u64,

    /// Run as a FOLLOWER: cold-sync finalized blocks from this upstream node and
    /// verify them against the trusted network identity, instead of running the
    /// consensus engine. The lightweight full-node path. Mutually exclusive with
    /// `--validator`.
    #[arg(long = "upstream", value_name = "URL", conflicts_with = "is_validator")]
    pub upstream: Option<String>,

    /// Trusted network identity: the BLS12-381 threshold group public key (hex)
    /// the follower anchors finalization verification on. Overrides the genesis
    /// `networkIdentity` config value.
    #[arg(long = "consensus.network-identity", value_name = "HEX")]
    pub network_identity: Option<String>,

    /// Epoch from which `--consensus.network-identity` is valid.
    #[arg(
        long = "consensus.network-identity-from-epoch",
        value_name = "EPOCH",
        default_value_t = 0
    )]
    pub network_identity_from_epoch: u64,

    /// Dev only: follow without verifying consensus certificates (EL-only sync).
    /// Requires `--upstream`.
    #[arg(long = "upstream.nocertify", default_value_t = false, requires = "upstream")]
    pub upstream_nocertify: bool,
}

impl ConsensusArgs {
    /// Validate argument consistency.
    ///
    /// - `--validator` without `--consensus.signing-key` → error
    /// - `--consensus.signing-key` without `--validator` → warning (ignored key)
    /// - `--bls-key-backend encrypted` without `--bls-passphrase` → error
    pub fn validate(&self) -> eyre::Result<()> {
        // Follower mode (`--upstream`) is the lightweight full-node path and must
        // not be combined with validator/consensus participation. (clap's
        // `conflicts_with` also enforces this on the CLI; this covers programmatic
        // construction and gives a clear message.)
        if self.upstream.is_some() && self.is_validator {
            eyre::bail!(
                "--upstream (follower mode) is mutually exclusive with --validator"
            );
        }
        if self.upstream_nocertify && self.upstream.is_none() {
            eyre::bail!("--upstream.nocertify requires --upstream");
        }
        if self.is_validator && self.signing_key.is_none() {
            eyre::bail!(
                "--validator requires --consensus.signing-key. \
                 Provide the path to your BLS signing key file."
            );
        }
        if !self.is_validator && self.signing_key.is_some() {
            tracing::warn!(
                "--consensus.signing-key provided without --validator; \
                 the signing key will be ignored. Add --validator to run as a validator."
            );
        }
        if !self.is_validator && self.validator_evm_key.is_some() {
            tracing::warn!(
                "--validator.evm-key provided without --validator; \
                 the EVM signer key will be ignored. Add --validator to run as a validator."
            );
        }
        // Two valid manual-provisioning shapes:
        //   * signer triplet: all of signing-share + public-polynomial + dkg-output.
        //   * verifier-join pair: public-polynomial + dkg-output WITHOUT signing-share
        //     — a node joining a running chain that has no threshold share yet; it runs
        //     the consensus engine in verifier (follow/verify) mode and acquires a share
        //     at the next DKG reshare. Any other partial combination is an error.
        let (share, poly, output) = (
            self.signing_share.is_some(),
            self.public_polynomial.is_some(),
            self.dkg_output.is_some(),
        );
        let signer_triplet = share && poly && output;
        let verifier_pair = !share && poly && output;
        if (share || poly || output) && !signer_triplet && !verifier_pair {
            eyre::bail!(
                "manual DKG provisioning requires either all of --consensus.signing-share, \
                 --consensus.public-polynomial, --consensus.dkg-output (signer), or \
                 --consensus.public-polynomial + --consensus.dkg-output without \
                 --consensus.signing-share (verifier-join)."
            );
        }
        if self.bls_key_backend == "encrypted" && self.bls_passphrase.is_none() {
            eyre::bail!(
                "--bls-key-backend encrypted requires --bls-passphrase or BLS_PASSPHRASE env var."
            );
        }
        Ok(())
    }

    /// Effective validator EVM-key path.
    ///
    /// Returns `None` for full-node mode. In validator mode, an explicit
    /// `--validator.evm-key` wins; otherwise the default is sibling
    /// `evm-key.hex` next to `--consensus.signing-key`.
    pub fn effective_validator_evm_key(&self) -> eyre::Result<Option<PathBuf>> {
        if !self.is_validator {
            return Ok(None);
        }
        if let Some(path) = &self.validator_evm_key {
            return Ok(Some(path.clone()));
        }
        let Some(signing_key) = &self.signing_key else {
            eyre::bail!("--validator requires --consensus.signing-key before deriving default --validator.evm-key")
        };
        Ok(Some(
            signing_key
                .parent()
                .map(|parent| parent.join("evm-key.hex"))
                .unwrap_or_else(|| PathBuf::from("evm-key.hex")),
        ))
    }

    /// Parse the `--bls-key-backend` argument into a [`KeyBackend`].
    pub fn key_backend(&self) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
        match self.bls_key_backend.as_str() {
            "plaintext" => Ok(outbe_consensus::bls::KeyBackend::Plaintext),
            "encrypted" => {
                let passphrase = self
                    .bls_passphrase
                    .clone()
                    .ok_or_else(|| eyre::eyre!("encrypted backend requires passphrase"))?;
                Ok(outbe_consensus::bls::KeyBackend::Encrypted(passphrase))
            }
            "os-level" => Ok(outbe_consensus::bls::KeyBackend::OsLevel),
            other => eyre::bail!(
                "unknown BLS key backend: {other} (expected: plaintext, encrypted, os-level)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestConsensusCli {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }

    fn default_args() -> ConsensusArgs {
        ConsensusArgs {
            is_validator: false,
            signing_key: None,
            validator_evm_key: None,
            signing_share: None,
            public_polynomial: None,
            dkg_output: None,
            listen_address: "127.0.0.1:30400".parse().unwrap(),
            storage_dir: None,
            keys_dir: None,
            trust_el_head: false,
            force_dkg: false,
            consensus_peers: vec![],
            use_local_defaults: false,
            payload_resolve_time_ms: 200,
            payload_return_time_ms: 450,
            worker_threads: 3,
            bls_key_backend: "plaintext".to_string(),
            bls_passphrase: None,
            tee_enclave_socket: None,
            tee_bootstrap_timeout_secs: 60,
            upstream: None,
            network_identity: None,
            network_identity_from_epoch: 0,
            upstream_nocertify: false,
        }
    }

    #[test]
    fn test_full_node_without_key_ok() {
        assert!(default_args().validate().is_ok());
    }

    #[test]
    fn test_follower_upstream_ok_without_validator() {
        let mut args = default_args();
        args.upstream = Some("http://upstream:8545".to_string());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_follower_upstream_conflicts_with_validator() {
        let mut args = default_args();
        args.upstream = Some("http://upstream:8545".to_string());
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("mutually exclusive"), "error: {err}");
    }

    #[test]
    fn test_nocertify_requires_upstream() {
        let mut args = default_args();
        args.upstream_nocertify = true;
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("--upstream.nocertify requires --upstream"), "error: {err}");
    }

    #[test]
    fn test_validator_without_signing_key_errors() {
        let mut args = default_args();
        args.is_validator = true;
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("--consensus.signing-key"), "error: {err}");
    }

    #[test]
    fn test_validator_with_signing_key_ok() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_manual_dkg_material_requires_complete_triplet() {
        let mut args = default_args();
        args.signing_share = Some(PathBuf::from("/tmp/dkg_share.hex"));
        args.public_polynomial = Some(PathBuf::from("/tmp/dkg_polynomial.hex"));
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("manual DKG provisioning"), "error: {err}");

        args.dkg_output = Some(PathBuf::from("/tmp/dkg_output.hex"));
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_validator_evm_key_default_is_sibling_to_signing_key() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/validator-1/signing-key.hex"));
        assert_eq!(
            args.effective_validator_evm_key().unwrap(),
            Some(PathBuf::from("/tmp/validator-1/evm-key.hex"))
        );
    }

    #[test]
    fn test_validator_evm_key_explicit_wins() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/validator-1/signing-key.hex"));
        args.validator_evm_key = Some(PathBuf::from("/secure/evm.hex"));
        assert_eq!(
            args.effective_validator_evm_key().unwrap(),
            Some(PathBuf::from("/secure/evm.hex"))
        );
    }

    #[test]
    fn test_full_node_ignores_validator_evm_key() {
        let mut args = default_args();
        args.validator_evm_key = Some(PathBuf::from("/secure/evm.hex"));
        assert!(args.validate().is_ok());
        assert_eq!(args.effective_validator_evm_key().unwrap(), None);
    }

    #[test]
    fn test_cli_parses_validator_evm_key() {
        let cli = TestConsensusCli::try_parse_from([
            "test",
            "--validator",
            "--consensus.signing-key",
            "/tmp/signing-key.hex",
            "--validator.evm-key",
            "/tmp/evm-key.hex",
        ])
        .unwrap();
        assert_eq!(
            cli.consensus.validator_evm_key,
            Some(PathBuf::from("/tmp/evm-key.hex"))
        );
    }

    #[test]
    fn test_signing_key_without_validator_warns_but_ok() {
        let mut args = default_args();
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_encrypted_backend_without_passphrase_errors() {
        let mut args = default_args();
        args.bls_key_backend = "encrypted".to_string();
        args.bls_passphrase = None;
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("passphrase"), "error: {err}");
    }

    #[test]
    fn test_encrypted_backend_with_passphrase_ok() {
        let mut args = default_args();
        args.bls_key_backend = "encrypted".to_string();
        args.bls_passphrase = Some("secret".to_string());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_key_backend_parsing() {
        let mut args = default_args();

        args.bls_key_backend = "plaintext".to_string();
        assert!(matches!(
            args.key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Plaintext
        ));

        args.bls_key_backend = "encrypted".to_string();
        args.bls_passphrase = Some("pass".to_string());
        assert!(matches!(
            args.key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Encrypted(_)
        ));

        args.bls_key_backend = "os-level".to_string();
        assert!(matches!(
            args.key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::OsLevel
        ));

        args.bls_key_backend = "invalid".to_string();
        assert!(args.key_backend().is_err());
    }

    #[test]
    fn test_plaintext_backward_compatibility() {
        // Default is plaintext — existing setups continue working.
        let args = default_args();
        assert_eq!(args.bls_key_backend, "plaintext");
        assert!(matches!(
            args.key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Plaintext
        ));
    }

    #[test]
    fn test_p2p_profile_defaults_to_production() {
        let args = default_args();
        assert!(!args.use_local_defaults);
    }

    #[test]
    fn test_removed_fee_recipient_flag_is_rejected() {
        let err = TestConsensusCli::try_parse_from([
            "test",
            "--consensus.fee-recipient",
            "0x0000000000000000000000000000000000000001",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("--consensus.fee-recipient"),
            "unexpected clap error: {err}"
        );
    }

    #[test]
    fn test_removed_validators_flag_is_rejected() {
        let err = TestConsensusCli::try_parse_from([
            "test",
            "--consensus.validators",
            "/tmp/validators.json",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("--consensus.validators"),
            "unexpected clap error: {err}"
        );
    }

    #[test]
    fn test_removed_execution_watchdog_fatal_flag_is_rejected() {
        let err = TestConsensusCli::try_parse_from([
            "test",
            "--consensus.execution-watchdog-fatal-enabled",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("--consensus.execution-watchdog-fatal-enabled"),
            "unexpected clap error: {err}"
        );
    }

    #[test]
    fn test_removed_leader_timeout_flag_is_rejected() {
        // Leader/cert timeouts are genesis-only now; the CLI flags were removed.
        let err =
            TestConsensusCli::try_parse_from(["test", "--consensus.leader-timeout-ms", "30000"])
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("--consensus.leader-timeout-ms"),
            "unexpected clap error: {err}"
        );
    }

    #[test]
    fn test_removed_certification_timeout_flag_is_rejected() {
        let err = TestConsensusCli::try_parse_from([
            "test",
            "--consensus.certification-timeout-ms",
            "30000",
        ])
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("--consensus.certification-timeout-ms"),
            "unexpected clap error: {err}"
        );
    }
}
