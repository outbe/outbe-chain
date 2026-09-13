use crate::world::rpc::*;

/// Consensus commitments observed for one canonical block on one validator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockCommitmentV1 {
    pub block_hash: B256,
    pub state_root: B256,
    pub ce_root: B256,
}

/// One exact canonical block observed only after every requested node finalized it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedCheckpoint {
    pub height: u64,
    pub block_hash: B256,
    pub state_root: B256,
}

impl Rpc {
    // ---- reads ----------------------------------------------------------

    /// Head block number on the node at `port` (`eth_blockNumber`).
    pub fn head(&self, port: u16) -> Option<u64> {
        eth::block_number(&self.url(port))
    }

    /// Finalized block number on the node at `port`.
    pub fn finalized(&self, port: u16) -> Option<u64> {
        eth::finalized_number(&self.url(port))
    }

    /// Finalized height with transport, shape, and decode errors preserved.
    pub fn finalized_result(&self, port: u16) -> Result<u64> {
        let value = eth::raw_json_result(
            &self.url(port),
            "eth_getBlockByNumber",
            serde_json::json!(["finalized", false]),
        )
        .wrap_err_with(|| format!("read finalized block from RPC port {port}"))?;
        let number = value
            .get("number")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("finalized block on RPC port {port} omitted number"))?;
        u64::from_str_radix(number.trim_start_matches("0x"), 16)
            .wrap_err_with(|| format!("decode finalized height {number} from RPC port {port}"))
    }

    /// Require one exact canonical block identity from a node.
    pub fn checkpoint_at(&self, port: u16, height: u64) -> Result<FinalizedCheckpoint> {
        let (block_hash, state_root, _) = eth::block_commitment_result(&self.url(port), height)
            .wrap_err_with(|| format!("read checkpoint h{height} from RPC port {port}"))?;
        Ok(FinalizedCheckpoint {
            height,
            block_hash,
            state_root,
        })
    }

    pub fn finalize_outcome(
        &self,
        outcome: &TxOutcome,
        ports: &[u16],
        tries: u32,
    ) -> Result<FinalizedCheckpoint> {
        let receipt_success = match outcome
            .receipt
            .get("status")
            .and_then(serde_json::Value::as_str)
        {
            Some("0x1") => true,
            Some("0x0") => false,
            _ => return Err(eyre!("mined receipt omitted or malformed status")),
        };
        ensure!(
            outcome.success == receipt_success,
            "transaction outcome contradicts mined receipt status"
        );
        let transaction_hash: B256 = outcome
            .transaction_hash
            .parse()
            .wrap_err("parse transaction outcome hash")?;
        let receipt_transaction_hash: B256 = serde_json::from_value(
            outcome
                .receipt
                .get("transactionHash")
                .cloned()
                .unwrap_or_default(),
        )
        .wrap_err("parse mined receipt transactionHash")?;
        ensure!(
            transaction_hash == receipt_transaction_hash,
            "transaction outcome contradicts mined receipt transactionHash"
        );
        ensure!(
            outcome.success,
            "cannot finalize a reverted transaction outcome"
        );
        ensure!(
            !ports.is_empty(),
            "finalized outcome requires at least one RPC"
        );
        let height = outcome
            .block_number()
            .ok_or_else(|| eyre!("mined receipt omitted or malformed blockNumber"))?;
        let receipt_hash = outcome.block_hash()?;
        self.wait_finalized_checkpoint(ports, height, tries)?;
        let mut expected = None;
        for &port in ports {
            let observed = self.checkpoint_at(port, height)?;
            ensure!(
                observed.block_hash == receipt_hash,
                "RPC port {port} canonical hash at h{height} differs from receipt: {} != {receipt_hash}",
                observed.block_hash
            );
            if let Some(expected) = expected {
                ensure!(
                    observed == expected,
                    "RPC port {port} disagrees on finalized checkpoint h{height}: {observed:?} != {expected:?}"
                );
            } else {
                expected = Some(observed);
            }
        }
        expected.ok_or_else(|| eyre!("finalized outcome had no checkpoint observations"))
    }

    /// Capture a post-event progress target from every expected live node.
    /// Call only after repair completes: neither an unavailable node nor a
    /// faster peer may disappear from the anchor used to prove fresh finality.
    pub fn fresh_finality_target(&self, ports: &[u16]) -> Result<u64> {
        ensure!(
            !ports.is_empty(),
            "fresh finality requires at least one RPC"
        );
        let mut maximum = 0;
        for &port in ports {
            maximum = maximum.max(self.finalized_result(port)?);
        }
        maximum
            .checked_add(2)
            .ok_or_else(|| eyre!("post-repair finalized target overflow"))
    }

    /// Wait until every expected node has finalized `min_height`, then require
    /// exact hash/root agreement at one shared finalized height.
    pub fn wait_finalized_checkpoint(
        &self,
        ports: &[u16],
        min_height: u64,
        tries: u32,
    ) -> Result<FinalizedCheckpoint> {
        ensure!(
            !ports.is_empty(),
            "finalized checkpoint wait requires at least one RPC"
        );
        let mut last_observation = String::new();
        for _ in 0..tries {
            let observations = ports
                .iter()
                .map(|&port| (port, self.finalized_result(port)))
                .collect::<Vec<_>>();
            if observations
                .iter()
                .all(|(_, height)| height.as_ref().is_ok_and(|height| *height >= min_height))
            {
                let height = observations
                    .iter()
                    .filter_map(|(_, height)| height.as_ref().ok().copied())
                    .min()
                    .ok_or_else(|| eyre!("finalized checkpoint observations disappeared"))?;
                let expected = self.checkpoint_at(ports[0], height)?;
                for &port in &ports[1..] {
                    let observed = self.checkpoint_at(port, height)?;
                    ensure!(
                        observed == expected,
                        "RPC port {port} disagrees on finalized checkpoint h{height}: {observed:?} != {expected:?}"
                    );
                }
                return Ok(expected);
            }
            last_observation = observations
                .into_iter()
                .map(|(port, height)| match height {
                    Ok(height) => format!("{port}=h{height}"),
                    Err(error) => format!("{port}=unavailable({error:#})"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            sleep(Duration::from_secs(3));
        }
        Err(eyre!(
            "RPC ports did not converge on finalized h{min_height} after {tries} attempts; last observations: {last_observation}"
        ))
    }

    /// Timestamp of the latest block, in EVM seconds.
    pub fn latest_block_timestamp(&self, port: u16) -> Option<u64> {
        eth::raw_json_with_params(
            &self.url(port),
            "eth_getBlockByNumber",
            serde_json::json!(["latest", false]),
        )
        .and_then(|block| block.get("timestamp").cloned())
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
    }

    /// Timestamp of one exact canonical block, in EVM seconds.
    pub fn block_timestamp(&self, port: u16, height: u64) -> Option<u64> {
        eth::raw_json_with_params(
            &self.url(port),
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{height:x}"), false]),
        )
        .and_then(|block| block.get("timestamp").cloned())
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
    }

    /// `stateRoot` of block `height` on the node at `port`.
    pub fn state_root(&self, port: u16, height: u64) -> Option<String> {
        eth::state_root(&self.url(port), height)
    }

    /// Canonical block/state/CE roots imported by one validator at `height`.
    pub fn block_commitment(&self, port: u16, height: u64) -> Option<BlockCommitmentV1> {
        let (block_hash, state_root, extra_data) = eth::block_commitment(&self.url(port), height)?;
        let artifacts = decode_outbe_block_artifacts(&extra_data).ok()?;
        let ce_root = artifacts.compressed_entities_root?.r_sealed;
        Some(BlockCommitmentV1 {
            block_hash,
            state_root,
            ce_root,
        })
    }

    /// Canonical block hash at `height` on the node at `port`.
    pub fn block_hash(&self, port: u16, height: u64) -> Option<String> {
        eth::block_hash(&self.url(port), height)
    }

    /// TEE registry `isBootstrapped()` on the primary node.
    pub fn is_bootstrapped(&self) -> Result<bool> {
        eth::read_call_result(
            &self.cfg.rpc0,
            addresses::TEE_ADDR,
            &ITeeRegistryV1::isBootstrappedCall {},
        )
        .map_err(|error| eyre!("read TeeRegistry bootstrap state: {error}"))
    }

    // ---- waits (poll loops) --------------------------------------------

    /// Wait for HEAD to reach `min`, including the final observation after retries.
    /// This positions an execution-height trigger; it does not prove finality.
    #[must_use = "a block wait must be checked; ignoring it can turn a stalled node into PASS"]
    pub fn wait_block(&self, port: u16, min: u64, tries: u32) -> Result<u64> {
        let mut remaining = tries;
        loop {
            match eth::block_number_result(&self.url(port)) {
                Ok(height) if height >= min => return Ok(height),
                Ok(height) if remaining == 0 => {
                    return Err(eyre!(
                        "RPC {port} did not reach HEAD {min} after {tries} retries: last height {height}"
                    ));
                }
                Err(error) if remaining == 0 => {
                    return Err(error).wrap_err_with(|| {
                        format!("RPC {port} could not observe HEAD {min} after {tries} retries")
                    });
                }
                _ => {}
            }
            remaining -= 1;
            sleep(Duration::from_secs(3));
        }
    }

    /// Wait until head on `port` is strictly greater than `height`.
    #[must_use = "a block wait must be checked; ignoring it can turn a stalled node into PASS"]
    pub fn wait_block_gt(&self, port: u16, height: u64, tries: u32) -> Result<u64> {
        let target = height
            .checked_add(1)
            .ok_or_else(|| eyre!("RPC {port} cannot advance HEAD beyond u64::MAX"))?;
        self.wait_block(port, target, tries)
    }

    /// Wait for the primary node's TEE bootstrap (5s polls).
    #[must_use = "TEE bootstrap completion must be checked"]
    pub fn wait_bootstrapped(
        &self,
        tries: u32,
        mut ensure_processes_alive: impl FnMut() -> Result<()>,
    ) -> Result<bool> {
        let mut observed = false;
        let mut last_error = None;
        for _ in 0..tries {
            ensure_processes_alive()?;
            match self.is_bootstrapped() {
                Ok(true) => return Ok(true),
                Ok(false) => observed = true,
                Err(error) => last_error = Some(error),
            }
            sleep(Duration::from_secs(5));
        }
        if observed {
            Ok(false)
        } else {
            Err(last_error.unwrap_or_else(|| eyre!("TEE bootstrap state was never observable")))
        }
    }

    /// Poll until finalized height reaches `want` on `port`.
    #[must_use = "finality wait must be checked"]
    pub fn wait_finalized_at_least(&self, port: u16, want: u64, tries: u32) -> bool {
        for _ in 0..tries {
            if self.finalized(port).is_some_and(|height| height >= want) {
                return true;
            }
            sleep(Duration::from_secs(2));
        }
        self.finalized(port).is_some_and(|height| height >= want)
    }
}
