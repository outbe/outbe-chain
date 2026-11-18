use crate::{rpc::Rpc, services::Radicle};

use alloy_primitives::Address;
use commonware_cryptography::{bls12381, Signer as _};
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::{
    tee_attestation_v1::TeePolicyScheduleV1, tee_genesis_v1::tee_attestation_v1_genesis_field,
};
use serde_json::{json, Value};
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const FOUNDERS: usize = 4;

pub fn port() -> u16 {
    reserve_port().local_addr().unwrap().port()
}

fn reserve_port() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("reserve a loopback TCP port")
}

pub fn binary() -> Command {
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
    pub fn spawn(command: &mut Command, log_path: &Path) -> Self {
        let log = fs::File::create(log_path).unwrap();
        Self(
            command
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        )
    }

    pub fn assert_running(&mut self, log_path: &Path) {
        if let Some(status) = self.0.try_wait().unwrap() {
            panic!(
                "process exited {status}: {}",
                fs::read_to_string(log_path).unwrap()
            );
        }
    }

    pub fn id(&self) -> u32 {
        self.0.id()
    }

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
    launch_commands: Vec<Command>,
    _radicle: Vec<Radicle>,
    _directory: tempfile::TempDir,
}
impl Cluster {
    pub fn start_with_genesis(config: Value, prepare: impl FnOnce(&mut Value, &[Address])) -> Self {
        Self::start_with_lease(config, None, prepare)
    }

    pub fn start_with_lease(
        config: Value,
        lease_seconds: Option<u64>,
        prepare: impl FnOnce(&mut Value, &[Address]),
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_owned();
        run(binary()
            .args(["dkg", "identities", "--output-dir"])
            .arg(&root)
            .args(["--validators", "4"]));
        let manifest = root.join("validators.json");
        let mut validators: Vec<Value> =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(validators.len(), FOUNDERS);
        // Keep every founder endpoint reserved until its designated child is launched.
        let reservations: Vec<[TcpListener; 6]> = (0..FOUNDERS)
            .map(|_| std::array::from_fn(|_| reserve_port()))
            .collect();
        let ports: Vec<_> = reservations
            .iter()
            .map(|listeners| {
                listeners
                    .each_ref()
                    .map(|listener| listener.local_addr().unwrap())
            })
            .collect();
        let mut peers = Vec::new();
        let mut addresses = Vec::new();
        for (index, validator) in validators.iter_mut().enumerate() {
            let listen = ports[index][0];
            let key = bls::load_individual_key(
                &root.join(format!("validator-{index}/signing-key.hex")),
                &KeyBackend::Plaintext,
            )
            .unwrap();
            peers.push((key.public_key(), listen));
            addresses.push(validator["address"].as_str().unwrap().parse().unwrap());
            validator["feeder_address"] =
                json!(format!("{:#x}", Address::repeat_byte(index as u8 + 0x51)));
            validator["radicle_node_id"] = json!(hex::encode([index as u8 + 1; 32]));
            validator["p2p_address"] = json!(listen.to_string());
        }
        write(&manifest, &json!(validators));
        let base = root.join("base.json");
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
        prepare(&mut genesis, &addresses);
        write(&seeded, &genesis);
        let final_genesis = root.join("genesis.json");
        run(binary()
            .args(["tee", "genesis", "--input"])
            .arg(seeded)
            .arg("--output")
            .arg(&final_genesis)
            .args(["--mode", "no-attestation-dev"]));
        if let Some(seconds) = lease_seconds {
            set_lease_policy(&final_genesis, seconds);
        }
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
        let mut launch_commands = Vec::new();
        let reth_ports: Vec<_> = ports.iter().map(|addresses| addresses[1].port()).collect();
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
        for (index, reserve) in reservations.into_iter().enumerate() {
            let validator = root.join(format!("validator-{index}"));
            let socket = validator.join("rad.sock");
            // Stub repository control only; both DKG ceremonies use real node P2P.
            radicle.push(Radicle::start(&socket, [index as u8 + 1; 32]));
            let [consensus, reth, rpc, metric, status, auth] = ports[index];
            let mut child = binary();
            child
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
                    &consensus.to_string(),
                    "--consensus.peers",
                    &peer_args,
                    "--consensus.use-local-defaults",
                ])
                .arg("--radicle.control-socket")
                .arg(&socket)
                .args([
                    "--radicle.status-address",
                    &status.to_string(),
                    "--http",
                    "--http.addr",
                    "127.0.0.1",
                    "--http.port",
                    &rpc.port().to_string(),
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
                    &metric.to_string(),
                    "--port",
                    &reth.port().to_string(),
                    "--authrpc.addr",
                    "127.0.0.1",
                    "--authrpc.port",
                    &auth.port().to_string(),
                    "--nat",
                    "none",
                    "--disable-discovery",
                    "--ipcdisable",
                    "--tee-canary.interval-secs",
                    "1",
                ]);
            drop(reserve);
            processes.push(Process::spawn(&mut child, &validator.join("node.log")));
            launch_commands.push(child);
            rpcs.push(Rpc::new(rpc.port()));
            metrics.push(metric.port());
        }
        Self {
            processes,
            rpcs,
            metrics,
            addresses,
            peers,
            root,
            launch_commands,
            _radicle: radicle,
            _directory: directory,
        }
    }

    pub fn reboot_founder(&mut self, index: usize, clock_offset_seconds: i64) -> u32 {
        assert_ne!(index, 0, "founder zero is the lifecycle upstream");
        let mut command = binary();
        command.args(self.launch_commands[index].get_args()).args([
            "--upstream",
            self.rpcs[0].url(),
            "--testnet.unix-time-offset-secs",
            &clock_offset_seconds.to_string(),
        ]);
        self.processes[index].stop();
        self.processes[index] = Process::spawn(
            &mut command,
            &self.root.join(format!("validator-{index}/node.log")),
        );
        self.processes[index].id()
    }
    pub async fn wait_finalized_on(&mut self, height: u64, live_founders: usize) -> u64 {
        assert!(live_founders > 0 && live_founders <= self.processes.len());
        let start = Instant::now();
        loop {
            self.assert_running(live_founders);
            let mut common_height = u64::MAX;
            for rpc in &self.rpcs[..live_founders] {
                common_height = common_height.min(rpc.finalized().await.unwrap_or_default());
            }
            if common_height >= height {
                return common_height;
            }
            assert!(
                start.elapsed() < Duration::from_secs(180),
                "finalization stalled before block {height}; logs: {}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub fn assert_running(&mut self, live_founders: usize) {
        for (index, process) in self.processes[..live_founders].iter_mut().enumerate() {
            process.assert_running(&self.root.join(format!("validator-{index}/node.log")));
        }
    }

    pub fn logs(&self) -> String {
        (0..FOUNDERS)
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
}

fn set_lease_policy(path: &Path, seconds: u64) {
    let mut genesis: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let field = &mut genesis["config"]["teeAttestationV1"];
    let encoded = field["policySchedule"]
        .as_str()
        .unwrap()
        .trim_start_matches("0x");
    let mut schedule =
        TeePolicyScheduleV1::decode_canonical(&hex::decode(encoded).unwrap()).unwrap();
    assert_eq!(schedule.entries.len(), 1);
    let policy = &mut schedule.entries[0].policy;
    policy.maximum_lease = seconds;
    *field = tee_attestation_v1_genesis_field(policy).unwrap();
    write(path, &genesis);
}
