use super::*;
use alloy_primitives::{keccak256, Bytes, B256};
use outbe_protocol::{codec::FieldElement, Codec};
use outbe_zk_canonical::paynote::{decode_public_inputs, PayNoteSuite};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};

fn wallet() -> Wallet {
    let mut key = Zeroizing::new([0; 32]);
    loop {
        SystemRandom::new().fill(key.as_mut()).unwrap();
        if let Ok(wallet) = Wallet::new(&hex::encode(key.as_ref())) {
            return wallet;
        }
    }
}

fn gem_data(owner: Address, id: U256, amount: U256, state: u8) -> IGem::GemData {
    IGem::GemData {
        gemId: id,
        owner,
        gemType: 0,
        state,
        promisLoad: amount,
        entryPrice: U256::ZERO,
        floorPrice: U256::ZERO,
        issuanceCurrency: 840,
        referenceCurrency: 840,
        issuedAt: 0,
        callPrice: U256::ZERO,
        calledAt: 0,
        callNoticePeriod: 0,
    }
}

#[test]
fn ids_amounts_and_deadlines_preserve_integer_boundaries() {
    assert_eq!(parse_gem_id(&U256::MAX.to_string()).unwrap(), U256::MAX);
    assert_eq!(parse_gem_id("0x42").unwrap(), U256::from(66));
    for value in ["0", "-1", "1e6", "1.5", "0x"] {
        assert!(parse_gem_id(value).is_err());
    }
    assert_eq!(parse_wusdc("26.85907").unwrap(), U256::from(26859070));
    for value in ["-1", "1.0000001", "1e3", "a", "1.2.3"] {
        assert!(parse_wusdc(value).is_err());
    }
    assert_eq!(
        units(U256::MAX, 6),
        format!(
            "{}.{}",
            U256::MAX / U256::from(1_000_000),
            U256::MAX % U256::from(1_000_000)
        )
    );
    let mut gem = gem_data(Address::ZERO, U256::from(1), U256::from(1), 2);
    gem.calledAt = 100;
    gem.callNoticePeriod = 10;
    settleable(&gem, 110).unwrap();
    assert!(settleable(&gem, 111).is_err());
    gem.state = 0;
    assert!(settleable(&gem, 100).is_err());
    gem.state = 1;
    settleable(&gem, 999).unwrap();
}

#[test]
fn send_cli_accepts_only_top_level_send_and_exact_native_amounts() {
    let recipient = "0x89a12ac6dE30463278eB4A0eEeBF2DA16eA9637d";
    let options = Options::try_parse_from([
        "rudis-gems",
        "send",
        recipient,
        "--rudis",
        "5000000",
        "--private-key",
        "test-only",
    ])
    .unwrap();
    let Command::Send(args) = options.command else {
        panic!("expected send")
    };
    assert_eq!(args.to_address, recipient.parse::<Address>().unwrap());
    assert_eq!(
        args.rudis,
        U256::from_str_radix("5000000000000000000000000", 10).unwrap()
    );
    assert!(
        Options::try_parse_from(["rudis-gems", "list", "send", recipient, "--rudis", "1"]).is_err()
    );
    assert!(Options::try_parse_from(["rudis-gems", "list"]).is_ok());
    assert_eq!(parse_rudis("0.000000000000000001").unwrap(), U256::from(1));
    assert_eq!(parse_rudis(&units(U256::MAX, 18)).unwrap(), U256::MAX);
    for invalid in [
        "0",
        "0.0",
        "-1",
        "1e6",
        "NaN",
        "1.0000000000000000001",
        "1.2.3",
        "",
        " 1",
    ] {
        assert!(parse_rudis(invalid).is_err(), "accepted {invalid}");
    }
    assert!(parse_rudis(&U256::MAX.to_string()).is_err());
    assert!(parse_recipient("0x0000000000000000000000000000000000000000").is_err());
}

