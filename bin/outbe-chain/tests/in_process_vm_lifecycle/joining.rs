use crate::{
    rpc::Rpc,
    services::Radicle,
    tee_cluster::{binary, Cluster, Process},
};
use alloy_primitives::{B256, U256};
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381::PrivateKey;
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    signer::OutbeEvmSigner,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};
use serde_json::{json, Value};
use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    time::Duration,
};

#[derive(Clone, Copy)]
pub enum Role {
    Validator,
    KeyedFullNode,
    KeylessFollower,
}

pub struct JoinIdentity {
    directory: tempfile::TempDir,
    pub node_id: B256,
    pub evm: OutbeEvmSigner,
    pub bls: PrivateKey,
    reth_public: [u8; 33],
}

impl JoinIdentity {
    pub fn generate(node_id: B256) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let output = binary()
            .args(["dkg", "identities", "--output-dir"])
            .arg(directory.path())
            .args(["--validators", "1"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "joining identity generation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = directory.path().join("validator-0");
        let evm = bls::load_validator_evm_signer(&path.join("evm-key.hex"), &KeyBackend::Plaintext)
            .unwrap();
        let consensus_key =
            bls::load_individual_key(&path.join("signing-key.hex"), &KeyBackend::Plaintext)
                .unwrap();
        let secret =
            bls::load_secp256k1_secret(&path.join("reth-p2p-secret.hex"), &KeyBackend::Plaintext)
                .unwrap();
        let reth_public = k256::ecdsa::SigningKey::from_bytes((&secret).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        Self {
            directory,
            node_id,
            evm,
            bls: consensus_key,
            reth_public,
        }
    }

    pub async fn binding(&self, upstream: &Rpc) -> NodeEnclaveBindingV1View {
        upstream
            .call_at(
                TEE_REGISTRY_ADDRESS,
                ITeeRegistryV1::nodeHostEnclaveBindingCall {
                    rethP2pPrefix: self.reth_public[0],
                    rethP2pX: B256::from_slice(&self.reth_public[1..]),
                },
                json!("finalized"),
            )
            .await
    }

    pub fn datadir(&self) -> PathBuf {
        self.directory.path().join("vm/data")
    }

    pub fn start(&self, cluster: &Cluster, role: Role) -> JoinProcess {
        let identity = self.directory.path().join("validator-0");
        let dir = self.directory.path().join("vm");
        std::fs::create_dir_all(&dir).unwrap();
        let socket = self.directory.path().join("rad.sock");
        let radicle = matches!(role, Role::Validator).then(|| {
            if socket.exists() {
                std::fs::remove_file(&socket).unwrap();
            }
            Radicle::start(&socket, self.node_id.0)
        });
        let reserves: [TcpListener; 6] =
            std::array::from_fn(|_| TcpListener::bind("127.0.0.1:0").unwrap());
        let [consensus, reth, rpc, metrics, status, auth] =
            reserves.each_ref().map(|port| port.local_addr().unwrap());
        let peers = cluster
            .peers
            .iter()
            .map(|(key, address)| format!("{}@{address}", hex::encode(key.encode())))
            .collect::<Vec<_>>()
            .join(",");
        let log = dir.join("node.log");
        let mut command = binary();
        command
            .args(["node", "--chain"])
            .arg(cluster.root.join("genesis.json"))
            .arg("--datadir")
            .arg(dir.join("data"))
            .arg("--log.file.directory")
            .arg(dir.join("logs"))
            .arg("--p2p-secret-key")
            .arg(identity.join("reth-p2p-secret.hex"))
            .args([
                "--upstream",
                cluster.rpcs[0].url(),
                "--engine.persistence-threshold",
                "0",
                "--engine.memory-block-buffer-target",
                "0",
                "--addr",
                "127.0.0.1",
                "--port",
                &reth.port().to_string(),
                "--http",
                "--http.addr",
                "127.0.0.1",
                "--http.port",
                &rpc.port().to_string(),
                "--http.api",
                "eth,net,web3,outbe",
                "--authrpc.addr",
                "127.0.0.1",
                "--authrpc.port",
                &auth.port().to_string(),
                "--metrics",
                &metrics.to_string(),
                "--tee-canary.interval-secs",
                "1",
                "--nat",
                "none",
                "--disable-discovery",
                "--ipcdisable",
            ]);
        if matches!(role, Role::Validator) {
            command
                .args(["--validator", "--consensus.signing-key"])
                .arg(identity.join("signing-key.hex"))
                .arg("--radicle.control-socket")
                .arg(socket)
                .args([
                    "--radicle.status-address",
                    &status.to_string(),
                    "--consensus.listen-addr",
                    &consensus.to_string(),
                    "--consensus.peers",
                    &peers,
                    "--consensus.use-local-defaults",
                ]);
        }
        if !matches!(role, Role::KeylessFollower) {
            command
                .arg("--validator.evm-key")
                .arg(identity.join("evm-key.hex"));
        }
        drop(reserves);
        JoinProcess {
            process: Process::spawn(&mut command, &log),
            rpc: Rpc::new(rpc.port()),
            consensus,
            log,
            _radicle: radicle,
        }
    }
}

pub struct JoinProcess {
    pub process: Process,
    pub rpc: Rpc,
    pub consensus: SocketAddr,
    log: PathBuf,
    _radicle: Option<Radicle>,
}

impl JoinProcess {
    pub fn assert_running(&mut self) {
        self.process.assert_running(&self.log);
    }

    pub async fn wait_status(
        &mut self,
        description: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        let outcome = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                self.assert_running();
                if let Some(status) = self.rpc.request("outbe_consensusStatus", json!([])).await {
                    if predicate(&status) {
                        return status;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        outcome.unwrap_or_else(|_| {
            let log = std::fs::read_to_string(&self.log).unwrap();
            let tail = log.lines().rev().take(60).collect::<Vec<_>>();
            panic!("{description} not observed: {tail:?}");
        })
    }
}

pub fn fund(genesis: &mut Value, identity: &JoinIdentity, value: U256) {
    genesis["alloc"][format!("{:x}", identity.evm.address())] = json!({"balance": value});
}
