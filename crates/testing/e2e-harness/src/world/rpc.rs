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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, Bytes, U256};
use eyre::{eyre, Result, WrapErr as _};
use outbe_compressed_entities::{PointReadRequestV1, PointReadResultV1, SelectedHeaderV1};

use crate::internal::{
    addresses,
    config::Config,
    eth::{
        self, IGovernance, IStaking, ITeeRegistry, ITribute, IUpdate, IValidatorSet, IVote,
        IWorldwideDay, IZeroFee,
    },
    parse::{self, ScheduledUpdate, VoteStatus},
    shell::Sh,
};
use crate::world::state::FixtureState;
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

    /// `stateRoot` of block `height` on the node at `port`.
    pub fn state_root(&self, port: u16, height: u64) -> Option<String> {
        eth::state_root(&self.url(port), height)
    }

    /// Canonical block hash at `height` on the node at `port`.
    pub fn block_hash(&self, port: u16, height: u64) -> Option<String> {
        eth::block_hash(&self.url(port), height)
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
        self.active_version_on_url(&self.cfg.rpc0)
    }

    /// Active protocol version on the node at `port`.
    pub fn active_version_on(&self, port: u16) -> Option<u64> {
        self.active_version_on_url(&self.url(port))
    }

    fn active_version_on_url(&self, rpc_url: &str) -> Option<u64> {
        eth::read_call(
            rpc_url,
            addresses::UPDATE_ADDR,
            &IUpdate::getActiveVersionCall {},
        )
        .map(|v| v as u64)
    }

    /// Scheduled update tuple for `id` (`IUpdate.getScheduledUpdate`).
    pub fn scheduled_update(&self, id: u64) -> Option<ScheduledUpdate> {
        self.scheduled_update_on_url(&self.cfg.rpc0, id)
    }

    /// Scheduled update tuple for `id` on the node at `port`.
    pub fn scheduled_update_on(&self, port: u16, id: u64) -> Option<ScheduledUpdate> {
        self.scheduled_update_on_url(&self.url(port), id)
    }

    fn scheduled_update_on_url(&self, rpc_url: &str, id: u64) -> Option<ScheduledUpdate> {
        let r = eth::read_call(
            rpc_url,
            addresses::UPDATE_ADDR,
            &IUpdate::getScheduledUpdateCall { id: U256::from(id) },
        )?;
        Some(ScheduledUpdate {
            version: r.version as u64,
            activation: r.activationHeight,
            status: r.status as u64,
        })
    }

    /// OIP record (`IGovernance.getOip`) — `(status, author, text)`.
    pub fn get_oip(&self, id: u64) -> Option<(u8, Address, String)> {
        let r = eth::read_call(
            &self.cfg.rpc0,
            addresses::GOVERNANCE_ADDR,
            &IGovernance::getOipCall { id: U256::from(id) },
        )?;
        Some((r.status, r.author, r.text))
    }

    /// GIP record (`IGovernance.getGip`) — `(status, author, text)`.
    pub fn get_gip(&self, id: u64) -> Option<(u8, Address, String)> {
        let r = eth::read_call(
            &self.cfg.rpc0,
            addresses::GOVERNANCE_ADDR,
            &IGovernance::getGipCall { id: U256::from(id) },
        )?;
        Some((r.status, r.author, r.text))
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
        self.vote_status_on_url(&self.cfg.rpc0, id)
    }

    /// Parsed `outbe-cli vote status` from the node at `port`.
    pub fn vote_status_on(&self, port: u16, id: u64) -> VoteStatus {
        self.vote_status_on_url(&self.url(port), id)
    }

    fn vote_status_on_url(&self, rpc_url: &str, id: u64) -> VoteStatus {
        let ids = id.to_string();
        let out = self
            .sh()
            .cli([
                "--rpc-url",
                rpc_url,
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

    /// Submit a proposal that must fail during CLI/RPC preflight.
    pub fn send_propose_rejection(
        &self,
        key: &str,
        target_module: &str,
        payload: &str,
    ) -> Result<String> {
        self.sh().cli_expected_failure([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "vote",
            "propose",
            "--target-module",
            target_module,
            "--payload",
            payload,
        ])
    }

    fn proposal_event_blocks(
        &self,
        port: u16,
        address: Address,
        signature: &str,
        proposal_id: u64,
    ) -> Vec<u64> {
        let signature = keccak256(signature.as_bytes());
        let indexed_id = format!("0x{proposal_id:064x}");
        eth::raw_json_with_params(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{address:#x}"),
                "fromBlock": "0x0",
                "toBlock": "finalized",
                "topics": [format!("{signature:#x}"), indexed_id],
            }]),
        )
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|log| {
            log.get("blockNumber")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        })
        .collect()
    }

    pub fn proposal_approved_event_blocks(&self, port: u16, proposal_id: u64) -> Vec<u64> {
        self.proposal_event_blocks(
            port,
            addresses::VOTE_ADDR,
            "ProposalApproved(uint256,(uint64,uint64))",
            proposal_id,
        )
    }

    pub fn scheduled_update_created_event_blocks(&self, port: u16, proposal_id: u64) -> Vec<u64> {
        self.proposal_event_blocks(
            port,
            addresses::UPDATE_ADDR,
            "ScheduledUpdateCreated(uint256,uint32,uint64,bytes)",
            proposal_id,
        )
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

    /// Submit a ballot that must be rejected during RPC preflight, returning
    /// the product CLI/RPC error text for a precise assertion.
    pub fn cast_vote_rejection(
        &self,
        validator: &Validator,
        id: u64,
        approve: bool,
    ) -> Result<String> {
        let key = validator.evm_key()?;
        let ids = id.to_string();
        let flag = if approve { "--yes" } else { "--no" };
        self.sh().cli_expected_failure([
            "--private-key",
            key.as_str(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "vote",
            "cast",
            "--proposal-id",
            ids.as_str(),
            flag,
        ])
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
        self.wait_active_version_on(self.cfg.primary_port(), want, tries)
    }

    /// Wait until one validator reports the requested active protocol version.
    pub fn wait_active_version_on(&self, port: u16, want: u64, tries: u32) -> Option<u64> {
        for _ in 0..tries {
            if let Some(v) = self.active_version_on(port) {
                if v == want {
                    return Some(v);
                }
            }
            sleep(Duration::from_secs(3));
        }
        self.active_version_on(port)
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

    /// Status code: 0 REGISTERED, 1 PENDING, 2 ACTIVE, 3 EXITING,
    /// 4 UNBONDING, 5 INACTIVE, 6 JAILED.
    pub fn validator_status(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr).map(|r| r.status as u64)
    }

    /// Felony slash counter.
    pub fn slash_count(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr).map(|r| r.slashCount)
    }

    /// Bonded stake recorded by the Staking precompile on a specific node.
    pub fn stake_on(&self, port: u16, addr: &str) -> Option<U256> {
        let validator = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::STK_ADDR,
            &IStaking::getStakeCall { validator },
        )
    }

    /// Network-wide bonded total recorded by the Staking precompile.
    pub fn total_staked_on(&self, port: u16) -> Option<U256> {
        eth::read_call(
            &self.url(port),
            addresses::STK_ADDR,
            &IStaking::getTotalStakedCall {},
        )
    }

    /// Native balance on a specific node, including precompile balances.
    pub fn balance_on(&self, port: u16, addr: &str) -> Option<U256> {
        eth::balance(&self.url(port), addr.parse().ok()?)
    }

    pub fn staking_balance_on(&self, port: u16) -> Option<U256> {
        eth::balance(&self.url(port), addresses::STK_ADDR)
    }

    /// Whether a finalized VoterFelony event exists for `validator` at or after
    /// `from_block`. The validator is the event's first indexed argument.
    pub fn has_voter_felony_event(&self, port: u16, validator: &str, from_block: u64) -> bool {
        let validator: Address = match validator.parse() {
            Ok(value) => value,
            Err(_) => return false,
        };
        let signature = keccak256("VoterFelony(address,uint64,uint64)");
        let indexed_validator = format!("0x{:0>64}", hex::encode(validator));
        eth::raw_json_with_params(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::SLASH_ADDR),
                "fromBlock": format!("0x{from_block:x}"),
                "toBlock": "finalized",
                "topics": [format!("{signature:#x}"), indexed_validator],
            }]),
        )
        .and_then(|value| value.as_array().map(|logs| !logs.is_empty()))
        .unwrap_or(false)
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

    /// Current ValidatorSet epoch on a specific node.
    pub fn epoch_on(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getEpochNumberCall {},
        )
        .and_then(|value| u64::try_from(value).ok())
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

    /// Canonical Tribute identities indexed by one owner.
    pub fn tributes_by_owner(&self, port: u16, owner: Address) -> Option<Vec<Bytes>> {
        eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getTributesByOwnerCall { owner },
        )
    }

    /// Canonical Tribute identities indexed by one Worldwide Day.
    pub fn tributes_by_day(&self, port: u16, worldwide_day: u32) -> Option<Vec<Bytes>> {
        eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getTributesByDayCall {
                worldwideDay: worldwide_day,
            },
        )
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
        self.tribute_offer_with_params(key, wwd, "100", 840, false)
    }

    /// Submit a Tribute offer with explicit business fields. This is used by
    /// duplicate-identity tests to prove that `(owner, worldwide_day)`, rather
    /// than the rest of the encrypted payload, is the uniqueness boundary.
    pub fn tribute_offer_with_params(
        &self,
        key: &str,
        wwd: &str,
        amount: &str,
        currency: u16,
        exclude_from_intex_issuance: bool,
    ) -> Option<String> {
        let started = Instant::now();
        let mut args = vec![
            "--private-key".to_owned(),
            key.to_owned(),
            "--rpc-url".to_owned(),
            self.cfg.rpc0.clone(),
            "tribute".to_owned(),
            "offer".to_owned(),
            wwd.to_owned(),
            "--amount".to_owned(),
            amount.to_owned(),
            "--currency".to_owned(),
            currency.to_string(),
        ];
        if exclude_from_intex_issuance {
            args.push("--exclude-from-intex-issuance".to_owned());
        }
        let out = self.sh().cli(args.iter().map(String::as_str)).ok()?;
        let tx_hash = parse::extract_tx_hash(&out)?;
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=submitted wall_ms={} cli_elapsed_ms={} tx={tx_hash} owner={} wwd={wwd} amount={amount} currency={currency} exclude={exclude_from_intex_issuance}",
            unix_time_millis(),
            started.elapsed().as_millis(),
            self.address_of(key).unwrap_or_else(|| "unknown".to_owned()),
        );
        Some(tx_hash)
    }

    /// Wait until the submitted transaction is mined and assert its receipt succeeded.
    pub fn wait_successful_receipt(&self, tx_hash: &str, tries: u32) -> bool {
        self.wait_receipt_status(tx_hash, true, tries)
    }

    /// Wait until a transaction receipt exists with the expected success bit.
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

    /// Emit a state/finality observation correlated with one Tribute receipt.
    pub fn trace_tribute_state(&self, tx_hash: &str, stage: &str, port: u16) {
        let receipt = eth::receipt_json(&self.url(port), tx_hash);
        let receipt_block = receipt
            .as_ref()
            .and_then(|value| value.get("blockNumber"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage={stage} wall_ms={} tx={tx_hash} receipt_block={receipt_block} supply={:?} head={:?} finalized={:?}",
            unix_time_millis(),
            self.supply(port),
            self.head(port),
            self.finalized(port),
        );
    }

    /// Canonical block number carried by a mined public receipt.
    pub fn receipt_block_number(&self, tx_hash: &str, port: u16) -> Option<u64> {
        let receipt = eth::receipt_json(&self.url(port), tx_hash)?;
        let encoded = receipt.get("blockNumber")?.as_str()?;
        u64::from_str_radix(encoded.trim_start_matches("0x"), 16).ok()
    }

    /// Stake `amount` whole COEN from `key` (REGISTERED/PENDING joiner).
    pub fn stake(&self, key: &str, amount: u64) -> Result<String> {
        let v = eth::address_of(key).ok_or_else(|| eyre!("cannot derive address for stake"))?;
        let base_units = eth::coen(amount);
        let tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::STK_ADDR,
            key,
            &IStaking::stakeCall {
                v,
                amount: base_units,
            },
            Some(base_units),
        )?;
        if !self.wait_successful_receipt(&tx, 20) {
            return Err(eyre!("stake receipt was not successful: {tx}"));
        }
        Ok(tx)
    }

    /// Confirm a PENDING joiner is synced/ready (stale-join guard).
    pub fn confirm_ready(&self, key: &str) -> Result<String> {
        let out = self.sh().cli([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "validator",
            "confirm-ready",
        ])?;
        let tx_hash = parse::extract_tx_hash(&out)
            .ok_or_else(|| eyre!("no tx hash in confirm-ready output:\n{out}"))?;
        if !self.wait_successful_receipt(&tx_hash, 60) {
            return Err(eyre!(
                "confirm-ready transaction was not successfully included: {tx_hash}"
            ));
        }
        Ok(tx_hash)
    }

    /// Self-deactivate the validator owning `key` (ACTIVE -> EXITING).
    pub fn deactivate(&self, key: &str) -> Result<String> {
        let v =
            eth::address_of(key).ok_or_else(|| eyre!("cannot derive address for deactivate"))?;
        let tx = self.deactivate_as(key, v)?;
        let receipt = eth::receipt_json(&self.cfg.rpc0, &tx)
            .ok_or_else(|| eyre!("deactivate receipt unavailable: {tx}"))?;
        let topic = format!(
            "{:#x}",
            alloy_primitives::keccak256("ValidatorDeactivated(address,uint64)")
        );
        if !receipt_has_log(&receipt, addresses::VS_ADDR, Some(&topic)) {
            return Err(eyre!(
                "deactivate receipt has no ValidatorDeactivated event: {tx}"
            ));
        }
        Ok(tx)
    }

    /// Attempt to deactivate `validator` using the EOA in `caller_key`.
    pub fn deactivate_as(&self, caller_key: &str, validator: Address) -> Result<String> {
        let tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            caller_key,
            &IValidatorSet::deactivateValidatorCall { v: validator },
            None,
        )?;
        if !self.wait_successful_receipt(&tx, 20) {
            return Err(eyre!("deactivate receipt was not successful: {tx}"));
        }
        Ok(tx)
    }

    /// Claim the caller's matured queue and return the public receipt JSON.
    pub fn claim_unbonded(&self, key: &str) -> Result<serde_json::Value> {
        let tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::STK_ADDR,
            key,
            &IStaking::claimUnbondedCall {},
            None,
        )?;
        let receipt = eth::receipt_json(&self.cfg.rpc0, &tx)
            .ok_or_else(|| eyre!("claim receipt unavailable: {tx}"))?;
        if !receipt_status(&receipt) {
            return Err(eyre!("claim receipt was not successful: {tx}"));
        }
        Ok(receipt)
    }

    /// Exact native fee charged for a public receipt.
    pub fn receipt_gas_cost(receipt: &serde_json::Value) -> Option<U256> {
        let gas_used = receipt.get("gasUsed")?.as_str()?;
        let gas_price = receipt.get("effectiveGasPrice")?.as_str()?;
        Some(parse_rpc_u256(gas_used)? * parse_rpc_u256(gas_price)?)
    }

    /// Felony slash percent from the node's authoritative typed RPC response.
    pub fn slash_percent(&self) -> Option<u64> {
        eth::raw_json_with_params(
            &self.cfg.rpc0,
            "outbe_getSlashConfig",
            serde_json::json!([]),
        )?
        .get("slashAmountPercent")?
        .as_u64()
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

    /// Poll until finalized height reaches `want` on `port`.
    pub fn wait_finalized_at_least(&self, port: u16, want: u64, tries: u32) -> bool {
        for _ in 0..tries {
            if self.finalized(port).is_some_and(|height| height >= want) {
                return true;
            }
            sleep(Duration::from_secs(2));
        }
        self.finalized(port).is_some_and(|height| height >= want)
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
        self.offer_until_supply_hash(key, wwd, primary, want, tries)
            .is_some()
    }

    /// Retry one Tribute offer until `supply(primary)` reaches `want`, returning
    /// the included transaction hash for projection/index verification.
    pub fn offer_until_supply_hash(
        &self,
        key: &str,
        wwd: &str,
        primary: u16,
        want: &str,
        tries: u32,
    ) -> Option<String> {
        let mut pending_tx = None;
        for _ in 0..tries {
            if pending_tx.is_none() {
                pending_tx = self.tribute_offer(key, wwd);
            }
            sleep(Duration::from_secs(6));
            if self.supply(primary).as_deref() == Some(want) {
                if let Some(tx_hash) = pending_tx.as_deref() {
                    self.trace_tribute_state(tx_hash, "state-visible", primary);
                }
                return pending_tx;
            }
            // Do not blindly submit a replacement while the first offer is still
            // pending. The CLI intentionally uses the account's pending nonce, so
            // an identical-fee retry is rejected as `replacement transaction
            // underpriced` and only adds noise to an otherwise healthy lifecycle
            // run. A failed receipt is terminal for that attempt and permits a
            // fresh logical offer; a pending or successful receipt is given the
            // remainder of the polling budget to become visible in state.
            if pending_tx
                .as_deref()
                .and_then(|hash| eth::receipt_success(&self.cfg.rpc0, hash))
                == Some(false)
            {
                pending_tx = None;
            }
        }
        (self.supply(primary).as_deref() == Some(want))
            .then_some(pending_tx)
            .flatten()
    }

    // ---- ZeroFee EIP-7702 vertical slice ----------------------------------

    pub fn assert_zerofee_readiness(&self) {
        let code = eth::code(&self.cfg.rpc0, addresses::ZEROFEE_ADDR).expect("read ZeroFee code");
        assert_eq!(code.as_ref(), &[0xef], "ZeroFee marker must be 0xef");
        assert_eq!(
            eth::storage(&self.cfg.rpc0, addresses::ZEROFEE_ADDR, U256::ZERO),
            Some(U256::from(1)),
            "ZeroFee schema slot 0 must be version 1"
        );
    }

    pub fn prepare_zerofee_account(
        &self,
        funder: &Operator,
        state: &mut FixtureState,
    ) -> Result<()> {
        // Deterministic non-validator fixture key. Each scenario owns a fresh
        // genesis/datadir, so reuse cannot leak nonce or quota between runs.
        let key = "0x1111111111111111111111111111111111111111111111111111111111111111";
        let address =
            eth::address_of(key).ok_or_else(|| eyre!("derive ZeroFee fixture address"))?;
        let funder_key = funder.evm_key()?;
        eth::send_value(&self.cfg.rpc0, address, &funder_key, eth::coen(1))?;

        let auth = eth::read_call(
            &self.cfg.rpc0,
            addresses::ZEROFEE_ADDR,
            &IZeroFee::authorizeSponsorshipCall { signer: address },
        )
        .ok_or_else(|| eyre!("read authorizeSponsorship"))?;
        if !auth {
            return Err(eyre!("fresh funded signer is not eligible for sponsorship"));
        }
        let counter = self
            .zerofee_counter(address)
            .ok_or_else(|| eyre!("read ZeroFee counter"))?;
        if counter.1 != 0 || counter.0 == 0 {
            return Err(eyre!("fresh counter must be (today, 0), got {counter:?}"));
        }

        state.zerofee_delegation_receipt = Some(eth::install_delegation(
            &self.cfg.rpc0,
            key,
            addresses::ZEROFEE_ADDR,
        )?);
        let delegation_hash = state
            .zerofee_delegation_receipt
            .as_ref()
            .and_then(|receipt| {
                receipt
                    .get("transactionHash")
                    .and_then(serde_json::Value::as_str)
            });
        state.zerofee_delegation_raw = delegation_hash.and_then(|hash| {
            eth::raw_json_with_params(
                &self.cfg.rpc0,
                "eth_getRawTransactionByHash",
                serde_json::json!([hash]),
            )
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
        });
        if state.zerofee_delegation_raw.is_none() {
            return Err(eyre!(
                "public RPC did not return the exact signed EIP-7702 transaction"
            ));
        }
        state.zerofee_key = Some(key.to_string());
        state.zerofee_address = Some(format!("{address:#x}"));
        state.zerofee_balance_before = eth::balance(&self.cfg.rpc0, address);
        Ok(())
    }

    pub fn assert_zerofee_delegation(&self, state: &FixtureState) {
        let address = zerofee_address(state);
        let code = eth::code(&self.cfg.rpc0, address).expect("read delegated account code");
        let expected = [&[0xef, 0x01, 0x00][..], addresses::ZEROFEE_ADDR.as_slice()].concat();
        assert_eq!(code.as_ref(), expected, "wrong EIP-7702 designator");
    }

    pub fn replay_zerofee_sponsored_transaction(&self, state: &mut FixtureState) -> Result<()> {
        let raw = state
            .zerofee_sponsored_raw
            .as_deref()
            .ok_or_else(|| eyre!("missing exact included sponsored transaction"))?;
        let before_balance = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        let before_counter = self.zerofee_counter(zerofee_address(state));
        let error = eth::raw_json_result(
            &self.cfg.rpc0,
            "eth_sendRawTransaction",
            serde_json::json!([raw]),
        )
        .expect_err("exact included EIP-7702 transaction replay unexpectedly accepted");
        state.zerofee_replay_error = Some(error.to_string());
        assert_eq!(
            eth::balance(&self.cfg.rpc0, zerofee_address(state)),
            before_balance,
            "replay changed signer balance"
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)),
            before_counter,
            "replay changed ZeroFee counter"
        );
        self.assert_zerofee_delegation(state);
        Ok(())
    }

    pub fn assert_zerofee_persisted_on_ports(&self, state: &FixtureState, ports: &[u16]) {
        let address = zerofee_address(state);
        let expected_code = [&[0xef, 0x01, 0x00][..], addresses::ZEROFEE_ADDR.as_slice()].concat();
        let expected_counter = self
            .zerofee_counter(address)
            .expect("primary ZeroFee counter");
        let expected_balance =
            eth::balance(&self.cfg.rpc0, address).expect("primary delegated-account COEN balance");
        assert_eq!(expected_counter.1, 8, "primary quota must remain exhausted");
        for &port in ports {
            let url = self.url(port);
            assert_eq!(
                eth::code(&url, address).map(|code| code.to_vec()),
                Some(expected_code.clone()),
                "delegation was not preserved on RPC port {port}"
            );
            let counter = eth::read_call(
                &url,
                addresses::ZEROFEE_ADDR,
                &IZeroFee::getCounterCall { signer: address },
            )
            .map(|value| (value.day, value.count));
            assert_eq!(
                counter,
                Some(expected_counter),
                "quota/day changed on RPC port {port}"
            );
            assert_eq!(
                eth::balance(&url, address),
                Some(expected_balance),
                "delegated-account COEN balance changed on RPC port {port}"
            );
        }
    }

    pub fn submit_zerofee_quota(&self, state: &mut FixtureState) -> Result<()> {
        let key = zerofee_key(state).to_string();
        for _ in 0..8 {
            let receipt =
                eth::send_reward_call(&self.cfg.rpc0, &key, addresses::AGENT_REWARD_ADDR, 0)?;
            if state.zerofee_sponsored_raw.is_none() {
                let tx_hash = receipt
                    .get("transactionHash")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| eyre!("sponsored receipt has no transactionHash: {receipt}"))?;
                state.zerofee_sponsored_raw = Some(
                    eth::raw_json_with_params(
                        &self.cfg.rpc0,
                        "eth_getRawTransactionByHash",
                        serde_json::json!([tx_hash]),
                    )
                    .and_then(|raw| raw.as_str().map(str::to_owned))
                    .ok_or_else(|| eyre!("included sponsored transaction has no raw encoding"))?,
                );
            }
            state.zerofee_sponsored_receipts.push(receipt);
        }
        state.zerofee_balance_after_quota = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        Ok(())
    }

    pub fn assert_zerofee_quota(&self, state: &FixtureState) {
        assert_eq!(state.zerofee_sponsored_receipts.len(), 8);
        for (index, receipt) in state.zerofee_sponsored_receipts.iter().enumerate() {
            assert!(
                receipt_status(receipt),
                "sponsored receipt #{} failed",
                index + 1
            );
            assert!(
                receipt_has_log(receipt, addresses::ZEROFEE_ADDR, Some(SPONSORSHIP_TOPIC)),
                "sponsored receipt #{} has no authorization event",
                index + 1
            );
        }
        assert_eq!(
            state.zerofee_balance_after_quota, state.zerofee_balance_before,
            "sponsored calls charged the signer"
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)).map(|v| v.1),
            Some(8)
        );
    }

    pub fn submit_zerofee_ninth(&self, state: &mut FixtureState) -> Result<()> {
        let before = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        state.zerofee_balance_after_quota = before;
        state.zerofee_ninth_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_balance_after_ninth = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        Ok(())
    }

    pub fn assert_zerofee_ninth(&self, state: &FixtureState) {
        let receipt = state.zerofee_ninth_receipt.as_ref().expect("ninth receipt");
        assert!(
            !receipt_status(receipt),
            "ninth sponsored call unexpectedly succeeded"
        );
        assert!(
            receipt_has_failure_code(receipt, 110),
            "ninth receipt has no OutbeFailure(110)"
        );
        assert_eq!(
            state.zerofee_balance_after_ninth,
            state.zerofee_balance_after_quota
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)).map(|v| v.1),
            Some(8)
        );
    }

    pub fn submit_zerofee_paid(&self, state: &mut FixtureState) -> Result<()> {
        state.zerofee_balance_after_ninth = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        state.zerofee_paid_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            1_000_000_000,
        )?);
        state.zerofee_balance_after_paid = eth::balance(&self.cfg.rpc0, zerofee_address(state));
        Ok(())
    }

    pub fn assert_zerofee_paid(&self, state: &FixtureState) {
        let receipt = state.zerofee_paid_receipt.as_ref().expect("paid receipt");
        assert!(receipt_status(receipt), "paid fallback failed");
        assert!(
            state.zerofee_balance_after_paid < state.zerofee_balance_after_ninth,
            "paid fallback did not charge a fee"
        );
        assert!(!receipt_has_log(
            receipt,
            addresses::ZEROFEE_ADDR,
            Some(SPONSORSHIP_TOPIC)
        ));
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)).map(|v| v.1),
            Some(8)
        );
    }

    pub fn assert_zerofee_cli_authorization(&self, state: &FixtureState) {
        let output = self
            .sh()
            .cli([
                "--private-key",
                zerofee_key(state),
                "--rpc-url",
                self.cfg.rpc0.as_str(),
                "zero-fee",
                "eip7702-authorize",
            ])
            .expect("run product CLI authorization");
        let json: serde_json::Value = serde_json::from_str(&output).expect("authorization JSON");
        assert_eq!(
            json["address"].as_str().map(str::to_ascii_lowercase),
            Some(format!("{:#x}", addresses::ZEROFEE_ADDR))
        );
        let chain = eth::raw_json(&self.cfg.rpc0, "eth_chainId")
            .and_then(|value| {
                value
                    .as_str()
                    .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            })
            .expect("RPC chain id");
        let cli_chain = json["chainId"]
            .as_str()
            .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok());
        assert_eq!(cli_chain, Some(chain));
    }

    pub fn submit_zerofee_invalid_authorization(
        &self,
        funder: &Validator,
        state: &mut FixtureState,
    ) -> Result<()> {
        let key = "0x2222222222222222222222222222222222222222222222222222222222222222";
        let address = eth::address_of(key).ok_or_else(|| eyre!("derive negative signer"))?;
        let funding = self.fund_key(funder, key, 1)?;
        if !self.wait_successful_receipt(&funding, 20) {
            return Err(eyre!("negative signer COEN funding failed: {funding}"));
        }
        let chain_id = self
            .chain_id(self.cfg.primary_port())
            .ok_or_else(|| eyre!("chain id"))?;
        state.zerofee_invalid_authorization_receipt = Some(eth::install_delegation_with_overrides(
            &self.cfg.rpc0,
            key,
            addresses::ZEROFEE_ADDR,
            Some(U256::from(chain_id.saturating_add(1))),
            None,
        )?);
        state.zerofee_negative_key = Some(key.to_string());
        state.zerofee_negative_address = Some(format!("{address:#x}"));
        Ok(())
    }

    pub fn assert_zerofee_invalid_authorization(&self, state: &FixtureState) {
        let address = zerofee_negative_address(state);
        let receipt = state
            .zerofee_invalid_authorization_receipt
            .as_ref()
            .expect("invalid authorization receipt");
        assert!(
            receipt_status(receipt),
            "outer transaction carrying an invalid authorization must still be a valid included transaction"
        );
        assert_eq!(
            eth::code(&self.cfg.rpc0, address).map(|code| code.to_vec()),
            Some(Vec::new()),
            "wrong-chain authorization installed delegation code"
        );
        assert_eq!(self.zerofee_counter(address).map(|value| value.1), Some(0));
    }

    pub fn submit_zerofee_wrong_target(&self, state: &mut FixtureState) -> Result<()> {
        let key = zerofee_negative_key(state).to_string();
        let address = zerofee_negative_address(state);
        // Authorization-list processing installs the designator before the
        // outer call executes. Calling the newly delegated Update target with
        // empty calldata may revert; that receipt status is not the delegation
        // postcondition, so the live account code below is authoritative.
        let _delegation = eth::install_delegation(&self.cfg.rpc0, &key, addresses::UPDATE_ADDR)?;
        state.zerofee_wrong_target_balance_before = eth::balance(&self.cfg.rpc0, address);
        state.zerofee_wrong_target_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            &key,
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_wrong_target_balance_after = eth::balance(&self.cfg.rpc0, address);
        Ok(())
    }

    pub fn assert_zerofee_wrong_target(&self, state: &FixtureState) {
        let address = zerofee_negative_address(state);
        let expected = [&[0xef, 0x01, 0x00][..], addresses::UPDATE_ADDR.as_slice()].concat();
        assert_eq!(
            eth::code(&self.cfg.rpc0, address).map(|code| code.to_vec()),
            Some(expected),
            "wrong-target delegation designator changed unexpectedly"
        );
        let receipt = state
            .zerofee_wrong_target_receipt
            .as_ref()
            .expect("wrong-target call receipt");
        assert!(
            !receipt_has_log(receipt, addresses::ZEROFEE_ADDR, Some(SPONSORSHIP_TOPIC)),
            "wrong-target delegation received ZeroFee sponsorship"
        );
        assert!(
            state.zerofee_wrong_target_balance_after < state.zerofee_wrong_target_balance_before,
            "wrong-target call did not pay its own COEN gas charge"
        );
        assert_eq!(self.zerofee_counter(address).map(|value| value.1), Some(0));
    }

    pub fn submit_zerofee_conflicting_authorization(&self, state: &mut FixtureState) -> Result<()> {
        state.zerofee_conflicting_authorization_receipt =
            Some(eth::install_delegation_with_overrides(
                &self.cfg.rpc0,
                zerofee_negative_key(state),
                addresses::ZEROFEE_ADDR,
                None,
                Some(0),
            )?);
        Ok(())
    }

    pub fn assert_zerofee_conflicting_authorization(&self, state: &FixtureState) {
        let address = zerofee_negative_address(state);
        let receipt = state
            .zerofee_conflicting_authorization_receipt
            .as_ref()
            .expect("conflicting authorization receipt");
        assert!(
            !receipt_has_log(receipt, addresses::ZEROFEE_ADDR, Some(SPONSORSHIP_TOPIC)),
            "conflicting authorization unexpectedly emitted sponsorship"
        );
        let expected = [&[0xef, 0x01, 0x00][..], addresses::UPDATE_ADDR.as_slice()].concat();
        assert_eq!(
            eth::code(&self.cfg.rpc0, address).map(|code| code.to_vec()),
            Some(expected),
            "stale authorization replaced the existing delegation"
        );
        assert_eq!(self.zerofee_counter(address).map(|value| value.1), Some(0));
    }

    pub fn wait_zerofee_day_rollover_and_submit(&self, state: &mut FixtureState) -> Result<()> {
        let address = zerofee_address(state);
        let before = self
            .zerofee_counter(address)
            .ok_or_else(|| eyre!("read exhausted counter before day rollover"))?;
        if before.1 != 8 {
            return Err(eyre!(
                "day rollover requires exhausted quota, got {before:?}"
            ));
        }
        state.zerofee_day_before_rollover = Some(before.0);

        let mut reset = None;
        for _ in 0..150 {
            let latest_timestamp = self.latest_block_timestamp(self.cfg.primary_port());
            let current = self.zerofee_counter(address);
            if latest_timestamp.is_some_and(|timestamp| timestamp % 86_400 < 200)
                && current.is_some_and(|value| value.0 != before.0 && value.1 == 0)
            {
                reset = current;
                break;
            }
            sleep(Duration::from_secs(1));
        }
        let _reset = reset.ok_or_else(|| eyre!("ZeroFee counter did not lazily reset"))?;
        state.zerofee_new_day_balance_before = eth::balance(&self.cfg.rpc0, address);
        state.zerofee_new_day_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_new_day_balance_after = eth::balance(&self.cfg.rpc0, address);
        Ok(())
    }

    pub fn assert_zerofee_day_rollover(&self, state: &FixtureState, ports: &[u16]) {
        let address = zerofee_address(state);
        let old_day = state
            .zerofee_day_before_rollover
            .expect("day before rollover");
        let receipt = state
            .zerofee_new_day_receipt
            .as_ref()
            .expect("new-day receipt");
        assert!(
            receipt_status(receipt),
            "first new-day sponsored call failed: receipt={receipt}"
        );
        assert!(
            receipt_has_log(receipt, addresses::ZEROFEE_ADDR, Some(SPONSORSHIP_TOPIC)),
            "first new-day call has no sponsorship event"
        );
        assert_eq!(
            state.zerofee_new_day_balance_after, state.zerofee_new_day_balance_before,
            "first new-day sponsored call charged the signer COEN"
        );
        let expected = self
            .zerofee_counter(address)
            .expect("primary new-day counter");
        assert_ne!(expected.0, old_day, "worldwide day did not change");
        assert_eq!(expected.1, 1, "new-day quota must restart at one use");
        let expected_code = [&[0xef, 0x01, 0x00][..], addresses::ZEROFEE_ADDR.as_slice()].concat();
        for &port in ports {
            let url = self.url(port);
            assert_eq!(
                eth::read_call(
                    &url,
                    addresses::ZEROFEE_ADDR,
                    &IZeroFee::getCounterCall { signer: address },
                )
                .map(|value| (value.day, value.count)),
                Some(expected),
                "new-day quota differs on RPC port {port}"
            );
            assert_eq!(
                eth::code(&url, address).map(|code| code.to_vec()),
                Some(expected_code.clone()),
                "delegation changed across day rollover on RPC port {port}"
            );
        }
    }

    fn zerofee_counter(&self, signer: Address) -> Option<(u32, u32)> {
        let value = eth::read_call(
            &self.cfg.rpc0,
            addresses::ZEROFEE_ADDR,
            &IZeroFee::getCounterCall { signer },
        )?;
        Some((value.day, value.count))
    }
}

fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

const SPONSORSHIP_TOPIC: &str =
    "0x82fb9fccc7b9033227aa1f5b18f6140ac5a8216361e4e7496146c804bd6e8cc8";

fn zerofee_key(state: &FixtureState) -> &str {
    state.zerofee_key.as_deref().expect("ZeroFee fixture key")
}

fn zerofee_address(state: &FixtureState) -> Address {
    state
        .zerofee_address
        .as_deref()
        .expect("ZeroFee fixture address")
        .parse()
        .expect("valid ZeroFee fixture address")
}

fn zerofee_negative_key(state: &FixtureState) -> &str {
    state
        .zerofee_negative_key
        .as_deref()
        .expect("negative ZeroFee fixture key")
}

fn zerofee_negative_address(state: &FixtureState) -> Address {
    state
        .zerofee_negative_address
        .as_deref()
        .expect("negative ZeroFee fixture address")
        .parse()
        .expect("valid negative ZeroFee fixture address")
}

fn receipt_status(receipt: &serde_json::Value) -> bool {
    matches!(receipt.get("status"), Some(serde_json::Value::Bool(true)))
        || receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1")
}

fn parse_rpc_u256(value: &str) -> Option<U256> {
    U256::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

fn receipt_has_log(receipt: &serde_json::Value, address: Address, topic0: Option<&str>) -> bool {
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

fn receipt_has_failure_code(receipt: &serde_json::Value, code: u16) -> bool {
    receipt["logs"].as_array().is_some_and(|logs| {
        logs.iter().any(|log| {
            log["address"].as_str().is_some_and(|v| {
                v.eq_ignore_ascii_case(&format!("{:#x}", addresses::ZEROFEE_LOG_ADDR))
            }) && log["topics"][1].as_str().is_some_and(|topic| {
                u16::from_str_radix(topic.trim_start_matches("0x").get(60..).unwrap_or(""), 16)
                    == Ok(code)
            })
        })
    })
}