struct TransferRpc {
    owner: Address,
    recipient: Address,
    amount: U256,
    balance: U256,
    chain: u64,
    pending: PathBuf,
    raw: Mutex<Option<Bytes>>,
    sends: AtomicUsize,
    lose_response: AtomicBool,
    reverted: AtomicBool,
}

impl Rpc for TransferRpc {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        use alloy_consensus::{Transaction, TxEnvelope};
        use alloy_eips::eip2718::Decodable2718;
        Ok(match method {
            "eth_chainId" => json!(format!("{:#x}", self.chain)),
            "eth_getTransactionCount" => {
                assert_eq!(params, json!([self.owner, "pending"]));
                json!("0x9")
            }
            "eth_getBlockByNumber" => json!({"baseFeePerGas":"0x7"}),
            "eth_estimateGas" => {
                assert_eq!(params[0]["from"], json!(self.owner));
                assert_eq!(params[0]["to"], json!(self.recipient));
                assert_eq!(params[0]["value"], json!(format!("{:#x}", self.amount)));
                assert_eq!(params[0]["data"], "0x");
                assert_eq!(params[0]["gasPrice"], "0xe");
                json!("0x5208")
            }
            "eth_getBalance" => {
                assert_eq!(params, json!([self.owner, "pending"]));
                json!(format!("{:#x}", self.balance))
            }
            "eth_sendRawTransaction" => {
                let raw: Bytes = serde_json::from_value(params[0].clone())?;
                let saved: Value = store::read_json(&self.pending)?;
                assert_eq!(saved["transaction"]["raw"], params[0]);
                let tx = TxEnvelope::decode_2718(&mut raw.as_ref())?;
                assert_eq!(tx.chain_id(), Some(CHAIN));
                assert_eq!(tx.to(), Some(self.recipient));
                assert_eq!(tx.value(), self.amount);
                assert_eq!(tx.nonce(), 9);
                assert_eq!(tx.gas_limit(), 25_200);
                assert_eq!(tx.gas_price(), Some(14));
                assert!(tx.input().is_empty());
                self.sends.fetch_add(1, Ordering::Relaxed);
                *self.raw.lock().unwrap() = Some(raw.clone());
                json!(keccak256(&raw))
            }
            "eth_getTransactionReceipt" => {
                if let Some(raw) = self.raw.lock().unwrap().as_ref() {
                    if self.lose_response.load(Ordering::Relaxed) {
                        eyre::bail!("simulated connection loss after acceptance");
                    }
                    assert_eq!(params[0], json!(keccak256(raw)));
                    json!({"transactionHash":params[0],"status":if self.reverted.load(Ordering::Relaxed) {"0x0"} else {"0x1"}})
                } else {
                    Value::Null
                }
            }
            _ => panic!("Unexpected native-transfer RPC method {method}"),
        })
    }
}

fn transfer_rpc(wallet: &Wallet, root: &Path) -> TransferRpc {
    let amount = parse_rudis("5000000.000000000000000001").unwrap();
    TransferRpc {
        owner: wallet.address,
        recipient: "0x89a12ac6dE30463278eB4A0eEeBF2DA16eA9637d"
            .parse()
            .unwrap(),
        amount,
        balance: amount + U256::from(25_200 * 14),
        chain: CHAIN,
        pending: root
            .join(CHAIN.to_string())
            .join(format!("{:#x}", wallet.address))
            .join("transfers/pending.json"),
        raw: Mutex::new(None),
        sends: AtomicUsize::new(0),
        lose_response: AtomicBool::new(false),
        reverted: AtomicBool::new(false),
    }
}

