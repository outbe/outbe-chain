use super::{
    rpc::Rpc,
    services::{self, Radicle},
};
use alloy_primitives::Address;
use commonware_cryptography::{bls12381, Signer as _};
use outbe_consensus::bls::{self, KeyBackend};
use serde_json::{json, Value};
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_outbe-chain"));
    command
        .env("RUST_MIN_STACK", "16777216")
        .env("RAYON_NUM_THREADS", "2");
    command
}
fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command {command:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

pub struct Process(Child);
impl Process {
    pub fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Cluster {
    pub processes: Vec<Process>,
    pub rpcs: Vec<Rpc>,
    pub metrics: Vec<u16>,
    pub addresses: Vec<Address>,
    pub peers: Vec<(bls12381::PublicKey, SocketAddr)>,
    pub root: PathBuf,
    _radicle: Vec<Radicle>,
    _directory: tempfile::TempDir,
}
impl Cluster {
    pub fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_owned();
        run(binary()
            .args(["dkg", "identities", "--output-dir"])
            .arg(&root)
            .args(["--validators", "4"]));
        let manifest = root.join("validators.json");
        let mut validators: Vec<Value> =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        let mut peers = Vec::new();
        let mut addresses = Vec::new();
        for (index, validator) in validators.iter_mut().enumerate() {
            let listen: SocketAddr = format!("127.0.0.1:{}", port()).parse().unwrap();
            let key = bls::load_individual_key(
                &root.join(format!("validator-{index}/signing-key.hex")),
                &KeyBackend::Plaintext,
            )
            .unwrap();
            peers.push((key.public_key(), listen));
            addresses.push(validator["address"].as_str().unwrap().parse().unwrap());
            validator["radicle_node_id"] = json!(hex::encode([index as u8 + 1; 32]));
            validator["p2p_address"] = json!(listen.to_string());
            validator["feeder_address"] = json!(Address::repeat_byte(0xf0 + index as u8));
        }
        write(&manifest, &json!(validators));
        let base = root.join("base.json");
        let config = json!({"chain_id":424242,"epoch_length_blocks":1200});
        let genesis = outbe_genesis_cli::stages::build_base_genesis(&config).unwrap();
        write(&base, &genesis);
        let seed = root.join("seed.json");
        write(&seed, &json!({}));
        let seeded = root.join("seeded.json");
        outbe_genesis_cli::create_genesis::run_seed_cli(
            &outbe_genesis_cli::create_genesis::SeedCliArgs {
                genesis: base,
                seed,
                validators: Some(manifest),
                output: seeded.clone(),
                worldwide_day: None,
                fresh_metadosis: false,
                contracts_dir: None,
            },
        )
        .unwrap();
        let mut genesis: Value = serde_json::from_slice(&fs::read(&seeded).unwrap()).unwrap();
        for address in &addresses {
            genesis["alloc"][format!("{address:x}")] = json!({"balance":"0x0"});
        }
        // Both downtime lanes must remain out of reach throughout this short test.
        genesis["alloc"]["000000000000000000000000000000000000ee01"]["storage"]
            [format!("0x{:064x}", 1)] = json!(format!("0x{:064x}", 300));
        genesis["alloc"]["000000000000000000000000000000000000ee01"]["storage"]
            [format!("0x{:064x}", 12)] = json!(format!("0x{:064x}", 300));
        write(&seeded, &genesis);
        let final_genesis = root.join("genesis.json");
        run(binary()
            .args(["tee", "genesis", "--input"])
            .arg(seeded)
            .arg("--output")
            .arg(&final_genesis)
            .args(["--mode", "gramine-direct-dev"]));
        let peer_args = validators
            .iter()
            .zip(&peers)
            .map(|(validator, (_, addr))| {
                format!("{}@{addr}", validator["public_key"].as_str().unwrap())
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut processes = Vec::new();
        let mut rpcs = Vec::new();
        let mut metrics = Vec::new();
        let mut radicle = Vec::new();
        let reth_ports: Vec<_> = (0..4).map(|_| port()).collect();
        let enodes: Vec<_> = reth_ports
            .iter()
            .enumerate()
            .map(|(index, port)| {
                let secret = bls::load_secp256k1_secret(
                    &root.join(format!("validator-{index}/reth-p2p-secret.hex")),
                    &KeyBackend::Plaintext,
                )
                .unwrap();
                let key = k256::ecdsa::SigningKey::from_bytes((&secret).into()).unwrap();
                format!(
                    "enode://{}@127.0.0.1:{port}",
                    hex::encode(&key.verifying_key().to_encoded_point(false).as_bytes()[1..])
                )
            })
            .collect();
        for (index, (_, listen)) in peers.iter().enumerate() {
            let validator = root.join(format!("validator-{index}"));
            let socket = validator.join("rad.sock");
            radicle.push(Radicle::start(&socket, [index as u8 + 1; 32]));
            let enclave = services::enclave(&final_genesis, &validator, index);
            let rpc = port();
            let metric = port();
            let log = fs::File::create(validator.join("node.log")).unwrap();
            let child = binary()
                .args(["node", "--validator", "--chain"])
                .arg(&final_genesis)
                .arg("--log.file.directory")
                .arg(validator.join("logs"))
                .args([
                    "--engine.persistence-threshold",
                    "0",
                    "--engine.memory-block-buffer-target",
                    "0",
                ])
                .arg("--datadir")
                .arg(validator.join("data"))
                .arg("--consensus.signing-key")
                .arg(validator.join("signing-key.hex"))
                .arg("--consensus.keys-dir")
                .arg(validator.join("dkg-keys"))
                .args([
                    "--consensus.listen-addr",
                    &listen.to_string(),
                    "--consensus.peers",
                    &peer_args,
                    "--consensus.use-local-defaults",
                ])
                .args(["--tee-enclave-socket", &enclave, "--radicle.control-socket"])
                .arg(&socket)
                .args([
                    "--radicle.status-address",
                    &format!("127.0.0.1:{}", port()),
                    "--http",
                    "--http.addr",
                    "127.0.0.1",
                    "--http.port",
                    &rpc.to_string(),
                    "--http.api",
                    "eth,net,web3,outbe",
                ])
                .arg("--p2p-secret-key")
                .arg(validator.join("reth-p2p-secret.hex"))
                .args([
                    "--addr",
                    "127.0.0.1",
                    "--trusted-peers",
                    &enodes
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != index)
                        .map(|(_, enode)| enode.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                ])
                .args([
                    "--metrics",
                    &format!("127.0.0.1:{metric}"),
                    "--port",
                    &reth_ports[index].to_string(),
                    "--authrpc.port",
                    &port().to_string(),
                    "--disable-discovery",
                    "--ipcdisable",
                ])
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            processes.push(Process(child));
            rpcs.push(Rpc::new(rpc));
            metrics.push(metric);
        }
        Self {
            processes,
            rpcs,
            metrics,
            addresses,
            peers,
            root,
            _radicle: radicle,
            _directory: directory,
        }
    }
    pub async fn wait_finalized(&mut self, height: u64) -> u64 {
        let start = Instant::now();
        loop {
            for (index, process) in self.processes.iter_mut().enumerate().take(3) {
                if let Some(status) = process.0.try_wait().unwrap() {
                    panic!(
                        "validator-{index} exited {status}: {}",
                        fs::read_to_string(self.root.join(format!("validator-{index}/node.log")))
                            .unwrap()
                    );
                }
            }
            if let Some(current) = self.rpcs[0].finalized().await {
                if current >= height {
                    return current;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(180),
                "finalization stalled before block {height}; logs: {}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub fn logs(&self) -> String {
        (0..4)
            .map(|i| {
                let text =
                    fs::read_to_string(self.root.join(format!("validator-{i}/node.log"))).unwrap();
                let lines = text.lines().rev().take(60).collect::<Vec<_>>();
                format!(
                    "validator-{i}: {}",
                    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub async fn zero_balances(&self) {
        for (rpc, address) in self.rpcs.iter().zip(&self.addresses).take(3) {
            assert_eq!(
                rpc.account("eth_getBalance", *address).await,
                alloy_primitives::U256::ZERO,
                "honest validator liquid balance"
            );
        }
    }
}
