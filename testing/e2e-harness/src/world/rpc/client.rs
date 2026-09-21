use crate::world::rpc::*;

#[derive(Debug, Clone)]
pub struct Rpc {
    pub(super) cfg: Config,
}

/// Mined transaction result, including contract-level reverts.
#[derive(Clone, Debug)]
pub struct TxOutcome {
    pub transaction_hash: String,
    pub success: bool,
    pub receipt: serde_json::Value,
}

impl TxOutcome {
    /// Canonical block number from the mined receipt.
    pub fn block_number(&self) -> Option<u64> {
        let encoded = self.receipt.get("blockNumber")?.as_str()?;
        u64::from_str_radix(encoded.trim_start_matches("0x"), 16).ok()
    }

    /// Exact native fee charged by this transaction.
    pub fn gas_cost(&self) -> Option<U256> {
        Rpc::receipt_gas_cost(&self.receipt)
    }

    /// Canonical block hash carried by the mined receipt.
    pub fn block_hash(&self) -> Result<B256> {
        let encoded = self
            .receipt
            .get("blockHash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("mined receipt omitted blockHash"))?;
        encoded
            .parse()
            .wrap_err_with(|| format!("parse mined receipt blockHash {encoded}"))
    }
}

impl From<eth::MinedCallOutcome> for TxOutcome {
    fn from(outcome: eth::MinedCallOutcome) -> Self {
        Self {
            transaction_hash: outcome.transaction_hash,
            success: outcome.success,
            receipt: outcome.receipt,
        }
    }
}

impl Rpc {
    pub(crate) fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    pub(super) fn sh(&self) -> Sh<'_> {
        Sh::new(&self.cfg)
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn canonical_nonce_on(&self, port: u16, address: Address) -> Option<u64> {
        eth::canonical_nonce(&self.url(port), address)
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn gas_price_on(&self, port: u16) -> Option<u128> {
        eth::gas_price(&self.url(port))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn send_raw_transaction_on(&self, port: u16, raw_transaction: &[u8]) -> Result<String> {
        eth::send_raw_transaction(&self.url(port), raw_transaction)
    }

    pub(crate) fn url(&self, port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }

    /// `(pending, queued)` transaction counts from `txpool_status`. This is the
    /// pool's real state: `eth_pendingTransactions` does not reflect it.
    pub fn txpool_status(&self, port: u16) -> Option<(u64, u64)> {
        let status = eth::raw_json(&self.url(port), "txpool_status")?;
        let count = |field: &str| -> Option<u64> {
            let raw = status.get(field)?;
            match raw {
                serde_json::Value::String(text) => {
                    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
                }
                other => other.as_u64(),
            }
        };
        Some((count("pending")?, count("queued")?))
    }

    /// Whether the node at `port` still holds `tx_hash` in either sub-pool.
    pub fn txpool_has(&self, port: u16, tx_hash: &str) -> Result<bool> {
        Ok(self.txpool_location(port, tx_hash)?.is_some())
    }

    /// Exact sub-pool containing `tx_hash`, preserving transport/decode errors.
    pub fn txpool_location(&self, port: u16, tx_hash: &str) -> Result<Option<&'static str>> {
        let needle: B256 = tx_hash
            .parse()
            .wrap_err("parse requested transaction hash")?;
        let content =
            eth::raw_json_result(&self.url(port), "txpool_content", serde_json::json!([]))
                .wrap_err_with(|| format!("read txpool_content from RPC port {port}"))?;
        let mut found = None;
        let mut seen = BTreeMap::new();
        let mut positions = BTreeMap::new();
        for kind in ["pending", "queued"] {
            let section = content
                .get(kind)
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| {
                    eyre!("txpool_content omitted or malformed {kind} on RPC port {port}")
                })?;
            for (sender, nonces) in section {
                let sender: Address = sender.parse().wrap_err("parse txpool sender")?;
                let nonces = nonces.as_object().ok_or_else(|| {
                    eyre!("txpool {kind} sender {sender} has malformed nonce map")
                })?;
                for (nonce_key, transaction) in nonces {
                    let nonce: u64 = nonce_key
                        .parse()
                        .wrap_err("parse txpool decimal nonce key")?;
                    ensure!(
                        nonce.to_string() == *nonce_key,
                        "txpool nonce key is not canonical decimal: {nonce_key}"
                    );
                    let hash: B256 = serde_json::from_value(
                        transaction.get("hash").cloned().unwrap_or_default(),
                    )
                    .wrap_err("parse txpool transaction hash")?;
                    let from: Address = serde_json::from_value(
                        transaction.get("from").cloned().unwrap_or_default(),
                    )
                    .wrap_err("parse txpool transaction sender")?;
                    let encoded_nonce = transaction
                        .get("nonce")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| value.strip_prefix("0x"))
                        .ok_or_else(|| {
                            eyre!("txpool transaction {hash} omitted or malformed nonce")
                        })?;
                    let transaction_nonce = u64::from_str_radix(encoded_nonce, 16)
                        .wrap_err("parse txpool transaction nonce")?;
                    ensure!(
                        !encoded_nonce.is_empty()
                            && encoded_nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
                            && (encoded_nonce == "0" || !encoded_nonce.starts_with('0')),
                        "txpool transaction {hash} has noncanonical nonce quantity"
                    );
                    ensure!(
                        from == sender && transaction_nonce == nonce,
                        "txpool transaction {hash} contradicts sender/nonce map entry"
                    );
                    ensure!(
                        seen.insert(hash, kind).is_none(),
                        "txpool returned duplicate transaction {hash}"
                    );
                    ensure!(
                        positions.insert((sender, nonce), hash).is_none(),
                        "txpool returned conflicting transactions for sender {sender} nonce {nonce}"
                    );
                    if hash == needle {
                        found = Some(kind);
                    }
                }
            }
        }
        // Validate both complete sub-pools even after finding the requested hash.
        Ok(found)
    }