#[tokio::test]
async fn native_send_preserves_signed_value_and_resumes_lost_response() {
    let wallet = wallet();
    let dir = tempfile::tempdir().unwrap();
    let rpc = transfer_rpc(&wallet, dir.path());
    rpc.lose_response.store(true, Ordering::Relaxed);
    assert!(
        transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, false)
            .await
            .is_err()
    );
    assert!(rpc.pending.exists());
    assert!(transfer::send(
        &rpc,
        &wallet,
        dir.path(),
        rpc.recipient,
        rpc.amount + U256::from(1),
        false
    )
    .await
    .is_err());
    rpc.lose_response.store(false, Ordering::Relaxed);
    transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, false)
        .await
        .unwrap();
    assert_eq!(rpc.sends.load(Ordering::Relaxed), 1);
    assert!(!rpc.pending.exists());
    let hash = keccak256(rpc.raw.lock().unwrap().as_ref().unwrap());
    assert!(rpc
        .pending
        .parent()
        .unwrap()
        .join(format!("{hash:#x}.json"))
        .exists());
}

#[tokio::test]
async fn native_send_checks_gas_funds_chain_and_dry_run_before_broadcast() {
    let wallet = wallet();
    let dir = tempfile::tempdir().unwrap();
    let mut rpc = transfer_rpc(&wallet, dir.path());
    transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, true)
        .await
        .unwrap();
    assert!(!rpc.pending.parent().unwrap().exists());
    rpc.balance -= U256::from(1);
    assert!(
        transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("plus gas")
    );
    rpc.balance += U256::from(1);
    rpc.chain = 1;
    assert!(
        transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("chain")
    );
    assert_eq!(rpc.sends.load(Ordering::Relaxed), 0);
    assert!(!rpc.pending.exists());
}

