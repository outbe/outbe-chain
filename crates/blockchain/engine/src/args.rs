//! Consensus CLI arguments.

use std::{fmt, net::SocketAddr, path::PathBuf};

use alloy_primitives::B256;

/// Complete required configuration for finalized offchain-data projection into MongoDB.
#[derive(Clone, Eq, PartialEq)]
pub struct OffchainDataArgs {
    /// MongoDB connection string.
    pub mongodb_uri: String,
    /// Logical database exclusively owned by this node's projector.
    pub mongodb_database: String,
    /// First block projected when the managed database has no checkpoint.
    pub start_block: u64,
}

/// Complete optional node-side OCOMP control-plane configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompNodeControlConfig {
    pub supervisor_socket: PathBuf,
    pub snapshot_exporter_socket: PathBuf,
    pub supervisor_uid: u32,
    pub snapshot_exporter_uid: u32,
    pub protocol_bundle_hash: B256,
    pub boot_nonce: B256,
    pub session_generation: u64,
    /// Present only for a validator that signs OCOMP result votes. A certified
    /// FullNode runs the same discovery/export/compute control plane without a
    /// signing key and can never request result attestation.
    pub key_path: Option<PathBuf>,
}

/// Local process-boundary settings. Omitted as a whole until OCOMP is enabled.
#[derive(Clone, Debug, clap::Args)]
pub struct OcompArgs {
    /// Node UDS serving the fixed Supervisor capability set.
    #[arg(long = "ocomp.supervisor-socket", value_name = "PATH")]
    pub supervisor_socket: Option<PathBuf>,

    /// Separate node UDS serving the fixed SnapshotExporter capability set.
    #[arg(long = "ocomp.snapshot-exporter-socket", value_name = "PATH")]
    pub snapshot_exporter_socket: Option<PathBuf>,

    /// Effective UID accepted on the Supervisor UDS via SO_PEERCRED.
    #[arg(long = "ocomp.supervisor-uid", value_name = "UID")]
    pub supervisor_uid: Option<u32>,

    /// Effective UID accepted on the SnapshotExporter UDS via SO_PEERCRED.
    #[arg(long = "ocomp.snapshot-exporter-uid", value_name = "UID")]
    pub snapshot_exporter_uid: Option<u32>,

    /// Exact pinned OCOMP protocol bundle identity.
    #[arg(long = "ocomp.protocol-bundle-hash", value_name = "B256")]
    pub protocol_bundle_hash: Option<B256>,

    /// Per-node process boot nonce bound into every local-control handshake.
    #[arg(long = "ocomp.boot-nonce", value_name = "B256")]
    pub boot_nonce: Option<B256>,

    /// Monotonic local-control session generation for this node boot.
    #[arg(long = "ocomp.session-generation", default_value_t = 1)]
    pub session_generation: u64,

    /// Node-owned result-signing key registered on-chain by `confirmValidatorReady`.
    #[arg(long = "ocomp.key", value_name = "PATH")]
    pub key_path: Option<PathBuf>,
}

impl Default for OcompArgs {
    fn default() -> Self {
        Self {
            supervisor_socket: None,
            snapshot_exporter_socket: None,
            supervisor_uid: None,
            snapshot_exporter_uid: None,
            protocol_bundle_hash: None,
            boot_nonce: None,
            session_generation: 1,
            key_path: None,
        }
    }
}

