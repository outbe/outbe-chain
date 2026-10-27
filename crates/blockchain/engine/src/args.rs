//! Consensus CLI arguments.

use std::{fmt, net::SocketAddr, path::PathBuf};

use outbe_primitives::tee_attestation_v1::AttestationMode;

/// Local authorization protocol used between one node and its enclave.
///
/// This is deliberately independent from [`AttestationMode`]: a development
/// network may run a production NodeHost-authorized enclave inside real SGX
/// while publishing only `GramineDirectDev` evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum TeeSessionMode {
    /// Preserve the historical policy defaults: DCAP uses NodeHost. The
    /// hardware-free development lane uses its separate development transport.
    #[default]
    PolicyDefault,
    /// Require the authenticated, sealed production NodeHost session.
    ProductionNodeHost,
    /// Require the separate development/mock transport.
    Development,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedTeeSession {
    ProductionNodeHost,
    Development,
}

impl TeeSessionMode {
    pub fn resolve(
        self,
        policy: AttestationMode,
    ) -> Result<ResolvedTeeSession, crate::errors::ArgsError> {
        match (self, policy) {
            (Self::PolicyDefault, AttestationMode::DcapRequired)
            | (Self::ProductionNodeHost, _) => Ok(ResolvedTeeSession::ProductionNodeHost),
            (Self::PolicyDefault | Self::Development, AttestationMode::GramineDirectDev) => {
                Ok(ResolvedTeeSession::Development)
            }
            (Self::Development, AttestationMode::DcapRequired) => {
                Err(crate::errors::ArgsError::DcapRequiredForbidsDevSession)
            }
        }
    }
}