#[tokio::test]
async fn native_send_reports_reverted_receipt() {
    let wallet = wallet();
    let dir = tempfile::tempdir().unwrap();
    let rpc = transfer_rpc(&wallet, dir.path());
    rpc.reverted.store(true, Ordering::Relaxed);
    assert!(
        transfer::send(&rpc, &wallet, dir.path(), rpc.recipient, rpc.amount, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("reverted")
    );
    assert_eq!(rpc.sends.load(Ordering::Relaxed), 1);
    assert!(!rpc.pending.exists());
}

#[test]
fn pow_and_mac_match_protocol_and_independent_python_vector() {
    let nonce = pow_nonce(U256::MAX).unwrap();
    outbe_common::pow::validate_pow(U256::MAX, nonce).unwrap();
    let keys = Keys::for_test();
    let owner = Address::repeat_byte(0x12);
    let amount = U256::from(2684948507639u64);
    let mac = keys.mac(owner, PromisOp::Burn, amount, 8, CHAIN);
    assert_eq!(
        format!("{mac:x}"),
        "476292cac040fec0dd6b66e02d7a0105e4d56ae850dc16c80c01b93bc752ff88"
    );
    assert_ne!(mac, keys.mac(owner, PromisOp::Mint, amount, 8, CHAIN));
    assert_ne!(mac, keys.mac(owner, PromisOp::Burn, amount, 7, CHAIN));
    assert_ne!(mac, keys.mac(owner, PromisOp::Burn, amount, 8, CHAIN + 1));
    assert_eq!(keys.balance(owner, &[]).unwrap(), U256::ZERO);
    assert!(keys.balance(owner, &[0; 56]).is_err());
}

#[test]
fn real_paynote_proof_binds_owner_asset_amount_and_recovers_change() {
    let dir = tempfile::tempdir().unwrap();
    let owner = wallet().address;
    let amount = (U256::from(1) << 199) + U256::from(123);
    let note = paynote::Note::random(CHAIN, WUSDC, amount).unwrap();
    let saved = note.save(dir.path()).unwrap();
    let note: paynote::Note = store::read_json(&saved).unwrap();
    note.validate().unwrap();
    let mut tree = paynote::new_tree(CHAIN).unwrap();
    tree.append(note.commitment.to_field().unwrap()).unwrap();
    let spent = amount - U256::from(17);
    let (proof, change, root) = paynote::prove(&note, spent, owner, &tree).unwrap();
    let public = decode_public_inputs(&proof).unwrap();
    assert_eq!(public.chain_id, CHAIN);
    assert_eq!(public.root, root.to_field().unwrap());
    assert_eq!(public.owner, owner.to_field().unwrap());
    assert_eq!(public.asset, WUSDC.to_field().unwrap());
    assert_eq!(
        public.spend_amount,
        PayNoteSuite::fields_from_u256(&spent).unwrap().map(|f| {
            let word = PayNoteSuite::field_to_b256(&f).unwrap();
            U256::from_be_slice(word.as_slice()).to::<u128>()
        })
    );
    let change = change.unwrap();
    assert_eq!(change.amount().unwrap(), U256::from(17));
    assert_ne!(change.commitment, note.commitment);
    change.save(dir.path()).unwrap();
    let original: paynote::Note = store::read_json(&saved).unwrap();
    assert_eq!(original.commitment, note.commitment);
    let mut tampered = proof;
    tampered[4 + 4 * 32 + 31] ^= 1; // Alter public owner after proving.
    assert!(!outbe_zk_backend::barretenberg::verify_circuit::<
        outbe_zk_canonical::noir::paynote::Paynote,
    >(&tampered)
    .unwrap_or(false));
}

struct TxRpc {
    journal: PathBuf,
    confirmed: AtomicBool,
    reject_receipt: AtomicBool,
    sends: AtomicUsize,
    raw: Mutex<Option<Bytes>>,
}
impl Rpc for TxRpc {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        Ok(match method {
            "eth_chainId" => json!(format!("{CHAIN:#x}")),
            "eth_getTransactionCount" => json!("0x0"),
            "eth_getBlockByNumber" => json!({"baseFeePerGas":"0x7"}),
            "eth_estimateGas" => json!("0x5208"),
            "eth_sendRawTransaction" => {
                let raw: Bytes = serde_json::from_value(params[0].clone())?;
                let journal: Value = store::read_json(&self.journal)?;
                assert_eq!(journal["transactions"]["deposit"]["raw"], params[0]);
                self.sends.fetch_add(1, Ordering::Relaxed);
                *self.raw.lock().unwrap() = Some(raw.clone());
                self.confirmed.store(true, Ordering::Relaxed);
                json!(keccak256(&raw))
            }
            "eth_getTransactionReceipt" => {
                if self.reject_receipt.load(Ordering::Relaxed)
                    && self.confirmed.load(Ordering::Relaxed)
                {
                    eyre::bail!("simulated connection loss after acceptance");
                }
                if self.confirmed.load(Ordering::Relaxed) {
                    json!({"transactionHash":params[0],"status":"0x1","logs":[]})
                } else {
                    Value::Null
                }
            }
            _ => panic!("Unexpected RPC method {method}"),
        })
    }
}

