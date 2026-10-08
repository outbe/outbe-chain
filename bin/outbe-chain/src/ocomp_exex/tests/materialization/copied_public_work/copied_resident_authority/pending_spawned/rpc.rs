use super::*;

#[derive(Default)]
pub(super) struct RpcEvidence {
    pub(super) calls: usize,
    pub(super) sent: Vec<(Address, Vec<u8>, B256)>,
    pub(super) round_days: Vec<u32>,
}
struct ScriptedResponses {
    own_sender: Address,
    replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
    expected: BTreeMap<Address, Vec<u8>>,
}
impl ScriptedResponses {
    fn respond(
        &self,
        request: &serde_json::Value,
        state: &mut RpcEvidence,
        point: ProjectionCheckpoint,
    ) -> serde_json::Value {
        let params = &request["params"];
        let method = request["method"].as_str().unwrap();
        match method {
            "eth_chainId" => {
                serde_json::json!(format!("0x{:x}", copied_native::chain().chain().id()))
            }
            "eth_getTransactionCount" => {
                assert_eq!(
                    params[0].as_str().unwrap().parse::<Address>().unwrap(),
                    self.own_sender
                );
                assert_eq!(params[1], "latest");
                // Stable scripted nonce: this component does not model native
                // nonce advancement or order concurrent worker scheduling.
                serde_json::json!("0x7")
            }
            "eth_gasPrice" => serde_json::json!("0x1"),
            "eth_call" => self.read_call(params, state),
            "eth_sendRawTransaction" => self.submit_transaction(params, state),
            "eth_getTransactionReceipt" => {
                let hash = params[0].as_str().unwrap().parse::<B256>().unwrap();
                if state.sent.iter().any(|(_, _, sent)| *sent == hash) {
                    // Deliberately reverted scripted receipts: proves actual delivery
                    // and completion without pretending native state was advanced.
                    serde_json::json!({"transactionHash": format!("{hash:#x}"), "blockNumber": format!("0x{:x}", point.block_number), "blockHash": format!("{:#x}", point.block_hash), "status":"0x0"})
                } else {
                    serde_json::Value::Null
                }
            }
            "eth_getBlockByNumber" => {
                assert!(
                    params[0] == "finalized" || params[0] == format!("0x{:x}", point.block_number)
                );
                serde_json::json!({"number":format!("0x{:x}", point.block_number),"hash":format!("{:#x}",point.block_hash)})
            }
            other => panic!("unexpected RPC method: {other}"),
        }
    }
    fn read_call(&self, params: &serde_json::Value, state: &mut RpcEvidence) -> serde_json::Value {
        assert_eq!(params[1], "finalized");
        let to = params[0]["to"]
            .as_str()
            .unwrap()
            .parse::<Address>()
            .unwrap();
        let data = hex::decode(
            params[0]["data"]
                .as_str()
                .unwrap()
                .strip_prefix("0x")
                .unwrap(),
        )
        .unwrap();
        if data.starts_with(&contributorPayoutRoundCall::SELECTOR) {
            let call = contributorPayoutRoundCall::abi_decode(&data).unwrap();
            state.round_days.push(call.worldwideDay);
        }
        let bytes = self
            .replies
            .get(&(to, data))
            .expect("RPC read outside native fixture/lookback or scripted ActiveGeneration");
        serde_json::json!(format!("0x{}", hex::encode(bytes)))
    }
    fn submit_transaction(
        &self,
        params: &serde_json::Value,
        state: &mut RpcEvidence,
    ) -> serde_json::Value {
        let raw = hex::decode(params[0].as_str().unwrap().strip_prefix("0x").unwrap()).unwrap();
        let mut slice = raw.as_slice();
        let tx = EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut slice).unwrap();
        assert!(slice.is_empty());
        assert!(matches!(&tx, EthereumTxEnvelope::Eip1559(_)));
        assert_eq!(tx.recover_signer().unwrap(), self.own_sender);
        assert_eq!(tx.chain_id(), Some(copied_native::chain().chain().id()));
        assert_eq!(tx.nonce(), 7, "must sign the nonce returned by this RPC");
        assert_eq!(tx.value(), U256::ZERO);
        let TxKind::Call(to) = tx.kind() else {
            panic!("unexpected contract creation")
        };
        assert_eq!(
            tx.input().as_ref(),
            self.expected
                .get(&to)
                .expect("unexpected submission destination")
                .as_slice()
        );
        assert!(
            !state.sent.iter().any(|(previous, _, _)| *previous == to),
            "duplicate submission"
        );
        let hash = keccak256(&raw);
        state.sent.push((to, raw, hash));
        serde_json::json!(format!("{hash:#x}"))
    }
}

pub(super) struct ScriptedRpc {
    pub(super) url: String,
    address: std::net::SocketAddr,
    stopped: Arc<AtomicBool>,
    pub(super) point: Arc<Mutex<ProjectionCheckpoint>>,
    pub(super) evidence: Arc<Mutex<RpcEvidence>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl ScriptedRpc {
    pub(super) fn start(
        point: ProjectionCheckpoint,
        own_sender: Address,
        replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
        expected: BTreeMap<Address, Vec<u8>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let evidence = Arc::new(Mutex::new(RpcEvidence::default()));
        let point = Arc::new(Mutex::new(point));
        let stop = Arc::clone(&stopped);
        let seen = Arc::clone(&evidence);
        let tip = Arc::clone(&point);
        let responses = ScriptedResponses {
            own_sender,
            replies,
            expected,
        };
        let thread = std::thread::spawn(move || {
            // Joining is unblocked explicitly with a loopback connection.
            // Every accepted request also has a strict byte/time bound.
            while !stop.load(Ordering::Acquire) {
                let (mut stream, _) = listener.accept().unwrap();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_http_json(&mut stream);
                let mut state = seen.lock().unwrap();
                state.calls += 1;
                assert!(state.calls <= 256, "unexpected RPC loop");
                let point = *tip.lock().unwrap();
                let result = responses.respond(&request, &mut state, point);
                let body = serde_json::to_vec(
                    &serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result}),
                )
                .unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url: format!("http://{address}"),
            address,
            stopped,
            point,
            evidence,
            thread: Some(thread),
        }
    }
    pub(super) fn assert_quiet(&self) {
        let evidence = self.evidence.lock().unwrap();
        assert_eq!(evidence.calls, 0);
        assert!(evidence.sent.is_empty());
    }
    pub(super) fn finish(mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("scripted RPC failed");
    }
}
impl Drop for ScriptedRpc {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn read_http_json(stream: &mut TcpStream) -> serde_json::Value {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST "));
    let mut length = None;
    let mut header_bytes = line.len();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        header_bytes += line.len();
        assert!(header_bytes < 16_384);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = length.expect("content length");
    assert!(length <= 256 * 1024);
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