impl OcompArgs {
    /// Returns a complete fixed-role configuration or rejects a partial profile.
    pub fn node_control(&self) -> eyre::Result<Option<OcompNodeControlConfig>> {
        let configured = [
            self.supervisor_socket.is_some(),
            self.snapshot_exporter_socket.is_some(),
            self.supervisor_uid.is_some(),
            self.snapshot_exporter_uid.is_some(),
            self.protocol_bundle_hash.is_some(),
            self.boot_nonce.is_some(),
        ];
        if configured.iter().all(|value| !value) && self.key_path.is_none() {
            return Ok(None);
        }
        if !configured.iter().all(|value| *value) {
            eyre::bail!(
                "OCOMP node control requires both role sockets, both peer UIDs, \
                 --ocomp.protocol-bundle-hash and --ocomp.boot-nonce"
            );
        }
        if self.session_generation == 0 {
            eyre::bail!("--ocomp.session-generation must be greater than zero");
        }

        let supervisor_socket = self
            .supervisor_socket
            .clone()
            .expect("complete profile checked above");
        let snapshot_exporter_socket = self
            .snapshot_exporter_socket
            .clone()
            .expect("complete profile checked above");
        if supervisor_socket == snapshot_exporter_socket {
            eyre::bail!("OCOMP Supervisor and SnapshotExporter require distinct sockets");
        }
        let protocol_bundle_hash = self
            .protocol_bundle_hash
            .expect("complete profile checked above");
        let boot_nonce = self.boot_nonce.expect("complete profile checked above");
        if protocol_bundle_hash.is_zero() {
            eyre::bail!("--ocomp.protocol-bundle-hash must not be zero");
        }
        if boot_nonce.is_zero() {
            eyre::bail!("--ocomp.boot-nonce must not be zero");
        }

        Ok(Some(OcompNodeControlConfig {
            supervisor_socket,
            snapshot_exporter_socket,
            supervisor_uid: self.supervisor_uid.expect("complete profile checked above"),
            snapshot_exporter_uid: self
                .snapshot_exporter_uid
                .expect("complete profile checked above"),
            protocol_bundle_hash,
            boot_nonce,
            session_generation: self.session_generation,
            key_path: self.key_path.clone(),
        }))
    }
}

