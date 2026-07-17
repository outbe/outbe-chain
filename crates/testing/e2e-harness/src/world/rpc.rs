//! Chain-interaction handle: reads/sends natively via alloy ([`crate::internal::eth`]),
//! governance/tribute sends via `outbe-cli`, and the poll/wait loops that back the
//! scenarios.
//!
//! This is the typed replacement for the `cast`-based RPC readers and the
//! scenario polling helpers used by the lifecycle and update flows.
//! Reads return `Option` — `None` is the analogue of the shell
//! `2>/dev/null || echo dn`. Only governance (`vote`), tribute, `confirm-ready`,
//! and `slash config` still go through `outbe-cli` (the product CLI under test).

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, U256};
use eyre::{eyre, Result, WrapErr as _};
use outbe_compressed_entities::{PointReadRequestV1, PointReadResultV1, SelectedHeaderV1};

use crate::internal::{
    addresses,
    config::Config,
    eth::{self, IStaking, ITeeRegistry, ITribute, IUpdate, IValidatorSet, IVote, IWorldwideDay},
    parse::{self, ScheduledUpdate, VoteStatus},
    shell::Sh,
};
use crate::world::validators::{Operator, Validator};

#[derive(Debug, Clone)]
pub struct Rpc {
    cfg: Config,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedEntityAtHeader {
    pub result: PointReadResultV1,
    pub header: SelectedHeaderV1,
}

impl Rpc {
    pub(crate) fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    fn sh(&self) -> Sh<'_> {
        Sh::new(&self.cfg)
    }

    fn url(&self, port: u16) -> String {
        format!("http://localhost:{port}")
    }

    // ---- reads ----------------------------------------------------------

    /// Head block number on the node at `port` (`eth_blockNumber`).
    pub fn head(&self, port: u16) -> Option<u64> {
        eth::block_number(&self.url(port))
    }

    /// Chain identity reported by the node at `port`.
    pub fn chain_id(&self, port: u16) -> Option<u64> {
        eth::raw_json(&self.url(port), "eth_chainId")?
            .as_str()
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
    }

    /// Finalized block number on the node at `port`.
    pub fn finalized(&self, port: u16) -> Option<u64> {
        eth::finalized_number(&self.url(port))
    }

    /// `stateRoot` of block `height` on the node at `port`.
    pub fn state_root(&self, port: u16, height: u64) -> Option<String> {
        eth::state_root(&self.url(port), height)
    }