    /// Chain identity reported by the node at `port`.
    pub fn chain_id(&self, port: u16) -> Option<u64> {
        eth::raw_json(&self.url(port), "eth_chainId")?
            .as_str()
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
    }

    /// Fund the EOA derived from `recipient_key` with whole COEN from `funder`.
    pub fn fund_key(
        &self,
        funder: &Validator,
        recipient_key: &str,
        amount_coen: u64,
    ) -> Result<String> {
        let recipient = eth::address_of(recipient_key)
            .ok_or_else(|| eyre!("cannot derive funded recipient address"))?;
        eth::send_value(
            &self.cfg.rpc0,
            recipient,
            &funder.evm_key()?,
            eth::coen(amount_coen),
        )
    }

    /// Wait for a tx receipt; `true` on success, `false` on revert/timeout.
    #[must_use = "transaction completion must be checked"]
    pub fn wait_tx(&self, tx: &str, tries: u32) -> bool {
        for _ in 0..tries {
            match eth::receipt_success(&self.cfg.rpc0, tx) {
                Some(true) => return true,
                Some(false) => return false,
                None => {}
            }
            sleep(Duration::from_secs(3));
        }
        false
    }

    /// Native balance on a specific node, including precompile balances.
    pub fn balance_on(&self, port: u16, addr: &str) -> Option<U256> {
        eth::balance(&self.url(port), addr.parse().ok()?)
    }

    // ---- identity + sends ----------------------------------------------------

    /// EOA address for a private key (`0x`-hex).
    pub fn address_of(&self, key: &str) -> Option<String> {
        eth::address_of(key).map(|a| format!("{a:#x}"))
    }

    /// Wait until the submitted transaction is mined and assert its receipt succeeded.
    #[must_use = "receipt completion must be checked"]
    pub fn wait_successful_receipt(&self, tx_hash: &str, tries: u32) -> bool {
        self.wait_receipt_status(tx_hash, true, tries)
    }