impl fmt::Debug for OffchainDataArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OffchainDataArgs")
            .field("mongodb_uri", &"<redacted>")
            .field("mongodb_database", &self.mongodb_database)
            .field("start_block", &self.start_block)
            .finish()
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
    /// This is consensus-archive recovery after a storage wipe; it never bypasses
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

    /// Minimum time (ms) before sending a proposal to keep block times stable.
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

    /// Path or `host:port` endpoint for the `outbe-tee-enclave` sidecar.
    /// Every `teeAttestationV1` ChainSpec requires it. `DcapRequired` performs
    /// NodeHost-authorized SGX initialization; `GramineDirectDev` connects only
    /// on its separate development chain. Missing or rejected transport stops
    /// startup and never selects an in-process stub or another attestation mode.
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

    /// Funded EVM private key used only to relay automatic renewal
    /// transactions. Required by DcapRequired nodes; FullNode identity keys
    /// never implicitly become funded transaction signers.
    #[arg(long = "tee-renewal.relay-key", value_name = "PATH")]
    pub tee_renewal_relay_key: Option<PathBuf>,

    /// Local HTTP JSON-RPC endpoint used by the renewal worker after Reth starts.
    #[arg(
        long = "tee-renewal.rpc-url",
        default_value = "http://127.0.0.1:8545",
        value_name = "URL"
    )]
    pub tee_renewal_rpc_url: String,

    /// Interval between automatic renewal reconciliation attempts.
    #[arg(long = "tee-renewal.poll-secs", default_value_t = 30)]
    pub tee_renewal_poll_secs: u64,

    /// Warning margin relative to the next finalized DKG freeze.
    #[arg(long = "tee-renewal.warning-blocks", default_value_t = 600)]
    pub tee_renewal_warning_blocks: u64,

    /// Critical margin relative to the next finalized DKG freeze.
    #[arg(long = "tee-renewal.critical-blocks", default_value_t = 120)]
    pub tee_renewal_critical_blocks: u64,

    /// Run as a FOLLOWER: cold-sync finalized blocks from this upstream node and
    /// verify them against the committee (anchored on the genesis validator set,
    /// read from the node's own genesis state), instead of running the consensus
    /// engine. The lightweight full-node path. Mutually exclusive with
    /// `--validator`.
    #[arg(long = "upstream", value_name = "URL", conflicts_with = "is_validator")]
    pub upstream: Option<String>,

    /// Dev only: follow without verifying consensus certificates (EL-only sync).
    /// Requires `--upstream`.
    #[arg(
        long = "upstream.nocertify",
        default_value_t = false,
        requires = "upstream"
    )]
    pub upstream_nocertify: bool,

    /// MongoDB URI for the required finalized offchain-data projection.
    #[arg(
        long = "projection.mongodb-uri",
        env = "OUTBE_PROJECTION_MONGODB_URI",
        value_name = "URI"
    )]
    pub projection_mongodb_uri: Option<String>,

    /// Logical MongoDB database exclusively owned by this node's projector.
    #[arg(
        long = "projection.mongodb-database",
        env = "OUTBE_PROJECTION_MONGODB_DATABASE",
        value_name = "DATABASE"
    )]
    pub projection_mongodb_database: Option<String>,

    /// First block to project into a new managed database.
    #[arg(long = "projection.start-block", default_value_t = 1)]
    pub projection_start_block: u64,

    #[command(flatten)]
    pub ocomp: OcompArgs,
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
            .field("tee_enclave_configured", &self.tee_enclave_socket.is_some())
            .field(
                "tee_renewal_relay_configured",
                &self.tee_renewal_relay_key.is_some(),
            )
            .field("tee_renewal_rpc_url", &self.tee_renewal_rpc_url)
            .field("tee_renewal_poll_secs", &self.tee_renewal_poll_secs)
            .field("upstream_configured", &self.upstream.is_some())
            .field(
                "offchain_data_configured",
                &self.projection_mongodb_uri.is_some(),
            )
            .field(
                "projection_mongodb_database",
                &self.projection_mongodb_database,
            )
            .field("projection_start_block", &self.projection_start_block)
            .field(
                "ocomp_control_configured",
                &self.ocomp.supervisor_socket.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ConsensusArgs {
    /// Validate argument consistency.
    ///
    /// - `--validator` without `--consensus.signing-key` → error
    /// - `--consensus.signing-key` without `--validator` → warning (ignored key)
    /// - `--bls-key-backend encrypted` without `--bls-passphrase` → error
    pub fn validate(&self) -> eyre::Result<()> {
        self.offchain_data()?;
        let ocomp_node_control = self.ocomp.node_control()?;
        if self.tee_renewal_poll_secs == 0 {
            eyre::bail!("--tee-renewal.poll-secs must be greater than zero");
        }
        if self.tee_renewal_critical_blocks > self.tee_renewal_warning_blocks {
            eyre::bail!("--tee-renewal.critical-blocks cannot exceed --tee-renewal.warning-blocks");
        }
        if self.tee_renewal_rpc_url.trim().is_empty() {
            eyre::bail!("--tee-renewal.rpc-url must not be empty");
        }
        // Follower mode (`--upstream`) is the lightweight full-node path and must
        // not be combined with validator/consensus participation. (clap's
        // `conflicts_with` also enforces this on the CLI; this covers programmatic
        // construction and gives a clear message.)
        if self.upstream.is_some() && self.is_validator {
            eyre::bail!("--upstream (follower mode) is mutually exclusive with --validator");
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
        if self.is_validator && ocomp_node_control.is_none() {
            eyre::bail!(
                "validator requires OCOMP voting configuration: provide both role sockets, \
                 both peer UIDs, --ocomp.protocol-bundle-hash, --ocomp.boot-nonce and --ocomp.key"
            );
        }
        if self.is_validator
            && ocomp_node_control
                .as_ref()
                .is_some_and(|config| config.key_path.is_none())
        {
            eyre::bail!("validator OCOMP voting configuration requires --ocomp.key");
        }
        if self.upstream.is_some() && ocomp_node_control.is_none() {
            eyre::bail!(
                "certified FullNode requires OCOMP compute control: provide both role sockets, \
                 both peer UIDs, --ocomp.protocol-bundle-hash and --ocomp.boot-nonce"
            );
        }
        if self.upstream.is_some()
            && ocomp_node_control
                .as_ref()
                .is_some_and(|config| config.key_path.is_some())
        {
            eyre::bail!("certified FullNode must not configure --ocomp.key");
        }
        if !self.is_validator && self.upstream.is_none() && ocomp_node_control.is_some() {
            eyre::bail!("OCOMP control requires --validator or certified --upstream FullNode");
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

    /// Returns the complete required projection configuration.
    pub fn offchain_data(&self) -> eyre::Result<OffchainDataArgs> {
        match (
            self.projection_mongodb_uri.as_ref(),
            self.projection_mongodb_database.as_ref(),
        ) {
            (None, None) => Err(eyre::eyre!(
                "MongoDB projection is required; provide --projection.mongodb-uri and --projection.mongodb-database"
            )),
            (Some(uri), Some(database)) => {
                if uri.trim().is_empty() {
                    eyre::bail!("--projection.mongodb-uri must not be empty");
                }
                if database.trim().is_empty() {
                    eyre::bail!("--projection.mongodb-database must not be empty");
                }
                Ok(OffchainDataArgs {
                    mongodb_uri: uri.clone(),
                    mongodb_database: database.clone(),
                    start_block: self.projection_start_block,
                })
            }
            _ => Err(eyre::eyre!(
                "--projection.mongodb-uri and --projection.mongodb-database must be provided together"
            )),
        }
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
            return Err(eyre::eyre!(
                "--validator requires --consensus.signing-key before deriving default --validator.evm-key"
            ));
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
            other => Err(eyre::eyre!(
                "unknown BLS key backend: {other} (expected: plaintext, encrypted, os-level)"
            )),
        }
    }
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
            testnet_unix_time_offset_secs: None,
            consensus_peers: vec![],
            use_local_defaults: false,
            payload_resolve_time_ms: 200,
            payload_return_time_ms: 450,
            worker_threads: 3,
            bls_key_backend: "plaintext".to_string(),
            bls_passphrase: None,
            tee_enclave_socket: None,
            tee_bootstrap_timeout_secs: 60,
            tee_renewal_relay_key: None,
            tee_renewal_rpc_url: "http://127.0.0.1:8545".to_owned(),
            tee_renewal_poll_secs: 30,
            tee_renewal_warning_blocks: 600,
            tee_renewal_critical_blocks: 120,
            upstream: None,
            upstream_nocertify: false,
            projection_mongodb_uri: Some("mongodb://localhost:27017".to_owned()),
            projection_mongodb_database: Some("outbe_projection".to_owned()),
            projection_start_block: 1,
            ocomp: OcompArgs::default(),
        }
    }

    fn configure_ocomp_control(args: &mut ConsensusArgs) {
        args.ocomp.supervisor_socket = Some("/tmp/supervisor.sock".into());
        args.ocomp.snapshot_exporter_socket = Some("/tmp/exporter.sock".into());
        args.ocomp.supervisor_uid = Some(1001);
        args.ocomp.snapshot_exporter_uid = Some(1002);
        args.ocomp.protocol_bundle_hash = Some(B256::repeat_byte(0x11));
        args.ocomp.boot_nonce = Some(B256::repeat_byte(0x22));
    }

    fn configure_ocomp_voting(args: &mut ConsensusArgs) {
        configure_ocomp_control(args);
        args.ocomp.key_path = Some("/tmp/ocomp-key.hex".into());
    }

    #[test]
    fn test_full_node_without_key_ok() {
        assert!(default_args().validate().is_ok());
    }

    #[test]
    fn ocomp_node_control_is_all_or_nothing_and_uses_distinct_role_sockets() {
        let mut args = default_args();
        args.ocomp.supervisor_socket = Some("/tmp/supervisor.sock".into());
        assert!(args.validate().is_err());

        configure_ocomp_voting(&mut args);
        let config = args.ocomp.node_control().unwrap().unwrap();
        assert_eq!(config.supervisor_uid, 1001);
        assert_eq!(config.snapshot_exporter_uid, 1002);

        args.ocomp.snapshot_exporter_socket = args.ocomp.supervisor_socket.clone();
        assert!(args.ocomp.node_control().is_err());
    }

    #[test]
    fn validator_requires_complete_ocomp_voting_config() {
        let non_participant = default_args();
        assert!(non_participant.validate().is_ok());

        let mut validator = default_args();
        validator.is_validator = true;
        validator.signing_key = Some("/tmp/bls-key.hex".into());
        let error = validator
            .validate()
            .expect_err("validator without OCOMP voting configuration must fail closed")
            .to_string();
        assert!(error.contains("validator requires OCOMP"), "error: {error}");
    }

    #[test]
    fn certified_full_node_requires_complete_keyless_ocomp_control() {
        let mut follower = default_args();
        follower.upstream = Some("http://upstream:8545".to_owned());
        let error = follower
            .validate()
            .expect_err("OCOMP-enabled FullNode without its local compute control must fail")
            .to_string();
        assert!(error.contains("FullNode requires OCOMP"), "error: {error}");

        configure_ocomp_control(&mut follower);
        follower
            .validate()
            .expect("FullNode accepts a complete keyless OCOMP control profile");
        let config = follower
            .ocomp
            .node_control()
            .expect("complete profile parses")
            .expect("profile is configured");
        assert!(config.key_path.is_none(), "FullNode has no voting key");

        follower.ocomp.key_path = Some("/tmp/forbidden-ocomp-key.hex".into());
        let error = follower
            .validate()
            .expect_err("FullNode must not acquire validator vote capability")
            .to_string();
        assert!(
            error.contains("must not configure --ocomp.key"),
            "error: {error}"
        );
    }

    #[test]
    fn removed_ocomp_validator_index_flag_is_rejected() {
        assert!(
            TestConsensusCli::try_parse_from(["test", "--ocomp.validator-index", "4",]).is_err()
        );
    }

    #[test]
    fn validator_and_full_node_require_complete_mongo_configuration() {
        for is_validator in [false, true] {
            let mut args = default_args();
            args.is_validator = is_validator;
            args.projection_mongodb_uri = None;
            args.projection_mongodb_database = None;
            let error = args.validate().unwrap_err().to_string();
            assert!(error.contains("required"), "error: {error}");

            args.projection_mongodb_uri = Some("mongodb://localhost:27017".to_owned());
            let error = args.validate().unwrap_err().to_string();
            assert!(
                error.contains("must be provided together"),
                "error: {error}"
            );
        }

        let mut args = default_args();
        args.projection_mongodb_uri = Some("mongodb://localhost:27017".to_owned());

        args.projection_mongodb_database = Some("outbe_projection".to_owned());
        args.projection_start_block = 42;
        let config = args.offchain_data().unwrap();
        assert_eq!(config.mongodb_uri, "mongodb://localhost:27017");
        assert_eq!(config.mongodb_database, "outbe_projection");
        assert_eq!(config.start_block, 42);
    }

    #[test]
    fn cli_parses_projection_configuration() {
        let cli = TestConsensusCli::try_parse_from([
            "test",
            "--projection.mongodb-uri",
            "mongodb://mongo:27017/?replicaSet=rs0",
            "--projection.mongodb-database",
            "outbe_projection",
            "--projection.start-block",
            "17",
        ])
        .unwrap();
        let config = cli.consensus.offchain_data().unwrap();
        assert_eq!(config.start_block, 17);
        assert_eq!(config.mongodb_database, "outbe_projection");
    }

    #[test]
    fn projection_defaults_to_first_executable_block() {
        let cli = TestConsensusCli::try_parse_from([
            "test",
            "--projection.mongodb-uri",
            "mongodb://mongo:27017/?replicaSet=rs0",
            "--projection.mongodb-database",
            "outbe_projection",
        ])
        .unwrap();

        assert_eq!(cli.consensus.offchain_data().unwrap().start_block, 1);
    }

    #[test]
    fn debug_output_redacts_operator_secrets() {
        let mut args = default_args();
        args.bls_passphrase = Some("bls-secret-value".to_owned());
        args.upstream = Some("https://user:upstream-secret@example.test".to_owned());
        args.projection_mongodb_uri =
            Some("mongodb://user:mongo-secret@localhost:27017".to_owned());
        args.projection_mongodb_database = Some("outbe_projection".to_owned());

        let args_debug = format!("{args:?}");
        let config_debug = format!("{:?}", args.offchain_data().unwrap());

        for secret in ["bls-secret-value", "upstream-secret", "mongo-secret"] {
            assert!(!args_debug.contains(secret));
            assert!(!config_debug.contains(secret));
        }
        assert!(args_debug.contains("offchain_data_configured: true"));
        assert!(config_debug.contains("mongodb_uri: \"<redacted>\""));
    }

    #[test]
    fn test_follower_upstream_ok_without_validator() {
        let mut args = default_args();
        args.upstream = Some("http://upstream:8545".to_string());
        configure_ocomp_control(&mut args);
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
    fn test_validator_with_signing_key_and_ocomp_voting_ok() {
        let mut args = default_args();
        args.is_validator = true;
        args.signing_key = Some(PathBuf::from("/tmp/key.hex"));
        configure_ocomp_voting(&mut args);
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
