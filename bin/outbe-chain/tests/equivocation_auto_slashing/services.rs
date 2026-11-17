use alloy_primitives::{B256, U256};
use outbe_consensus::bls::{self, KeyBackend};
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1, NodeIdV1};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    thread,
    time::Duration,
};

pub fn enclave(genesis: &Path, validator: &Path, index: usize) -> String {
    let seeded =
        outbe_tee::tee_attestation_activation::DcapSeededChainSpecBindingV1::from_genesis_path(
            genesis,
        )
        .unwrap();
    let binding = NetworkBindingV1 {
        chain_id: U256::from(seeded.chain_id).to_be_bytes(),
        genesis_hash: seeded.genesis_hash,
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let seed = U256::from(index + 1).to_be_bytes();
    let keys = outbe_tee_enclave::keys::EnclaveKeys::new([0x07; 32], Some(seed)).unwrap();
    let secret = bls::load_secp256k1_secret(
        &validator.join("reth-p2p-secret.hex"),
        &KeyBackend::Plaintext,
    )
    .unwrap();
    let p2p_key = k256::ecdsa::SigningKey::from_bytes((&secret).into()).unwrap();
    let reth_p2p_public: [u8; 33] = p2p_key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let node_id = NodeIdV1 { reth_p2p_public };
    let tee_dir = validator.join("tee");
    std::fs::create_dir_all(&tee_dir).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&tee_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let boot =
        outbe_tee_enclave::seal::EnclaveBootConfig::new(binding.chain_id, tee_dir, keys.isv_svn());
    let context = outbe_tee_enclave::transport::ServerContext {
        chain_id: B256::from(binding.chain_id),
        initialization: Arc::new(
            outbe_tee_enclave::initialization::factory::development_for_network(
                binding, &keys, node_id,
            ),
        ),
        offer_key: Arc::new(OnceLock::new()),
        boot: Some(Arc::new(boot)),
        keys: Arc::new(keys),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    thread::spawn(move || outbe_tee_enclave::transport::serve_tcp(&listener, context).unwrap());
    endpoint
}

pub struct Radicle {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Radicle {
    pub fn start(socket: &Path, node_id: [u8; 32]) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut bytes = vec![0xed, 0x01];
        bytes.extend_from_slice(&node_id);
        let encoded = multibase::encode(multibase::Base::Base58Btc, bytes);
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        if BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .is_err()
                        {
                            continue;
                        }
                        let command: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let response = match command["command"].as_str().unwrap() {
                            "nodeId" => serde_json::json!(encoded),
                            "config" => serde_json::json!({"externalAddresses":["127.0.0.1:8776"]}),
                            _ => serde_json::json!({"success":true}),
                        };
                        let _ = writeln!(stream, "{response}");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Radicle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