#[tokio::test]
async fn signed_deposit_survives_lost_response_and_does_not_broadcast_twice() {
    let dir = tempfile::tempdir().unwrap();
    let wallet = wallet();
    let id = U256::from(42);
    let mut journal = Journal::new(dir.path(), wallet.address, id, U256::from(99), CHAIN).unwrap();
    let rpc = TxRpc {
        journal: dir.path().join("operation.json"),
        confirmed: AtomicBool::new(false),
        reject_receipt: AtomicBool::new(true),
        sends: AtomicUsize::new(0),
        raw: Mutex::new(None),
    };
    let result = journal
        .send(&rpc, &wallet, "deposit", PAYNOTE, vec![1, 2, 3])
        .await;
    assert!(result.is_err());
    rpc.reject_receipt.store(false, Ordering::Relaxed);
    let mut resumed = Journal::load(dir.path(), wallet.address, id, CHAIN)
        .unwrap()
        .unwrap();
    resumed.resume(&rpc, "deposit").await.unwrap().unwrap();
    resumed.resume(&rpc, "deposit").await.unwrap().unwrap();
    assert_eq!(rpc.sends.load(Ordering::Relaxed), 1);
    assert!(Journal::load(dir.path(), wallet.address, id + U256::from(1), CHAIN).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&rpc.journal)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn reverted_receipt_stops_and_clears_only_the_failed_step() {
    struct Reverted;
    impl Rpc for Reverted {
        async fn request(&self, method: &str, params: Value) -> Result<Value> {
            assert_eq!(method, "eth_getTransactionReceipt");
            Ok(json!({"transactionHash":params[0],"status":"0x0"}))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let owner = wallet().address;
    let raw = Bytes::from(vec![1, 2, 3]);
    store::save_json(
        &dir.path().join("operation.json"),
        &json!({"version":1,"chainId":CHAIN.to_string(),"owner":owner,
        "gemId":"42","amount":"99","transactions":{"mine":{"raw":raw,"hash":keccak256(&raw)}}}),
        false,
    )
    .unwrap();
    let mut journal = Journal::load(dir.path(), owner, U256::from(42), CHAIN)
        .unwrap()
        .unwrap();
    assert!(journal
        .resume(&Reverted, "mine")
        .await
        .unwrap_err()
        .to_string()
        .contains("reverted"));
    assert!(!journal.has("mine"));
}

#[test]
fn operation_lock_releases_on_drop_and_secrets_are_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let lock = store::lock(dir.path()).unwrap();
    assert!(store::lock(dir.path()).is_err());
    drop(lock);
    store::lock(dir.path()).unwrap();
    let note = paynote::Note::random(CHAIN, WUSDC, U256::from(123)).unwrap();
    let source = dir.path().join("source-note.json");
    store::save_json(&source, &note, false).unwrap();
    assert!(store::save_json(&source, &json!({"replacement":true}), false).is_err());
    let recovered = load_source_note(dir.path(), U256::from(1)).unwrap();
    assert_eq!(recovered.commitment, note.commitment);
}

#[test]
fn malformed_deposit_marker_cannot_escape_operation_directory() {
    let dir = tempfile::tempdir().unwrap();
    store::save_json(
        &dir.path().join("deposit-started.json"),
        &json!({"originalNote":"../../secret.json"}),
        false,
    )
    .unwrap();
    assert!(load_source_note(dir.path(), U256::from(1)).is_err());
}

fn hkdf_test(salt: &[u8], input: &[u8], info: &[u8]) -> [u8; 32] {
    struct Len;
    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            32
        }
    }
    let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt);
    let extracted = salt.extract(input);
    let infos = [info];
    let mut key = [0; 32];
    extracted
        .expand(&infos, Len)
        .unwrap()
        .fill(&mut key)
        .unwrap();
    key
}

fn seal_test(key: &[u8], nonce: [u8; 12], data: &[u8]) -> Bytes {
    use ring::aead;
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).unwrap());
    let mut data = data.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut data,
    )
    .unwrap();
    data.into()
}

struct MiningRpc {
    owner: Address,
    amount: U256,
    state: Mutex<MiningState>,
}
struct MiningState {
    op_nonce: u64,
    balance: U256,
    mined: bool,
    native: U256,
    tx_nonce: u64,
    receipts: std::collections::BTreeMap<B256, Value>,
    methods: Vec<String>,
}

fn event_log<E: alloy_sol_types::SolEvent>(event: E, address: Address) -> Value {
    let log = event.encode_log_data();
    json!({"address":address,"topics":log.topics(),"data":log.data})
}