    /// Wait until a transaction receipt exists with the expected success bit.
    #[must_use = "receipt status must be checked"]
    pub fn wait_receipt_status(&self, tx_hash: &str, expected: bool, tries: u32) -> bool {
        let started = Instant::now();
        for _ in 0..tries {
            match eth::receipt_success(&self.cfg.rpc0, tx_hash) {
                Some(status) => {
                    let receipt = eth::receipt_json(&self.cfg.rpc0, tx_hash);
                    let block = receipt
                        .as_ref()
                        .and_then(|value| value.get("blockNumber"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let block_hash = receipt
                        .as_ref()
                        .and_then(|value| value.get("blockHash"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let events = receipt
                        .as_ref()
                        .and_then(|value| value.get("logs"))
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, Vec::len);
                    eprintln!(
                        "E2E_TRIBUTE_TIMELINE stage=receipt wall_ms={} wait_elapsed_ms={} tx={tx_hash} status={status} block={block} block_hash={block_hash} events={events} head={:?} finalized={:?}",
                        unix_time_millis(),
                        started.elapsed().as_millis(),
                        self.head(self.cfg.primary_port()),
                        self.finalized(self.cfg.primary_port()),
                    );
                    return status == expected;
                }
                None => sleep(Duration::from_millis(500)),
            }
        }
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=receipt-timeout wall_ms={} wait_elapsed_ms={} tx={tx_hash} expected_status={expected} head={:?} finalized={:?}",
            unix_time_millis(),
            started.elapsed().as_millis(),
            self.head(self.cfg.primary_port()),
            self.finalized(self.cfg.primary_port()),
        );
        false
    }

    /// Canonical block number carried by a mined public receipt.
    pub fn receipt_block_number(&self, tx_hash: &str, port: u16) -> Option<u64> {
        let receipt = eth::receipt_json(&self.url(port), tx_hash)?;
        let encoded = receipt.get("blockNumber")?.as_str()?;
        u64::from_str_radix(encoded.trim_start_matches("0x"), 16).ok()
    }

    /// Public JSON-RPC receipt used by OCOMP evidence correlation.
    pub fn transaction_receipt(&self, tx_hash: &str, port: u16) -> Option<serde_json::Value> {
        eth::receipt_json(&self.url(port), tx_hash)
    }

    /// Exact native fee charged for a public receipt.
    pub fn receipt_gas_cost(receipt: &serde_json::Value) -> Option<U256> {
        let gas_used = receipt.get("gasUsed")?.as_str()?;
        let gas_price = receipt.get("effectiveGasPrice")?.as_str()?;
        parse_rpc_u256(gas_used)?.checked_mul(parse_rpc_u256(gas_price)?)
    }
}

pub(in crate::world::rpc) fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(in crate::world::rpc) fn receipt_status(receipt: &serde_json::Value) -> bool {
    matches!(receipt.get("status"), Some(serde_json::Value::Bool(true)))
        || receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1")
}

fn parse_rpc_u256(value: &str) -> Option<U256> {
    U256::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn parse_rpc_word(encoded: &str) -> Option<U256> {
    U256::from_str_radix(encoded.trim_start_matches("0x"), 16).ok()
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn decode_rpc_data_words(
    log: &serde_json::Value,
    expected: usize,
) -> Option<Vec<U256>> {
    let bytes = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
    if bytes.len() != expected.checked_mul(32)? {
        return None;
    }
    Some(
        bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|word| U256::from_be_bytes(*word))
            .collect::<Vec<_>>(),
    )
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn rpc_log_block_number(log: &serde_json::Value) -> Option<u64> {
    u64::from_str_radix(
        log.get("blockNumber")?.as_str()?.trim_start_matches("0x"),
        16,
    )
    .ok()
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn canonical_rpc_log_block_hash(
    rpc_url: &str,
    log: &serde_json::Value,
    block_number: u64,
) -> Option<B256> {
    let observed = log.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
    let canonical = eth::block_hash(rpc_url, block_number)?
        .parse::<B256>()
        .ok()?;
    (observed == canonical).then_some(observed)
}

pub(in crate::world::rpc) fn receipt_has_log(
    receipt: &serde_json::Value,
    address: Address,
    topic0: Option<&str>,
) -> bool {
    receipt["logs"].as_array().is_some_and(|logs| {
        logs.iter().any(|log| {
            log["address"]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case(&format!("{address:#x}")))
                && topic0.is_none_or(|topic| {
                    log["topics"][0]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(topic))
                })
        })
    })
}

sol!("../../contracts/precompiles/src/IOracle.sol");

sol!("../../contracts/precompiles/src/ITributeFactory.sol");