/// CLI arguments for the Outbe consensus layer.
#[derive(Clone, clap::Args)]
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
    /// This is consensus-archive recovery after a storage wipe. It never bypasses
    /// the permanent offer-key gate or regenerates a lost enclave identity.
    /// Only allowed on testnet/devnet chains (rejected on mainnet chain_id).
    #[arg(long = "testnet.trust-el-head", default_value_t = false)]
    pub trust_el_head: bool,

    /// Signed proposer clock offset for deterministic local testnet scenarios.
    /// Rejected on mainnet; normal nodes omit it and use the system clock.
    #[arg(long = "testnet.unix-time-offset-secs", value_name = "SECONDS")]
    pub testnet_unix_time_offset_secs: Option<i64>,

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

    /// Time (ms) to prepare proposal transactions before resolving the payload.
    #[arg(long = "consensus.payload-resolve-time-ms", default_value_t = 200)]
    pub payload_resolve_time_ms: u64,

    /// No effect. The node parses this value but does not read it. The genesis
    /// `minBlockTimeMs` value sets the minimum block time.
    #[arg(long = "consensus.payload-return-time-ms", default_value_t = 450)]
    pub payload_return_time_ms: u64,

    // Simplex leader / certification timeouts are NOT CLI flags. They are
    // consensus-critical and must be identical across all validators. Thus the
    // only sources of truth are the `outbe_consensus::timing` defaults and
    // `genesis.json` (`leaderTimeoutMs` / `certificationTimeoutMs`). A per-node
    // CLI override could desync timings and fork the network.
    /// Number of worker threads for the consensus runtime.
    #[arg(long = "consensus.worker-threads", default_value_t = 3)]
    pub worker_threads: usize,

    /// BLS key storage backend: plaintext, encrypted, or os-level.
    /// - `plaintext`: hex files on disk (default, suitable for development)
    /// - `encrypted`: AES-256-GCM + Argon2id, requires --bls-passphrase
    /// - `os-level`: macOS Keychain / Linux Secret Service
    #[arg(
        long = "bls-key-backend",
        default_value = "plaintext",
        value_name = "BACKEND"
    )]
    pub bls_key_backend: String,

    /// Passphrase for the `encrypted` BLS key backend.
    /// You can also provide it through the BLS_PASSPHRASE environment variable.
    #[arg(long = "bls-passphrase", env = "BLS_PASSPHRASE", value_name = "SECRET")]
    pub bls_passphrase: Option<String>,

    /// Validator EVM key storage backend: plaintext, encrypted, or os-level.
    #[arg(
        long = "evm-key-backend",
        default_value = "plaintext",
        value_name = "BACKEND"
    )]
    pub evm_key_backend: String,

    /// Passphrase for the `encrypted` validator EVM key backend.
    /// You can also provide it through the EVM_PASSPHRASE environment variable.
    #[arg(long = "evm-passphrase", env = "EVM_PASSPHRASE", value_name = "SECRET")]
    pub evm_passphrase: Option<String>,

    /// Reth P2P / NodeHost key storage backend: plaintext, encrypted, or os-level.
    #[arg(
        long = "reth-key-backend",
        default_value = "plaintext",
        value_name = "BACKEND"
    )]
    pub reth_key_backend: String,

    /// Passphrase for the `encrypted` Reth P2P key backend.
    /// You can also provide it through the RETH_P2P_PASSPHRASE environment variable.
    #[arg(
        long = "reth-passphrase",
        env = "RETH_P2P_PASSPHRASE",
        value_name = "SECRET"
    )]
    pub reth_passphrase: Option<String>,

    /// Path or `host:port` endpoint for the `outbe-tee-enclave` sidecar.
    /// Every `teeAttestationV1` ChainSpec requires it. The local session selection
    /// is independent from the genesis-fixed attestation policy.
    /// `DcapRequired` always needs NodeHost. `GramineDirectDev` can use
    /// either the development transport or an explicitly selected production
    /// NodeHost session. Missing or rejected transport stops startup and never
    /// selects an in-process stub or another attestation mode.
    #[arg(long = "tee-enclave-socket", value_name = "PATH")]
    pub tee_enclave_socket: Option<PathBuf>,

    /// Local node-to-enclave authorization protocol. This never changes the
    /// genesis-fixed attestation policy and never provides a fallback. Use
    /// `production-node-host` with a GramineDirectDev chain to run the
    /// production enclave under real SGX with remote attestation disabled.
    #[arg(
        long = "tee-session-mode",
        value_enum,
        default_value_t = TeeSessionMode::PolicyDefault
    )]
    pub tee_session_mode: TeeSessionMode,

    /// Local liveness deadline (seconds) for the one-time TEE DKG + bootstrap on a
    /// fresh chain (block 0). The whole ceremony must finish before block 1. If it
    /// times out (or fails), node startup fails fast. The node halts rather than
    /// proceeding into a permanently un-bootstrapped chain. This deadline is local
    /// only. It is not a consensus rule.
    #[arg(
        long = "tee-bootstrap-timeout-secs",
        value_name = "SECS",
        default_value_t = 60
    )]
    pub tee_bootstrap_timeout_secs: u64,

    /// Interval between TEE-enclave canary probes (known-plaintext decrypt +
    /// health telemetry). `0` disables the canary. The canary is a signal only. It
    /// never gates consensus participation.
    #[arg(long = "tee-canary.interval-secs", default_value_t = 30)]
    pub tee_canary_interval_secs: u64,

    /// Consecutive canary failures before `outbe_consensusStatus.enclave`
    /// reports `degraded` (transport-unreachable reports `unavailable`
    /// immediately).
    #[arg(long = "tee-canary.failure-threshold", default_value_t = 3)]
    pub tee_canary_failure_threshold: u64,

    /// Interval, in seconds of canonical block time, between pending-pool
    /// snapshots. A transaction present in two consecutive snapshots stayed
    /// pending for at least one full interval without being mined. The pool
    /// evicts it. Thus the effective pending lifetime is one to two intervals.
    /// This is node-local pool policy. It never affects block validity. Keep the value
    /// uniform across the fleet so every node sheds a stuck transaction at the
    /// same rate.
    #[arg(long = "txpool.outbe.pending-staleness-secs", default_value_t = 600)]
    pub txpool_pending_staleness_secs: u64,

    /// Heartwood native control socket used for bounded identity, configuration,
    /// and topology reconciliation. Required only in validator mode.
    #[arg(long = "radicle.control-socket", value_name = "PATH")]
    pub radicle_control_socket: Option<PathBuf>,

    /// Loopback-only read-only status endpoint exposed by `outbe-radicle`.
    /// Repository availability is observable through this endpoint but never
    /// gates consensus.
    #[arg(long = "radicle.status-address", value_name = "SOCKET")]
    pub radicle_status_address: Option<SocketAddr>,

    /// Run as a FOLLOWER (`--upstream` without `--validator`) or bootstrap a
    /// VALIDATOR (`--validator --upstream`) by syncing finalized blocks from
    /// this upstream node. Each block is verified against the committee,
    /// anchored on the genesis validator set read from local genesis state.
    #[arg(long = "upstream", value_name = "URL")]
    pub upstream: Option<String>,

    /// Dev only: follow without verifying consensus certificates (EL-only sync).
    /// Requires `--upstream`.
    #[arg(
        long = "upstream.nocertify",
        default_value_t = false,
        requires = "upstream"
    )]
    pub upstream_nocertify: bool,
}