impl Rpc for MiningRpc {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        use alloy_consensus::{Transaction, TxEnvelope};
        use alloy_eips::eip2718::Decodable2718;
        use alloy_sol_types::SolValue;
        let mut state = self.state.lock().unwrap();
        match method {
            "eth_chainId" => Ok(json!(format!("{CHAIN:#x}"))),
            "eth_getBlockByNumber" => {
                Ok(json!({"baseFeePerGas":"0x7","number":"0x1","timestamp":"0x64"}))
            }
            "eth_getTransactionCount" => Ok(json!(format!("{:#x}", state.tx_nonce))),
            "eth_estimateGas" => Ok(json!("0x186a0")),
            "eth_getBalance" => Ok(json!(format!("{:#x}", state.native))),
            "rudis_deriveKeys" => {
                use outbe_tee::protocol::{derive_account_keys_message, eip191_hash, Ledger};
                use ring::agreement;
                assert_eq!(params[0], "Promis");
                assert_eq!(params[1], json!(self.owner));
                let client_public: B256 = serde_json::from_value(params[2].clone())?;
                let signature: Bytes = serde_json::from_value(params[3].clone())?;
                let digest = eip191_hash(&derive_account_keys_message(
                    Ledger::Promis,
                    self.owner,
                    client_public,
                ));
                let signature_part = k256::ecdsa::Signature::from_slice(&signature[..64])?;
                let recovery = k256::ecdsa::RecoveryId::from_byte(signature[64] - 27).unwrap();
                let public = k256::ecdsa::VerifyingKey::recover_from_prehash(
                    digest.as_slice(),
                    &signature_part,
                    recovery,
                )?;
                assert_eq!(
                    Address::from_slice(
                        &keccak256(&public.to_encoded_point(false).as_bytes()[1..])[12..]
                    ),
                    self.owner
                );
                let secret = agreement::EphemeralPrivateKey::generate(
                    &agreement::X25519,
                    &SystemRandom::new(),
                )
                .unwrap();
                let public = B256::from_slice(secret.compute_public_key().unwrap().as_ref());
                let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, client_public);
                let key = agreement::agree_ephemeral(secret, &peer, |shared| {
                    hkdf_test(client_public.as_slice(), shared, b"outbe/tee/dkg-share/v1")
                })
                .unwrap();
                let nonce = [9; 12];
                Ok(
                    json!({"sealed":seal_test(&key,nonce,&[7;64]),"nonce":Bytes::from(nonce.to_vec()),"enclaveEphemeralPubkey":public}),
                )
            }
            "eth_call" => {
                let to: Address = serde_json::from_value(params[0]["to"].clone())?;
                let data: Bytes = serde_json::from_value(params[0]["data"].clone())?;
                let result = if to == GEM && data.starts_with(&IGem::getGemStatusCall::SELECTOR) {
                    assert!(!state.mined, "must not query a burned GEM during recovery");
                    gem_data(self.owner, U256::from(42), self.amount, 3).abi_encode()
                } else if to == PROMIS && data.starts_with(&IPromis::opNonceOfCall::SELECTOR) {
                    state.op_nonce.abi_encode()
                } else if to == PROMIS && data.starts_with(&IPromis::balanceOfCall::SELECTOR) {
                    let version = state.op_nonce.to_be_bytes();
                    let mut input = self.owner.as_slice().to_vec();
                    input.push(0);
                    input.extend_from_slice(&version);
                    let nonce = hkdf_test(&[7; 32], &input, b"outbe/promis/nonce/v1");
                    let ciphertext = seal_test(
                        &[7; 32],
                        nonce[..12].try_into().unwrap(),
                        &state.balance.to_be_bytes::<32>(),
                    );
                    let mut blob = version.to_vec();
                    blob.extend_from_slice(&ciphertext);
                    Bytes::from(blob).abi_encode()
                } else if to == WUSDC && data.starts_with(&IERC20::balanceOfCall::SELECTOR) {
                    U256::from(99_000_000).abi_encode()
                } else {
                    panic!("Unexpected eth_call to {to}: {}", hex::encode(&data));
                };
                Ok(json!(Bytes::from(result)))
            }
            "eth_getTransactionReceipt" => {
                let hash: B256 = serde_json::from_value(params[0].clone())?;
                Ok(state.receipts.get(&hash).cloned().unwrap_or(Value::Null))
            }
            "eth_sendRawTransaction" => {
                let raw: Bytes = serde_json::from_value(params[0].clone())?;
                let tx = TxEnvelope::decode_2718(&mut raw.as_ref())?;
                assert_eq!(tx.chain_id(), Some(CHAIN));
                assert_eq!(tx.nonce(), state.tx_nonce);
                let hash = keccak256(&raw);
                let event = if tx.to() == Some(FACTORY) {
                    assert!(!state.mined);
                    let call = IGemFactory::minePromisCall::abi_decode_validate(tx.input())?;
                    assert_eq!(call.gemId, U256::from(42));
                    assert_eq!(call.opNonce, state.op_nonce);
                    assert_eq!(
                        call.mac,
                        Keys::for_test().mac(
                            self.owner,
                            PromisOp::Mint,
                            self.amount,
                            state.op_nonce,
                            CHAIN
                        )
                    );
                    outbe_common::pow::validate_pow(call.gemId, call.nonce).unwrap();
                    state.mined = true;
                    state.balance += self.amount;
                    state.methods.push("minePromis".to_owned());
                    event_log(
                        IGemFactory::GemMined {
                            gemId: call.gemId,
                            owner: self.owner,
                            promisLoad: self.amount,
                        },
                        FACTORY,
                    )
                } else {
                    assert_eq!(tx.to(), Some(PROMIS_FACTORY));
                    assert!(state.mined);
                    let call = IRudisFactory::mineRudisCall::abi_decode_validate(tx.input())?;
                    assert_eq!(call.amount, self.amount);
                    assert_eq!(call.opNonce, state.op_nonce);
                    assert_eq!(
                        call.mac,
                        Keys::for_test().mac(
                            self.owner,
                            PromisOp::Burn,
                            self.amount,
                            state.op_nonce,
                            CHAIN
                        )
                    );
                    state.balance -= call.amount;
                    let native = call.amount * U256::from(NATIVE_PER_PROMIS_MINOR);
                    state.native += native;
                    state.methods.push("mineRudis".to_owned());
                    event_log(
                        IRudisFactory::RudisMined {
                            sender: self.owner,
                            amount: native,
                        },
                        PROMIS_FACTORY,
                    )
                };
                state.op_nonce += 1;
                state.tx_nonce += 1;
                state.receipts.insert(
                    hash,
                    json!({"transactionHash":hash,"status":"0x1","logs":[event]}),
                );
                Ok(json!(hash))
            }
            _ => panic!("Unexpected RPC method {method}"),
        }
    }
}

#[tokio::test]
async fn settled_gem_mines_converts_with_fresh_mac_and_resumes_without_repeating() {
    let wallet = wallet();
    let amount = U256::from(2_685_907_082_970u64);
    let existing = U256::from(123_456);
    let rpc = MiningRpc {
        owner: wallet.address,
        amount,
        state: Mutex::new(MiningState {
            op_nonce: 7,
            balance: existing,
            mined: false,
            native: U256::ZERO,
            tx_nonce: 0,
            receipts: Default::default(),
            methods: Vec::new(),
        }),
    };
    let dir = tempfile::tempdir().unwrap();
    settle(&rpc, &wallet, dir.path(), U256::from(42), None, false)
        .await
        .unwrap();
    settle(&rpc, &wallet, dir.path(), U256::from(42), None, false)
        .await
        .unwrap();
    let state = rpc.state.lock().unwrap();
    assert_eq!(state.methods, ["minePromis", "mineRudis"]);
    assert_eq!(state.balance, existing);
    assert_eq!(state.native, amount * U256::from(NATIVE_PER_PROMIS_MINOR));
    assert_eq!(state.op_nonce, 9);
}