    /// Fetch one latest-finalized compressed-entity package and its exact header.
    pub fn compressed_entity(
        &self,
        port: u16,
        request: PointReadRequestV1,
    ) -> Result<CompressedEntityAtHeader> {
        let result = eth::raw_json_with_params(
            &self.url(port),
            "outbe_getCompressedEntity",
            serde_json::json!([request]),
        )
        .ok_or_else(|| eyre!("outbe_getCompressedEntity returned no result on port {port}"))?;
        let result: PointReadResultV1 =
            serde_json::from_value(result).wrap_err("decode compressed-entity package")?;
        let common = match &result {
            PointReadResultV1::Present { common, .. }
            | PointReadResultV1::Absent { common, .. } => common,
            PointReadResultV1::Unavailable => {
                return Err(eyre!(
                    "compressed-entity package is unavailable on port {port}"
                ));
            }
        };
        let block = eth::raw_json_with_params(
            &self.url(port),
            "eth_getBlockByHash",
            serde_json::json!([common.block_hash, false]),
        )
        .ok_or_else(|| eyre!("selected block {} is unavailable", common.block_hash))?;
        let returned_hash = block
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("selected block has no hash"))?;
        if !returned_hash.eq_ignore_ascii_case(&common.block_hash.to_string()) {
            return Err(eyre!("selected block hash does not match proof package"));
        }
        let returned_number = block
            .get("number")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("selected block has no canonical number"))?;
        if returned_number != common.block_number {
            return Err(eyre!("selected block number does not match proof package"));
        }
        let extra_data: Bytes = serde_json::from_value(
            block
                .get("extraData")
                .cloned()
                .ok_or_else(|| eyre!("selected block has no extraData"))?,
        )
        .wrap_err("decode selected block extraData")?;
        Ok(CompressedEntityAtHeader {
            header: SelectedHeaderV1 {
                block_number: common.block_number,
                block_hash: common.block_hash,
                extra_data: extra_data.to_vec(),
            },
            result,
        })
    }

    /// TEE registry `isBootstrapped()` on the primary node.
    pub fn is_bootstrapped(&self) -> bool {
        eth::read_call(
            &self.cfg.rpc0,
            addresses::TEE_ADDR,
            &ITeeRegistry::isBootstrappedCall {},
        )
        .unwrap_or(false)
    }

    /// Active protocol version (`IUpdate.getActiveVersion`).
    pub fn active_version(&self) -> Option<u64> {
        eth::read_call(
            &self.cfg.rpc0,
            addresses::UPDATE_ADDR,
            &IUpdate::getActiveVersionCall {},
        )
        .map(|v| v as u64)
    }

    /// Scheduled update tuple for `id` (`IUpdate.getScheduledUpdate`).
    pub fn scheduled_update(&self, id: u64) -> Option<ScheduledUpdate> {
        let r = eth::read_call(
            &self.cfg.rpc0,
            addresses::UPDATE_ADDR,
            &IUpdate::getScheduledUpdateCall { id: U256::from(id) },
        )?;
        Some(ScheduledUpdate {
            version: r.version as u64,
            activation: r.activationHeight,
            status: r.status as u64,
        })
    }

    /// `IVote.listProposals` on the node at `port` (pagination probe).
    pub fn list_proposals_on(&self, port: u16, index: U256, count: U256) -> Option<Vec<U256>> {
        eth::read_call(
            &self.url(port),
            addresses::VOTE_ADDR,
            &IVote::listProposalsCall { index, count },
        )
    }

    /// `IVote.getProposalVoters` on the node at `port` (pagination probe).
    pub fn get_proposal_voters_on(
        &self,
        port: u16,
        proposal_id: u64,
        index: U256,
        count: U256,
    ) -> Option<Vec<Address>> {
        eth::read_call(
            &self.url(port),
            addresses::VOTE_ADDR,
            &IVote::getProposalVotersCall {
                proposalId: U256::from(proposal_id),
                index,
                count,
            },
        )
    }

    /// Parsed `outbe-cli vote status` for proposal `id`.
    pub fn vote_status(&self, id: u64) -> VoteStatus {
        let ids = id.to_string();
        let out = self
            .sh()
            .cli([
                "--rpc-url",
                self.cfg.rpc0.as_str(),
                "vote",
                "status",
                "--proposal-id",
                ids.as_str(),
            ])
            .unwrap_or_default();
        parse::parse_vote_status(&out, id)
    }

    // ---- sends (governance / tribute go through outbe-cli) --------------

    /// `outbe-cli vote propose --target-module <addr> --payload <json>` from an
    /// operator; returns the tx hash.
    pub fn send_propose(
        &self,
        operator: &Operator,
        target_module: &str,
        payload: &str,
    ) -> Result<String> {
        let key = operator.evm_key()?;
        let out = self.sh().cli([
            "--private-key",
            key.as_str(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "vote",
            "propose",
            "--target-module",
            target_module,
            "--payload",
            payload,
        ])?;
        parse::extract_tx_hash(&out).ok_or_else(|| eyre!("no tx hash in propose output:\n{out}"))
    }

    /// `outbe-cli vote cast --proposal-id <id> --yes|--no`; returns the tx hash.
    pub fn cast_vote(&self, validator: &Validator, id: u64, approve: bool) -> Result<String> {
        let key = validator.evm_key()?;
        let ids = id.to_string();
        let flag = if approve { "--yes" } else { "--no" };
        let out = self.sh().cli([
            "--private-key",
            key.as_str(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "vote",
            "cast",
            "--proposal-id",
            ids.as_str(),
            flag,
        ])?;
        parse::extract_tx_hash(&out).ok_or_else(|| eyre!("no tx hash in vote output:\n{out}"))
    }

    // ---- waits (poll loops) --------------------------------------------

    /// Wait until head on `port` reaches at least `min`; returns the last head seen.
    pub fn wait_block(&self, port: u16, min: u64, tries: u32) -> Option<u64> {
        for _ in 0..tries {
            if let Some(h) = self.head(port) {
                if h >= min {
                    return Some(h);
                }
            }
            sleep(Duration::from_secs(3));
        }
        self.head(port)
    }

    /// Wait until head on `port` is strictly greater than `height`.
    pub fn wait_block_gt(&self, port: u16, height: u64, tries: u32) -> Option<u64> {
        for _ in 0..tries {
            if let Some(h) = self.head(port) {
                if h > height {
                    return Some(h);
                }
            }
            sleep(Duration::from_secs(3));
        }
        self.head(port)
    }

    /// Wait for the primary node's TEE bootstrap (5s polls).
    pub fn wait_bootstrapped(&self, tries: u32) -> bool {
        for _ in 0..tries {
            if self.is_bootstrapped() {
                return true;
            }
            sleep(Duration::from_secs(5));
        }
        false
    }

    /// Wait for a tx receipt; `true` on success, `false` on revert/timeout.
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

    /// Wait until proposal `id` reports `status=want`.
    pub fn wait_vote_status(&self, id: u64, want: &str, tries: u32) -> bool {
        for _ in 0..tries {
            if self.vote_status(id).status == want {
                return true;
            }
            sleep(Duration::from_secs(3));
        }
        false
    }

    /// Wait until the active protocol version equals `want`.
    pub fn wait_active_version(&self, want: u64, tries: u32) -> Option<u64> {
        for _ in 0..tries {
            if let Some(v) = self.active_version() {
                if v == want {
                    return Some(v);
                }
            }
            sleep(Duration::from_secs(3));
        }
        self.active_version()
    }

    // ---- validator lifecycle reads (ValidatorSet / tribute / metadosis) ------

    /// The full `validatorByAddress` record, or `None` if unreadable.
    fn validator_record(
        &self,
        port: u16,
        addr: &str,
    ) -> Option<IValidatorSet::validatorByAddressReturn> {
        let v: Address = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::validatorByAddressCall { v },
        )
    }

    /// Status code: 0 REGISTERED, 1 PENDING, 2 ACTIVE, 3 EXITING, 4 UNBONDING, 5 INACTIVE.
    pub fn validator_status(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr).map(|r| r.status as u64)
    }

    /// Felony slash counter.
    pub fn slash_count(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr).map(|r| r.slashCount)
    }

    /// Whether the validator holds a live DKG share.
    pub fn has_share(&self, port: u16, addr: &str) -> Option<bool> {
        self.validator_record(port, addr).map(|r| r.hasShare)
    }

    /// Whether `addr` is a current consensus participant (ACTIVE or EXITING-with-share).
    pub fn is_participant(&self, port: u16, addr: &str) -> bool {
        let Ok(v) = addr.parse::<Address>() else {
            return false;
        };
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::isConsensusParticipantCall { v },
        )
        .unwrap_or(false)
    }

    /// Number of ACTIVE validators.
    pub fn active_count(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::activeValidatorCountCall {},
        )
        .map(|v| v as u64)
    }

    /// Consensus set size (ACTIVE + EXITING-with-share).
    pub fn consensus_count(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::activeConsensusCountCall {},
        )
        .map(|v| v as u64)
    }

    /// Tribute total supply on the node at `port` (decimal, for parity checks).
    pub fn supply(&self, port: u16) -> Option<String> {
        eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::totalSupplyCall {},
        )
        .map(|v| v.to_string())
    }

    /// Metadosis worldwide-day status byte (field 2 of `getWorldwideDay`).
    pub fn wwd_status(&self, port: u16, wwd: &str) -> Option<String> {
        let day: u32 = wwd.parse().ok()?;
        let r = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IWorldwideDay::getWorldwideDayCall { day },
        )?;
        Some(r.f1.to_string())
    }

    /// A JSON field from `outbe_consensusStatus` on the node at `port`.
    pub fn consensus_status_field(&self, port: u16, field: &str) -> Option<String> {
        let v = eth::raw_json(&self.url(port), "outbe_consensusStatus")?;
        match v.get(field)? {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    // ---- identity + sends ----------------------------------------------------

    /// EOA address for a private key (`0x`-hex).
    pub fn address_of(&self, key: &str) -> Option<String> {
        eth::address_of(key).map(|a| format!("{a:#x}"))
    }

    /// Submit a tribute offer for worldwide-day `wwd` from `key`; returns tx hash if any.
    pub fn tribute_offer(&self, key: &str, wwd: &str) -> Option<String> {
        let out = self
            .sh()
            .cli([
                "--private-key",
                key,
                "--rpc-url",
                self.cfg.rpc0.as_str(),
                "tribute",
                "offer",
                wwd,
                "--amount",
                "100",
                "--currency",
                "840",
            ])
            .ok()?;
        parse::extract_tx_hash(&out)
    }

    /// Wait until the submitted transaction is mined and assert its receipt succeeded.
    pub fn wait_successful_receipt(&self, tx_hash: &str, tries: u32) -> bool {
        self.wait_receipt_status(tx_hash, true, tries)
    }

    /// Wait until a transaction receipt exists with the expected success bit.
    pub fn wait_receipt_status(&self, tx_hash: &str, expected: bool, tries: u32) -> bool {
        for _ in 0..tries {
            match eth::receipt_success(&self.cfg.rpc0, tx_hash) {
                Some(status) => return status == expected,
                None => sleep(Duration::from_millis(500)),
            }
        }
        false
    }

    /// Stake `amount` ether from `key` (REGISTERED/PENDING joiner).
    pub fn stake(&self, key: &str, amount: u64) -> Result<()> {
        let v = eth::address_of(key).ok_or_else(|| eyre!("cannot derive address for stake"))?;
        let wei = eth::ether(amount);
        eth::send_call(
            &self.cfg.rpc0,
            addresses::STK_ADDR,
            key,
            &IStaking::stakeCall { v, amount: wei },
            Some(wei),
        )?;
        Ok(())
    }

    /// Confirm a PENDING joiner is synced/ready (stale-join guard).
    pub fn confirm_ready(&self, key: &str) -> Result<String> {
        self.sh().cli([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "validator",
            "confirm-ready",
        ])
    }

    /// Self-deactivate the validator owning `key` (ACTIVE -> EXITING).
    pub fn deactivate(&self, key: &str) -> Result<()> {
        let v =
            eth::address_of(key).ok_or_else(|| eyre!("cannot derive address for deactivate"))?;
        eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            key,
            &IValidatorSet::deactivateValidatorCall { v },
            None,
        )?;
        Ok(())
    }

    /// Felony slash percent from `outbe-cli slash config`, if readable.
    pub fn slash_percent(&self) -> Option<u64> {
        let out = self
            .sh()
            .cli(["--rpc-url", self.cfg.rpc0.as_str(), "slash", "config"])
            .ok()?;
        let line = out
            .lines()
            .find(|l| l.to_lowercase().contains("slash amount"))?;
        let digits: String = line.chars().filter(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    // ---- lifecycle waits -----------------------------------------------------

    /// Poll until `addr` is a consensus participant (10s polls, like the shell loops).
    pub fn wait_participant(&self, port: u16, addr: &str, tries: u32) -> bool {
        for _ in 0..tries {
            if self.is_participant(port, addr) {
                return true;
            }
            sleep(Duration::from_secs(10));
        }
        false
    }

    /// Poll until ACTIVE validator count equals `want` (10s polls).
    pub fn wait_active_count(&self, port: u16, want: u64, tries: u32) -> bool {
        for _ in 0..tries {
            if self.active_count(port) == Some(want) {
                return true;
            }
            sleep(Duration::from_secs(10));
        }
        false
    }

    /// Retry a tribute offer until `supply(primary)` reaches `want` (6s polls).
    pub fn offer_until_supply(
        &self,
        key: &str,
        wwd: &str,
        primary: u16,
        want: &str,
        tries: u32,
    ) -> bool {
        for _ in 0..tries {
            let _ = self.tribute_offer(key, wwd);
            sleep(Duration::from_secs(6));
            if self.supply(primary).as_deref() == Some(want) {
                return true;
            }
        }
        self.supply(primary).as_deref() == Some(want)
    }
}