impl fmt::Debug for ConsensusArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsensusArgs")
            .field("is_validator", &self.is_validator)
            .field("listen_address", &self.listen_address)
            .field("trust_el_head", &self.trust_el_head)
            .field(
                "testnet_unix_time_offset_secs",
                &self.testnet_unix_time_offset_secs,
            )
            .field("use_local_defaults", &self.use_local_defaults)
            .field("worker_threads", &self.worker_threads)
            .field("bls_key_backend", &self.bls_key_backend)
            .field("bls_passphrase_configured", &self.bls_passphrase.is_some())
            .field("evm_key_backend", &self.evm_key_backend)
            .field("evm_passphrase_configured", &self.evm_passphrase.is_some())
            .field("reth_key_backend", &self.reth_key_backend)
            .field(
                "reth_passphrase_configured",
                &self.reth_passphrase.is_some(),
            )
            .field("tee_enclave_configured", &self.tee_enclave_socket.is_some())
            .field("tee_session_mode", &self.tee_session_mode)
            .field(
                "radicle_control_socket_configured",
                &self.radicle_control_socket.is_some(),
            )
            .field(
                "radicle_status_address_configured",
                &self.radicle_status_address.is_some(),
            )
            .field("upstream_configured", &self.upstream.is_some())
            .finish_non_exhaustive()
    }
}

impl ConsensusArgs {
    /// Validate argument consistency.
    ///
    /// - `--validator` without `--consensus.signing-key` -> error
    /// - `--consensus.signing-key` without `--validator` -> warning (ignored key)
    /// - `--bls-key-backend encrypted` without `--bls-passphrase` -> error
    pub fn validate(&self) -> eyre::Result<()> {
        self.validate_role_and_transport()?;
        self.warn_ignored_signers();
        self.validate_key_material()
    }

    fn validate_role_and_transport(&self) -> eyre::Result<()> {
        if self.txpool_pending_staleness_secs == 0 {
            return Err(crate::errors::ArgsError::PendingStalenessSecsZero.into());
        }
        if self.upstream_nocertify && self.upstream.is_none() {
            return Err(crate::errors::ArgsError::UpstreamNocertifyRequiresUpstream.into());
        }
        if self.is_validator && self.signing_key.is_none() {
            return Err(crate::errors::ArgsError::ValidatorRequiresSigningKey.into());
        }
        self.validate_radicle_coordination()
    }

    fn validate_radicle_coordination(&self) -> eyre::Result<()> {
        if self.is_validator {
            if self.radicle_control_socket.is_none() {
                return Err(crate::errors::ArgsError::ValidatorRequiresRadicleControlSocket.into());
            }
            let status = self
                .radicle_status_address
                .ok_or(crate::errors::ArgsError::ValidatorRequiresRadicleStatusAddress)?;
            if !status.ip().is_loopback() {
                return Err(crate::errors::ArgsError::RadicleStatusAddressNotLoopback.into());
            }
        } else if self.radicle_control_socket.is_some() || self.radicle_status_address.is_some() {
            return Err(crate::errors::ArgsError::RadicleFlagsRequireValidator.into());
        }
        Ok(())
    }

    fn warn_ignored_signers(&self) {
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
    }

    fn validate_key_material(&self) -> eyre::Result<()> {
        if self.bls_key_backend == "encrypted" && self.bls_passphrase.is_none() {
            return Err(crate::errors::ArgsError::EncryptedBackendRequiresPassphraseArg.into());
        }
        if self.evm_key_backend == "encrypted" && self.evm_passphrase.is_none() {
            return Err(crate::errors::ArgsError::EncryptedEvmBackendRequiresPassphraseArg.into());
        }
        if self.reth_key_backend == "encrypted" && self.reth_passphrase.is_none() {
            return Err(crate::errors::ArgsError::EncryptedRethBackendRequiresPassphraseArg.into());
        }
        Ok(())
    }

    /// Effective validator EVM-key path.
    ///
    /// Returns `None` for full-node mode. In validator mode, an explicit
    /// `--validator.evm-key` wins. Otherwise the default is sibling
    /// `evm-key.hex` next to `--consensus.signing-key`.
    pub fn effective_validator_evm_key(&self) -> eyre::Result<Option<PathBuf>> {
        if !self.is_validator {
            return Ok(None);
        }
        if let Some(path) = &self.validator_evm_key {
            return Ok(Some(path.clone()));
        }
        let Some(signing_key) = &self.signing_key else {
            return Err(crate::errors::ArgsError::ValidatorEvmKeyRequiresSigningKey.into());
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
        parse_key_backend(
            &self.bls_key_backend,
            self.bls_passphrase.clone(),
            crate::errors::ArgsError::EncryptedBackendRequiresPassphrase,
            crate::errors::ArgsError::UnknownBlsKeyBackend,
        )
    }

    /// Parse the `--evm-key-backend` argument into a [`KeyBackend`].
    pub fn evm_key_backend(&self) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
        parse_key_backend(
            &self.evm_key_backend,
            self.evm_passphrase.clone(),
            crate::errors::ArgsError::EncryptedEvmBackendRequiresPassphrase,
            crate::errors::ArgsError::UnknownEvmKeyBackend,
        )
    }

    /// Parse the `--reth-key-backend` argument into a [`KeyBackend`].
    pub fn reth_key_backend(&self) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
        parse_key_backend(
            &self.reth_key_backend,
            self.reth_passphrase.clone(),
            crate::errors::ArgsError::EncryptedRethBackendRequiresPassphrase,
            crate::errors::ArgsError::UnknownRethKeyBackend,
        )
    }
}

fn parse_key_backend(
    backend: &str,
    passphrase: Option<String>,
    missing: crate::errors::ArgsError,
    unknown: impl FnOnce(String) -> crate::errors::ArgsError,
) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
    use outbe_consensus::bls::{parse_key_backend_name, KeyBackendNameError};
    parse_key_backend_name(backend, passphrase).map_err(|error| match error {
        KeyBackendNameError::MissingPassphrase => missing.into(),
        KeyBackendNameError::Unknown(name) => unknown(name).into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestConsensusCli {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }

    impl fmt::Debug for TestConsensusCli {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("TestConsensusCli")
                .finish_non_exhaustive()
        }
    }

    fn default_args() -> ConsensusArgs {
        TestConsensusCli::try_parse_from(["test"])
            .expect("default CLI args should parse")
            .consensus
    }

    #[test]
    fn test_full_node_without_key_ok() {
        assert!(default_args().validate().is_ok());
    }

    #[test]
    fn local_tee_session_mode_is_independent_from_attestation_policy() {
        use outbe_primitives::tee_attestation_v1::AttestationMode;

        assert_eq!(
            TeeSessionMode::PolicyDefault
                .resolve(AttestationMode::DcapRequired)
                .unwrap(),
            ResolvedTeeSession::ProductionNodeHost
        );
        assert_eq!(
            TeeSessionMode::PolicyDefault
                .resolve(AttestationMode::GramineDirectDev)
                .unwrap(),
            ResolvedTeeSession::Development
        );
        assert_eq!(
            TeeSessionMode::ProductionNodeHost
                .resolve(AttestationMode::GramineDirectDev)
                .unwrap(),
            ResolvedTeeSession::ProductionNodeHost
        );
        assert_eq!(
            TeeSessionMode::Development
                .resolve(AttestationMode::GramineDirectDev)
                .unwrap(),
            ResolvedTeeSession::Development
        );
        assert!(TeeSessionMode::Development
            .resolve(AttestationMode::DcapRequired)
            .is_err());
    }

    #[test]
    fn debug_output_redacts_operator_secrets() {
        let mut args = default_args();
        args.bls_passphrase = Some("bls-secret-value".to_owned());
        args.evm_passphrase = Some("evm-secret-value".to_owned());
        args.reth_passphrase = Some("reth-secret-value".to_owned());
        args.upstream = Some("https://user:upstream-secret@example.test".to_owned());

        let args_debug = format!("{args:?}");

        for secret in [
            "bls-secret-value",
            "evm-secret-value",
            "reth-secret-value",
            "upstream-secret",
        ] {
            assert!(!args_debug.contains(secret), "secret leaked");
        }
    }

    #[test]
    fn test_follower_upstream_ok_without_validator() {
        let mut args = default_args();
        args.upstream = Some("http://upstream:8545".to_string());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_validator_with_upstream_ok() {
        let mut args = default_args();
        args.upstream = Some("http://upstream:8545".to_string());
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        args.radicle_control_socket = Some(PathBuf::from("/tmp/radicle.sock"));
        args.radicle_status_address = Some("127.0.0.1:8777".parse().unwrap());
        assert!(args.validate().is_ok());

        let cli = TestConsensusCli::try_parse_from([
            "test",
            "--validator",
            "--upstream",
            "http://upstream:8545",
            "--consensus.signing-key",
            "/tmp/signing-key.hex",
            "--radicle.control-socket",
            "/tmp/radicle.sock",
            "--radicle.status-address",
            "127.0.0.1:8777",
        ])
        .unwrap();
        assert!(cli.consensus.is_validator);
        assert_eq!(
            cli.consensus.upstream.as_deref(),
            Some("http://upstream:8545")
        );
        assert!(cli.consensus.validate().is_ok());
    }

    #[test]
    fn test_nocertify_requires_upstream() {
        let mut args = default_args();
        args.upstream_nocertify = true;
        let err = args.validate().unwrap_err().to_string();
        assert!(
            err.contains("--upstream.nocertify requires --upstream"),
            "error: {err}"
        );
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
        args.radicle_control_socket = Some(PathBuf::from("/tmp/radicle.sock"));
        args.radicle_status_address = Some("127.0.0.1:8777".parse().unwrap());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn radicle_flags_are_required_for_validators() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));

        let error = args.validate().unwrap_err().to_string();
        assert!(error.contains("--radicle.control-socket"), "error: {error}");

        args.radicle_control_socket = Some(PathBuf::from("/tmp/radicle.sock"));
        let error = args.validate().unwrap_err().to_string();
        assert!(error.contains("--radicle.status-address"), "error: {error}");
    }

    #[test]
    fn radicle_status_must_be_loopback() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        args.radicle_control_socket = Some(PathBuf::from("/tmp/radicle.sock"));
        args.radicle_status_address = Some("192.0.2.1:8777".parse().unwrap());

        let error = args.validate().unwrap_err().to_string();
        assert!(error.contains("loopback"), "error: {error}");
    }

    #[test]
    fn radicle_flags_are_rejected_outside_validator_mode() {
        let mut args = default_args();
        args.radicle_control_socket = Some(PathBuf::from("/tmp/radicle.sock"));
        assert!(args
            .validate()
            .unwrap_err()
            .to_string()
            .contains("--validator"));

        args.radicle_control_socket = None;
        args.radicle_status_address = Some("127.0.0.1:8777".parse().unwrap());
        assert!(args
            .validate()
            .unwrap_err()
            .to_string()
            .contains("--validator"));

        args.upstream = Some("http://upstream:8545".to_owned());
        assert!(args
            .validate()
            .unwrap_err()
            .to_string()
            .contains("--validator"));
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
    fn test_encrypted_backend_requires_password() {
        let mut args = default_args();
        args.bls_key_backend = "encrypted".to_string();
        args.bls_passphrase = None;
        let err = args.validate().unwrap_err().to_string();
        assert!(err.contains("passphrase"), "error: {err}");

        args.bls_passphrase = Some("secret".to_string());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_key_backend_parsing() {
        let mut args = default_args();
        assert_eq!(args.bls_key_backend, "plaintext");
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
    fn role_backends_parse_independently_and_name_the_missing_passphrase() {
        let mut args = default_args();
        assert_eq!(args.evm_key_backend, "plaintext");
        assert_eq!(args.reth_key_backend, "plaintext");
        assert!(matches!(
            args.evm_key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Plaintext
        ));
        assert!(matches!(
            args.reth_key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Plaintext
        ));

        args.evm_key_backend = "encrypted".to_string();
        let evm_missing = args.evm_key_backend().unwrap_err().to_string();
        assert!(evm_missing.contains("EVM"), "{evm_missing}");
        args.evm_passphrase = Some("evm-secret-value".to_string());
        assert!(matches!(
            args.evm_key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Encrypted(_)
        ));

        args.reth_key_backend = "encrypted".to_string();
        let reth_missing = args.validate().unwrap_err().to_string();
        assert!(
            reth_missing.contains("--reth-key-backend"),
            "{reth_missing}"
        );
        args.reth_passphrase = Some("reth-secret-value".to_string());
        assert!(args.validate().is_ok());
        assert!(matches!(
            args.reth_key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::Encrypted(_)
        ));

        args.reth_key_backend = "os-level".to_string();
        assert!(matches!(
            args.reth_key_backend().unwrap(),
            outbe_consensus::bls::KeyBackend::OsLevel
        ));
        args.evm_key_backend = "nope".to_string();
        let unknown = args.evm_key_backend().unwrap_err().to_string();
        assert!(unknown.contains("EVM"), "{unknown}");
        assert!(!unknown.contains("evm-secret-value"), "secret leaked");
    }

    #[test]
    fn cli_parses_independent_role_backends() {
        let cli = TestConsensusCli::try_parse_from([
            "test",
            "--evm-key-backend",
            "encrypted",
            "--evm-passphrase",
            "evm-secret-value",
            "--reth-key-backend",
            "os-level",
        ])
        .unwrap();
        assert_eq!(cli.consensus.bls_key_backend, "plaintext");
        assert_eq!(cli.consensus.evm_key_backend, "encrypted");
        assert_eq!(cli.consensus.reth_key_backend, "os-level");
        let rendered = format!("{:?}", cli.consensus);
        assert!(!rendered.contains("evm-secret-value"), "secret leaked");
        assert!(rendered.contains("evm_passphrase_configured"));
        assert!(rendered.contains("reth_passphrase_configured"));
    }

    #[test]
    fn test_p2p_profile_defaults_to_production() {
        let args = default_args();
        assert!(!args.use_local_defaults);
    }

    #[test]
    fn test_removed_legacy_flags_rejected_by_clap() {
        for argv in [
            &["test", "--tee-renewal.relay-key", "/tmp/relay-key.hex"][..],
            &["test", "--tee-renewal.rpc-url", "http://127.0.0.1:8545"],
            &["test", "--tee-renewal.poll-secs", "30"],
            &["test", "--tee-renewal.warning-blocks", "600"],
            &["test", "--tee-renewal.critical-blocks", "120"],
            &[
                "test",
                "--consensus.fee-recipient",
                "0x0000000000000000000000000000000000000001",
            ],
            &["test", "--consensus.validators", "/tmp/validators.json"],
            &["test", "--consensus.execution-watchdog-fatal-enabled"],
            &["test", "--consensus.leader-timeout-ms", "30000"],
            &["test", "--consensus.certification-timeout-ms", "30000"],
            &["test", "--consensus.signing-share", "/tmp/share.hex"],
            &["test", "--consensus.public-polynomial", "/tmp/poly.hex"],
            &["test", "--consensus.dkg-output", "/tmp/output.hex"],
        ] {
            let err = TestConsensusCli::try_parse_from(argv)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(argv[1]),
                "unexpected clap error for {}: {err}",
                argv[1]
            );
        }
    }
}
