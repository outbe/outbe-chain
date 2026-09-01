//! Chain-interaction handle: reads/sends natively via alloy ([`crate::internal::eth`]),
//! governance/tribute sends via `outbe-cli`, and the poll/wait loops that back the
//! scenarios.
//!
//! This is the typed replacement for the `cast`-based RPC readers and the
//! scenario polling helpers used by the lifecycle and update flows.
//! Reads return `Option` - `None` is the analogue of the shell
//! `2>/dev/null || echo dn`. Only governance (`vote`), tribute, `confirm-ready`,
//! and `slash config` still go through `outbe-cli` (the product CLI under test).

use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall as _};
use eyre::{eyre, Result, WrapErr as _};
#[cfg(feature = "ocomp-integration")]
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{PointReadRequestV1, PointReadResultV1, SelectedHeaderV1};
#[cfg(feature = "ocomp-integration")]
use outbe_nod::NodCertifiedGenerationProjection;
#[cfg(feature = "ocomp-integration")]
use outbe_nodfactory::certified_read::active_nod_set;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::{
    nod_materialization::NodMaterializationHeadV1,
    profile::poc_schema_limits,
    state::{ActiveGenerationV1, OcompJobRecordV1},
    vote::OcompVoteAccountabilityV1,
};
#[cfg(feature = "ocomp-integration")]
use outbe_ocompregistry::precompile::IOcompRegistry;
use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ocomp-integration")]
use crate::internal::eth::{IDesis, IGratis, INodFactory, IPromisLimit};
use crate::internal::{
    addresses,
    config::Config,
    eth::{
        self, IAgentReward, IGovernance, IL2Registry, IMetadosis, INod, IRadicleRegistry,
        ISlashIndicator, IStaking, ITeeRegistryV1, ITribute, IUpdate, IValidatorSet,
        IValidatorSetRaw, IVote, IZeroFee,
    },
    parse::{self, ScheduledUpdate, VoteStatus},
    shell::Sh,
};
use crate::ocomp_evidence::sha256_hex;
use crate::world::state::FixtureState;
use crate::world::validators::{Operator, Validator};

sol!("../../contracts/precompiles/src/IOracle.sol");
sol!("../../contracts/precompiles/src/ITributeFactory.sol");

#[derive(Debug, Clone)]
pub struct Rpc {
    cfg: Config,
}

pub struct TributeZkOffer<'a> {
    pub tribute_draft_id_hex: &'a str,
    pub su_hash_hex: &'a str,
    pub merkle_root_hex: &'a str,
    pub proof_hex: &'a str,
    pub signature_hex: &'a str,
}

fn zerofee_rollover_wait_budget_secs(latest_timestamp: u64) -> u64 {
    const SECONDS_PER_DAY: u64 = 86_400;
    const MINIMUM_WAIT_SECONDS: u64 = 150;
    const FINALITY_SLACK_SECONDS: u64 = 60;

    let remaining = SECONDS_PER_DAY - latest_timestamp % SECONDS_PER_DAY;
    remaining
        .saturating_add(FINALITY_SLACK_SECONDS)
        .max(MINIMUM_WAIT_SECONDS)
}

#[cfg(feature = "ocomp-integration")]
fn encode_reward_bearing_tribute_plaintext(
    creator: Address,
    tribute_draft_id: B256,
    amount_base: &str,
    amount_atto: &str,
    su_hash: B256,
    wallet_addresses: &[Address],
    sra_addresses: &[Address],
) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "creator": format!("{creator:#x}"),
        "tribute_draft_id": format!("{tribute_draft_id:#x}"),
        "amount_base": amount_base,
        "amount_atto": amount_atto,
        "su_hashes": [format!("{su_hash:#x}")],
        "wallet_addresses": wallet_addresses
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>(),
        "sra_addresses": sra_addresses
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>(),
    }))
    .map_err(Into::into)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedEntityAtHeader {
    pub result: PointReadResultV1,
    pub header: SelectedHeaderV1,
}

/// Consensus commitments observed for one canonical block on one validator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockCommitmentV1 {
    pub block_hash: B256,
    pub state_root: B256,
    pub ce_root: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStateV1 {
    pub status: u8,
    pub day_type: u8,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStartedV1 {
    pub worldwide_day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
    pub block_number: u64,
    pub block_hash: B256,
}

/// Latest canonical Oracle publication observed through a validator RPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OracleRateDataV1 {
    pub rate: U256,
    pub last_block: u64,
    pub last_timestamp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStatusChangeV1 {
    pub worldwide_day: u32,
    pub old_status: u8,
    pub new_status: u8,
    pub block_number: u64,
    pub block_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayTerminalReceiptV1 {
    pub outcome: u8,
    pub value_routed: U256,
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub retirement_outcome: u8,
    pub block_number: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicJobRequestV1 {
    pub intent_id: B256,
    pub worldwide_day: u32,
    pub pending_nonce: u64,
    pub attempt: u32,
    pub finality_recorded_height: u64,
    pub open_height: u64,
    pub deadline_height: u64,
    pub activation_preconditions_hash: B256,
    pub request_height: u64,
    pub request_block_hash: B256,
    pub transaction_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicActivationV1 {
    pub intent_id: B256,
    pub job_id: B256,
    pub activation_call_id: B256,
    pub result_digest: B256,
    pub terminal_receipt_hash: B256,
    pub worldwide_day: u32,
    pub block_number: u64,
    pub block_hash: B256,
    pub transaction_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NodMaterializationObservationV1 {
    pub worldwide_day: u32,
    pub generation: u64,
    pub nod_count: u32,
    pub next_nod_ordinal: u32,
    pub successful_batch_transactions: u32,
    pub completion_block_number: u64,
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodMaterializationProgressV1 {
    worldwide_day: u32,
    generation: u64,
    next_nod_ordinal: u32,
    completed: bool,
    block_number: u64,
}

#[cfg(feature = "ocomp-integration")]
struct MaterializationStallDeadline {
    last_progress: u32,
    deadline: Instant,
    stall: Duration,
}

#[cfg(feature = "ocomp-integration")]
impl MaterializationStallDeadline {
    fn new(now: Instant, stall: Duration) -> Self {
        Self {
            last_progress: 0,
            deadline: now + stall,
            stall,
        }
    }

    fn last_progress(&self) -> u32 {
        self.last_progress
    }

    fn observe(&mut self, now: Instant, progress: u32) -> bool {
        if progress > self.last_progress {
            self.last_progress = progress;
            self.deadline = now + self.stall;
        }
        now >= self.deadline
    }
}

/// One public `submitLysisResult(bytes)` transaction observed in a canonical
/// finalized block. The harness derives this only from public RPC block and
/// receipt data; Supervisor journals and chain storage are not test inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicResultVoteTransactionV1 {
    pub transaction_hash: B256,
    pub signer: Address,
    pub block_number: u64,
    pub block_hash: B256,
    pub calldata_len: usize,
    pub raw_transaction_len: usize,
    pub block_rlp_len: usize,
    pub gas_used: u64,
    pub success: bool,
}

/// Public projection of the bounded accountability object. Keeping this
/// evidence shape independent from the optional protocol crate lets ordinary
/// harness builds retain scenario state without enabling OCOMP integration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicVoteAccountabilityV1 {
    pub job_id: B256,
    pub result_validator_set_epoch: u64,
    pub result_committee_set_hash: B256,
    pub result_ocomp_binding_hash: B256,
    pub member_count: u16,
    pub quorum_threshold: u16,
    pub slot_validator_indexes: Vec<u16>,
    pub slot_first_signatures: Vec<(u16, Vec<u8>)>,
    pub quorum_result_digest: Option<B256>,
    pub quorum_height: Option<u64>,
    pub quorum_signer_bitmap: Option<Vec<u8>>,
    pub closed_height: Option<u64>,
    pub timely_bitmap: Option<Vec<u8>>,
    pub matching_bitmap: Option<Vec<u8>>,
    pub divergent_bitmap: Option<Vec<u8>>,
    pub missing_bitmap: Option<Vec<u8>>,
    pub equivocation_bitmap: Option<Vec<u8>>,
}

/// Finalized, cross-owner authority for one proof-backed Nod generation.
///
/// Both owner projections are read at `block_number`; Mongo/CAS never supplies
/// any field in this record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompCertifiedGenerationV1 {
    pub worldwide_day: u32,
    pub generation: u64,
    pub job_id: B256,
    pub program_semantics_hash: B256,
    pub nod_root: B256,
    pub bucket_root: B256,
    pub output_manifest_root: B256,
    pub tribute_count: u32,
    pub nod_count: u32,
    pub bucket_count: u32,
    pub nod_amount_total: U256,
    pub nod_gratis_consumed: U256,
    pub issued_at: u64,
    pub result_evidence_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
}

impl CompressedEntityAtHeader {
    /// Hash the exact JSON transport package and extract its authenticated CE root.
    pub fn evidence_identity(&self) -> Result<(String, String)> {
        let proof_sha256 = sha256_hex(&serde_json::to_vec(&self.result)?);
        let artifacts = decode_outbe_block_artifacts(&self.header.extra_data)
            .map_err(|error| eyre!("decode compressed-entity header artifacts: {error}"))?;
        let ce_root = artifacts
            .compressed_entities_root
            .ok_or_else(|| eyre!("compressed-entity header has no CE root"))?
            .r_sealed
            .to_string();
        Ok((ce_root, proof_sha256))
    }
}

/// Public ValidatorSet record returned by either address or dense index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorRecord {
    pub address: Address,
    pub consensus_pubkey: Bytes,
    pub stake: U256,
    pub status: u8,
    pub slash_count: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
    pub blocks_proposed: u64,
    pub joined_at_height: u64,
    pub deactivated_at_height: u64,
    pub unbonding_end: u64,
    pub has_bls_share: bool,
}

/// Versioned P2P address stored atomically by ValidatorSet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorP2pAddress {
    pub version: u8,
    pub encoded: Bytes,
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

    fn sh(&self) -> Sh<'_> {
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

    #[cfg(feature = "ocomp-integration")]
    pub fn assert_certified_nod_mining_blocked(
        &self,
        port: u16,
        private_key: &str,
        generation: &OcompCertifiedGenerationV1,
    ) -> Result<()> {
        let head = self
            .nod_materialization_head_on(port)
            .ok_or_else(|| eyre!("certified NOD materialization head is absent"))?;
        if head.worldwide_day != generation.worldwide_day
            || head.generation != generation.generation
            || head.next_nod_ordinal >= head.nod_count
        {
            return Err(eyre!(
                "certified materialization head does not match the incomplete generation"
            ));
        }
        let owner = self
            .address_of(private_key)
            .ok_or_else(|| eyre!("derive first capacity owner"))?
            .parse::<Address>()
            .wrap_err("parse first capacity owner")?;
        let nod_id = outbe_nod::NodContract::generate_nod_id(
            owner,
            WorldwideDay::new(generation.worldwide_day),
        )?;
        let call = INodFactory::mineGratisCall {
            nodId: nod_id.to_u256(),
            nonce: 0,
            mac: B256::ZERO,
            opNonce: 0,
            payNoteProof: Bytes::new(),
        };
        // This is an intentional negative transaction. Supplying an explicit
        // bounded gas limit prevents the RPC client from replacing the actual
        // status=0 receipt with an eth_estimateGas error.
        let transaction_hash = eth::send_calldata(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            private_key,
            call.abi_encode(),
            300_000,
        )?;
        if eth::receipt_success(&self.url(port), &transaction_hash) != Some(false) {
            return Err(eyre!(
                "mineGratis succeeded before materialization completion"
            ));
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn wait_for_completed_nod_materialization(
        &self,
        port: u16,
        generation: &OcompCertifiedGenerationV1,
        timeout_seconds: u64,
    ) -> Option<NodMaterializationObservationV1> {
        let stall = Duration::from_secs(timeout_seconds);
        let mut deadline = MaterializationStallDeadline::new(Instant::now(), stall);
        loop {
            if let Some(completed) = self.completed_nod_materialization(port, generation) {
                return Some(completed);
            }
            let head_progress = self
                .nod_materialization_head_on(port)
                .filter(|head| {
                    head.worldwide_day == generation.worldwide_day
                        && head.generation == generation.generation
                })
                .map_or(deadline.last_progress(), |head| head.next_nod_ordinal);
            let finalized_progress = if head_progress > deadline.last_progress() {
                self.materialization_progress_on(
                    port,
                    generation.worldwide_day,
                    generation.generation,
                )
                .and_then(|events| events.into_iter().map(|event| event.next_nod_ordinal).max())
                .unwrap_or(deadline.last_progress())
            } else {
                deadline.last_progress()
            };
            if deadline.observe(Instant::now(), finalized_progress) {
                eprintln!(
                    "NOD materialization made no finalized progress for {timeout_seconds}s: \
                     worldwide_day={} generation={} cursor={}/{}",
                    generation.worldwide_day,
                    generation.generation,
                    deadline.last_progress(),
                    generation.nod_count,
                );
                return None;
            }
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn completed_nod_materialization(
        &self,
        port: u16,
        expected: &OcompCertifiedGenerationV1,
    ) -> Option<NodMaterializationObservationV1> {
        let pending_projection = eth::read_call(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: expected.worldwide_day,
            },
        )?;
        if pending_projection.exists || self.nod_materialization_head_on(port).is_some() {
            return None;
        }
        let progress =
            self.materialization_progress_on(port, expected.worldwide_day, expected.generation)?;
        let completed = progress
            .iter()
            .find(|event| event.completed && event.next_nod_ordinal == expected.nod_count)?;
        Some(NodMaterializationObservationV1 {
            worldwide_day: expected.worldwide_day,
            generation: expected.generation,
            nod_count: expected.nod_count,
            next_nod_ordinal: completed.next_nod_ordinal,
            successful_batch_transactions: u32::try_from(progress.len()).ok()?,
            completion_block_number: completed.block_number,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn assert_one_materialized_nod_for_owner(
        &self,
        port: u16,
        owner: Address,
        completion_block_number: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let last_observation = match self.materialized_nod_for_owner(port, owner) {
                Ok(Some((nod_id, body))) => {
                    if body.owner != owner || body.nodId != U256::from_be_slice(&nod_id) {
                        return Err(eyre!("owner enumeration and nodData disagree"));
                    }
                    return Ok(());
                }
                Ok(None) => "balanceOf returned zero".to_owned(),
                Err(error) => error,
            };
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "materialized owner read did not become available: owner={owner:#x} completion_block={completion_block_number} head={:?} finalized={:?} last_observation={last_observation}",
                    self.head(port),
                    self.finalized(port),
                ));
            }
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn mine_first_materialized_capacity_nod(
        &self,
        port: u16,
        private_key: &str,
        pay_note_proof: &[u8],
    ) -> Result<()> {
        let owner = self
            .address_of(private_key)
            .ok_or_else(|| eyre!("derive capacity owner"))?
            .parse::<Address>()
            .wrap_err("parse capacity owner")?;
        let nod_id = self
            .nod_id_of_owner_by_index_on(port, owner, 0)
            .map_err(|error| eyre!("capacity owner NOD read failed: {error}"))?
            .ok_or_else(|| eyre!("capacity owner NOD is unavailable"))?;
        let body = self
            .nod_data_on(port, &nod_id)
            .map_err(|error| eyre!("capacity owner NOD body read failed: {error}"))?;
        let entity = outbe_compressed_entities::WwdEntityId::try_from(nod_id.as_slice())?;
        let nonce = (0_u64..100_000)
            .find(|nonce| outbe_nodfactory::runtime::validate_pow(entity, *nonce).is_ok())
            .ok_or_else(|| eyre!("find bounded mineGratis nonce"))?;
        let op_nonce = eth::read_call(
            &self.url(port),
            addresses::GRATIS_ADDR,
            &IGratis::opNonceOfCall { account: owner },
        )
        .ok_or_else(|| eyre!("read Gratis op nonce"))?;
        let modify_key = eth::derive_gratis_modify_key(&self.url(port), private_key)?;
        let chain_id = B256::from(U256::from(
            self.chain_id(port)
                .ok_or_else(|| eyre!("read chain ID for mineGratis"))?,
        ));
        let mac = outbe_tee_enclave::gratis::modify_mac(
            &modify_key,
            owner,
            outbe_tee::protocol::GratisOp::Mint,
            body.gratisLoadMinor,
            op_nonce,
            chain_id,
        );
        let transaction_hash = eth::send_call(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            private_key,
            &INodFactory::mineGratisCall {
                nodId: U256::from_be_slice(&nod_id),
                nonce,
                mac: B256::from(mac),
                opNonce: op_nonce,
                payNoteProof: Bytes::copy_from_slice(pay_note_proof),
            },
            None,
        )?;
        if eth::receipt_success(&self.url(port), &transaction_hash) != Some(true) {
            return Err(eyre!("post-completion mineGratis transaction failed"));
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn nod_materialization_head_on(&self, port: u16) -> Option<NodMaterializationHeadV1> {
        let head = eth::read_call(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            &INodFactory::materializationHeadCall {},
        )?;
        head.exists.then(|| {
            NodMaterializationHeadV1::decode_canonical(
                head.canonicalHead.as_ref(),
                &poc_schema_limits(),
            )
            .ok()
        })?
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn materialized_nod_for_owner(
        &self,
        port: u16,
        owner: Address,
    ) -> std::result::Result<Option<(Vec<u8>, crate::internal::eth::INod::NodData)>, String> {
        let balance = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::balanceOfCall { owner },
        )?;
        if balance.is_zero() {
            return Ok(None);
        }
        if balance != U256::from(1) {
            return Err(format!(
                "balanceOf returned {balance}, expected exactly one"
            ));
        }
        let nod_id = self
            .nod_id_of_owner_by_index_on(port, owner, 0)?
            .ok_or_else(|| "index zero unexpectedly reported absence".to_owned())?;
        if self.nod_id_of_owner_by_index_on(port, owner, 1)?.is_some() {
            return Err("owner has more than one materialized NOD".to_owned());
        }
        let body = self.nod_data_on(port, &nod_id)?;
        Ok(Some((nod_id, body)))
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn nod_id_of_owner_by_index_on(
        &self,
        port: u16,
        owner: Address,
        index: u64,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        let result = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::tokenOfOwnerByIndexCall {
                owner,
                index: U256::from(index),
            },
        )
        .map(|value| value.to_be_bytes::<32>().to_vec());
        classify_owner_index_result(index, result)
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn nod_data_on(
        &self,
        port: u16,
        nod_id: &[u8],
    ) -> std::result::Result<crate::internal::eth::INod::NodData, String> {
        eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::nodDataCall {
                nodId: U256::from_be_slice(nod_id),
            },
        )
    }

    #[cfg(feature = "ocomp-integration")]
    fn materialization_progress_on(
        &self,
        port: u16,
        worldwide_day: u32,
        generation: u64,
    ) -> Option<Vec<NodMaterializationProgressV1>> {
        let finalized_height = eth::finalized_number(&self.url(port))?;
        let topic0 = keccak256(
            b"NodMaterializationProgress(uint64,uint32,uint64,uint32,uint32,bool,uint64)",
        );
        let logs = eth::raw_json_with_params(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::NOD_FACTORY_ADDR),
                "fromBlock": "0x0",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}")],
            }]),
        )?;
        Some(
            logs.as_array()?
                .iter()
                .filter_map(decode_nod_materialization_progress)
                .filter(|event| {
                    event.worldwide_day == worldwide_day && event.generation == generation
                })
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) fn url(&self, port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }

    // ---- reads ----------------------------------------------------------

    /// Head block number on the node at `port` (`eth_blockNumber`).
    pub fn head(&self, port: u16) -> Option<u64> {
        eth::block_number(&self.url(port))
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
    pub fn txpool_has(&self, port: u16, tx_hash: &str) -> bool {
        let Some(content) = eth::raw_json(&self.url(port), "txpool_content") else {
            return false;
        };
        let needle = tx_hash.to_ascii_lowercase();
        // `txpool_content` is {pending|queued: {sender: {nonce: tx}}}; the hash
        // lives inside each tx object, so a serialized-contains check is both
        // sufficient and immune to field-layout changes.
        serde_json::to_string(&content)
            .map(|text| text.to_ascii_lowercase().contains(&needle))
            .unwrap_or(false)
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

    #[cfg(feature = "ocomp-integration")]
    pub fn desis_auction_stage_on(
        &self,
        port: u16,
        worldwide_day: u32,
        block_number: u64,
    ) -> Option<u8> {
        eth::read_call_at(
            &self.url(port),
            addresses::DESIS_ADDR,
            &IDesis::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
            block_number,
        )
        .map(|stage| stage as u8)
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
            &ITeeRegistryV1::isBootstrappedCall {},
        )
        .unwrap_or(false)
    }

    /// Active protocol version (`IUpdate.getActiveVersion`).
    pub fn active_version(&self) -> Option<u64> {
        self.active_version_on_url(&self.cfg.rpc0)
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn active_ocomp_protocol_bundle_hash_on(&self, port: u16) -> Option<B256> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::activeProtocolBundleHashCall {},
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn retiring_ocomp_protocol_bundle_hash_on(&self, port: u16) -> Option<B256> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retiringProtocolBundleHashCall {},
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_live_lineage_count_on(&self, port: u16, bundle_hash: B256) -> Option<u32> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::liveLineageCountCall {
                protocolBundleHash: bundle_hash,
            },
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_retention_until_on(&self, port: u16, bundle_hash: B256) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retentionUntilCall {
                protocolBundleHash: bundle_hash,
            },
        )
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
            &IUpdate::getScheduledUpdateCall {
                proposalId: U256::from(id),
            },
        )?;
        Some(ScheduledUpdate {
            version: r.version as u64,
            activation: r.activationHeight,
            status: r.status as u64,
        })
    }

    /// OIP record (`IGovernance.getOip`) - `(status, author, text)`.
    pub fn get_oip(&self, id: u64) -> Option<(u8, Address, String)> {
        let r = eth::read_call(
            &self.cfg.rpc0,
            addresses::GOVERNANCE_ADDR,
            &IGovernance::getOipCall { id: U256::from(id) },
        )?;
        Some((r.status, r.author, r.text))
    }

    /// GIP record (`IGovernance.getGip`) - `(status, author, text)`.
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

    /// Submit a Stablecoin Factory proposal through the production operator CLI.
    #[allow(clippy::too_many_arguments)]
    pub fn stablecoin_propose(
        &self,
        key: &str,
        name: &str,
        ticker: &str,
        iso4217: u16,
        supply_cap: U256,
        policy_id: U256,
    ) -> Result<String> {
        let iso4217 = iso4217.to_string();
        let supply_cap = supply_cap.to_string();
        let policy_id = policy_id.to_string();
        let out = self.sh().cli([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "stablecoin",
            "propose",
            "--name",
            name,
            "--ticker",
            ticker,
            "--iso4217",
            iso4217.as_str(),
            "--supply-cap",
            supply_cap.as_str(),
            "--policy-id",
            policy_id.as_str(),
        ])?;
        parse::extract_tx_hash(&out)
            .ok_or_else(|| eyre!("no tx hash in stablecoin propose output:\n{out}"))
    }

    /// Submit a Stablecoin Factory proposal expected to fail during RPC preflight.
    #[allow(clippy::too_many_arguments)]
    pub fn stablecoin_propose_rejection(
        &self,
        key: &str,
        name: &str,
        ticker: &str,
        iso4217: u16,
        supply_cap: U256,
        policy_id: U256,
    ) -> Result<String> {
        let iso4217 = iso4217.to_string();
        let supply_cap = supply_cap.to_string();
        let policy_id = policy_id.to_string();
        self.sh().cli_expected_failure([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "stablecoin",
            "propose",
            "--name",
            name,
            "--ticker",
            ticker,
            "--iso4217",
            iso4217.as_str(),
            "--supply-cap",
            supply_cap.as_str(),
            "--policy-id",
            policy_id.as_str(),
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

    /// The full `validatorByAddress` record, or `None` if absent/unreadable.
    pub fn validator_record(&self, port: u16, addr: &str) -> Option<ValidatorRecord> {
        let v: Address = addr.parse().ok()?;
        let record = eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::validatorByAddressCall { addr: v },
        )?;
        Some(ValidatorRecord {
            address: record.validatorAddress,
            consensus_pubkey: record.consensusPubkey,
            stake: record.stake,
            status: record.status,
            slash_count: record.slashCount,
            missed_blocks: record.missedBlocks,
            missed_votes: record.missedVotes,
            blocks_proposed: record.blocksProposed,
            joined_at_height: record.joinedAtHeight,
            deactivated_at_height: record.deactivatedAtHeight,
            unbonding_end: record.unbondingEnd,
            has_bls_share: record.hasBLSShare,
        })
    }

    /// The full `validatorByAddress` record at one exact canonical block.
    pub fn validator_record_at(
        &self,
        port: u16,
        addr: &str,
        block_number: u64,
    ) -> Option<ValidatorRecord> {
        let v: Address = addr.parse().ok()?;
        let record = eth::read_call_at(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::validatorByAddressCall { addr: v },
            block_number,
        )?;
        Some(ValidatorRecord {
            address: record.validatorAddress,
            consensus_pubkey: record.consensusPubkey,
            stake: record.stake,
            status: record.status,
            slash_count: record.slashCount,
            missed_blocks: record.missedBlocks,
            missed_votes: record.missedVotes,
            blocks_proposed: record.blocksProposed,
            joined_at_height: record.joinedAtHeight,
            deactivated_at_height: record.deactivatedAtHeight,
            unbonding_end: record.unbondingEnd,
            has_bls_share: record.hasBLSShare,
        })
    }

    /// The full record at the one-based dense ValidatorSet index.
    pub fn validator_record_by_index(&self, port: u16, index: u64) -> Option<ValidatorRecord> {
        let record = eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::validatorByIndexCall { index },
        )?;
        Some(ValidatorRecord {
            address: record.validatorAddress,
            consensus_pubkey: record.consensusPubkey,
            stake: record.stake,
            status: record.status,
            slash_count: record.slashCount,
            missed_blocks: record.missedBlocks,
            missed_votes: record.missedVotes,
            blocks_proposed: record.blocksProposed,
            joined_at_height: record.joinedAtHeight,
            deactivated_at_height: record.deactivatedAtHeight,
            unbonding_end: record.unbondingEnd,
            has_bls_share: record.hasBLSShare,
        })
    }

    /// Dense ValidatorSet membership, including non-active records.
    pub fn validators(&self, port: u16) -> Option<Vec<Address>> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getValidatorsCall {},
        )
    }

    /// Validators whose persisted status is ACTIVE, share or no share.
    pub fn active_validators(&self, port: u16) -> Option<Vec<Address>> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getActiveValidatorsCall {},
        )
    }

    /// Current consensus participants selected by status plus share ownership.
    pub fn active_consensus_set(&self, port: u16) -> Option<Vec<Address>> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getActiveConsensusSetCall {},
        )
    }

    /// Number of records in the dense ValidatorSet index.
    pub fn validator_count(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::validatorCountCall {},
        )
        .map(u64::from)
    }

    /// Whether an address currently owns a ValidatorSet record.
    pub fn is_validator(&self, port: u16, addr: &str) -> Option<bool> {
        let v: Address = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::isValidatorCall { addr: v },
        )
    }

    /// Whether consensus should schedule another validator-set change.
    pub fn has_pending_set_change(&self, port: u16) -> Option<bool> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::hasPendingSetChangeCall {},
        )
    }

    /// ValidatorSet epoch start timestamp.
    pub fn epoch_start_timestamp(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getEpochStartTimestampCall {},
        )
    }

    /// ValidatorSet epoch start block.
    pub fn epoch_start_block(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getEpochStartBlockCall {},
        )
    }

    /// Version and encoded P2P address. `(0, empty)` means no address is set.
    pub fn validator_p2p_address(&self, port: u16, addr: &str) -> Option<ValidatorP2pAddress> {
        let v: Address = addr.parse().ok()?;
        let value = eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getP2pAddressCall {
                validatorAddress: v,
            },
        )?;
        Some(ValidatorP2pAddress {
            version: value.version,
            encoded: value.encoded,
        })
    }

    pub fn validator_radicle_node_id(&self, port: u16, addr: &str) -> Option<B256> {
        let validator_address = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getRadicleNodeIdCall {
                validator: validator_address,
            },
        )
    }

    pub fn validator_radicle_node_id_at(
        &self,
        port: u16,
        addr: &str,
        block_number: u64,
    ) -> Option<B256> {
        let validator_address = addr.parse().ok()?;
        eth::read_call_at(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getRadicleNodeIdCall {
                validator: validator_address,
            },
            block_number,
        )
    }

    /// Status code: 0 REGISTERED, 1 PENDING, 2 ACTIVE, 3 EXITING,
    /// 4 UNBONDING, 5 INACTIVE, 6 JAILED.
    pub fn validator_status(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr)
            .map(|r| u64::from(r.status))
    }

    /// Felony slash counter.
    pub fn slash_count(&self, port: u16, addr: &str) -> Option<u64> {
        self.validator_record(port, addr).map(|r| r.slash_count)
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

    /// AgentReward claimable balance observed through the public ABI on one
    /// validator.
    pub fn get_agent_reward_claimable_balance_on(
        &self,
        port: u16,
        account: Address,
    ) -> Option<U256> {
        eth::read_call(
            &self.url(port),
            addresses::AGENT_REWARD_ADDR,
            &IAgentReward::getClaimableBalanceCall { account },
        )
    }

    /// Claim the caller's complete AgentReward balance in one pool as a Gem
    /// through an ordinary paid transaction and return its public receipt.
    pub fn claim_agent_reward_gem(&self, key: &str, pool: u8) -> Result<serde_json::Value> {
        let tx_hash = eth::send_call(
            &self.cfg.rpc0,
            addresses::AGENT_REWARD_ADDR,
            key,
            &IAgentReward::claimRewardCall { pool },
            None,
        )?;
        let receipt = eth::receipt_json(&self.cfg.rpc0, &tx_hash)
            .ok_or_else(|| eyre!("AgentReward claim receipt unavailable: {tx_hash}"))?;
        if !receipt_status(&receipt) {
            return Err(eyre!("AgentReward claim reverted: {tx_hash}"));
        }
        Ok(receipt)
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

    /// Number of finalized evidence-felony applications for `validator` at or
    /// after `from_block`.
    pub fn evidence_felony_event_count(
        &self,
        port: u16,
        validator: &str,
        from_block: u64,
    ) -> Option<usize> {
        let validator: Address = validator.parse().ok()?;
        let signature = keccak256("EvidenceFelonyApplied(address,address,uint256,uint256)");
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
        )?
        .as_array()
        .map(Vec::len)
    }

    /// Whether the validator holds a live DKG share.
    pub fn has_share(&self, port: u16, addr: &str) -> Option<bool> {
        self.validator_record(port, addr).map(|r| r.has_bls_share)
    }

    /// Whether `addr` is a current consensus participant (ACTIVE or EXITING-with-share).
    pub fn is_participant(&self, port: u16, addr: &str) -> bool {
        let Ok(v) = addr.parse::<Address>() else {
            return false;
        };
        eth::read_call(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::isConsensusParticipantCall { addr: v },
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

    /// SlashIndicator's cumulative proposer-miss counter.
    pub fn proposer_miss_count(&self, port: u16, addr: &str) -> Option<u64> {
        let validator = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::SLASH_ADDR,
            &ISlashIndicator::getProposerMissCountCall { validator },
        )
    }

    /// SlashIndicator's cumulative felony counter.
    pub fn felony_count(&self, port: u16, addr: &str) -> Option<u64> {
        let validator = addr.parse().ok()?;
        eth::read_call(
            &self.url(port),
            addresses::SLASH_ADDR,
            &ISlashIndicator::getFelonyCountCall { validator },
        )
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

    /// Nod total supply on the node at `port`.
    pub fn nod_supply(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::totalSupplyCall {},
        )
        .and_then(|value| u64::try_from(value).ok())
    }

    /// Canonical Tribute identities indexed by one owner.
    pub fn tributes_by_owner(&self, port: u16, owner: Address) -> Option<Vec<U256>> {
        eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getTributesByOwnerCall { owner },
        )
    }

    /// Canonical Tribute identities indexed by one Worldwide Day.
    pub fn tributes_by_day(&self, port: u16, worldwide_day: u32) -> Option<Vec<U256>> {
        eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getTributesByDayCall {
                worldwideDay: worldwide_day,
            },
        )
    }

    /// Metadosis worldwide-day status byte (field 1 of `getWorldwideDay`).
    pub fn wwd_status(&self, port: u16, wwd: &str) -> Option<String> {
        let day: u32 = wwd.parse().ok()?;
        let r = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
        )?;
        Some(r.status.to_string())
    }

    pub fn metadosis_wwd_state_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let r = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
        )?;
        Some(MetadosisWorldwideDayStateV1 {
            status: r.status,
            day_type: r.dayType,
            forming_start: r.formingStart,
            forming_end: r.formingEnd,
            lookback_end: r.lookbackEnd,
            offering_end: r.offeringEnd,
            scheduled_process_time: r.scheduledProcessTime,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn metadosis_terminal_receipt_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayTerminalReceiptV1> {
        let receipt = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayTerminalReceiptCall { wwd: day },
        )?;
        Some(MetadosisWorldwideDayTerminalReceiptV1 {
            outcome: receipt.outcome,
            value_routed: receipt.valueRouted,
            carry_over_before: receipt.carryOverBefore,
            carry_over_after: receipt.carryOverAfter,
            retirement_outcome: receipt.retirementOutcome,
            block_number: receipt.blockNumber,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn promis_limit_total_unallocated_on(&self, port: u16) -> Option<U256> {
        eth::read_call(
            &self.url(port),
            addresses::PROMIS_LIMIT_ADDR,
            &IPromisLimit::totalUnallocatedCall {},
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn metadosis_wwd_state_at(
        &self,
        port: u16,
        day: u32,
        block_number: u64,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let r = eth::read_call_at(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
            block_number,
        )?;
        Some(MetadosisWorldwideDayStateV1 {
            status: r.status,
            day_type: r.dayType,
            forming_start: r.formingStart,
            forming_end: r.formingEnd,
            lookback_end: r.lookbackEnd,
            offering_end: r.offeringEnd,
            scheduled_process_time: r.scheduledProcessTime,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn metadosis_unknown_status_reverts_at(
        &self,
        port: u16,
        status: u8,
        block_number: u64,
    ) -> Option<bool> {
        eth::read_call_reverts_at(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDaysByStatusCall { status },
            block_number,
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_metadosis_wwd_started_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStartedV1> {
        const SIGNATURE: &str = "WorldwideDayStarted(uint32,uint64,uint64,uint64,uint64,uint64)";
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let topic0 = keccak256(SIGNATURE.as_bytes());
        let indexed_day = format!("0x{day:064x}");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": "0x1",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}"), indexed_day]
            }]),
        )?;
        let logs = logs.as_array()?;
        if logs.len() != 1 {
            return None;
        }
        let log = &logs[0];
        let data = decode_rpc_data_words(log, 5)?;
        let block_number = rpc_log_block_number(log)?;
        let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
        Some(MetadosisWorldwideDayStartedV1 {
            worldwide_day: day,
            forming_start: u64::try_from(data[0]).ok()?,
            forming_end: u64::try_from(data[1]).ok()?,
            lookback_end: u64::try_from(data[2]).ok()?,
            offering_end: u64::try_from(data[3]).ok()?,
            scheduled_process_time: u64::try_from(data[4]).ok()?,
            block_number,
            block_hash,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_metadosis_wwd_status_changes_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<Vec<MetadosisWorldwideDayStatusChangeV1>> {
        const SIGNATURE: &str = "WorldwideDayStatusChange(uint32,uint8,uint8,uint64)";
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let topic0 = keccak256(SIGNATURE.as_bytes());
        let indexed_day = format!("0x{day:064x}");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": "0x1",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}"), indexed_day]
            }]),
        )?;
        logs.as_array()?
            .iter()
            .map(|log| {
                let data = decode_rpc_data_words(log, 3)?;
                let block_number = rpc_log_block_number(log)?;
                if u64::try_from(data[2]).ok()? != block_number {
                    return None;
                }
                let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
                Some(MetadosisWorldwideDayStatusChangeV1 {
                    worldwide_day: day,
                    old_status: u8::try_from(data[0]).ok()?,
                    new_status: u8::try_from(data[1]).ok()?,
                    block_number,
                    block_hash,
                })
            })
            .collect()
    }

    /// A JSON field from `outbe_consensusStatus` on the node at `port`.
    pub fn consensus_status_field(&self, port: u16, field: &str) -> Option<String> {
        let v = eth::raw_json(&self.url(port), "outbe_consensusStatus")?;
        match v.get(field)? {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Whether the local consensus runtime has a private threshold share for
    /// the currently active DKG material.
    pub fn has_threshold_shares(&self, port: u16) -> Option<bool> {
        eth::raw_json(&self.url(port), "outbe_consensusStatus")?
            .get("hasThresholdShares")?
            .as_bool()
    }

    /// Immutable Radicle integration status as published by validator `port`.
    pub fn radicle_status(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radicleStatus")
    }

    /// Canonical signed endpoint evidence known by validator `port`.
    pub fn radicle_peers(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radiclePeers")
    }

    /// Finalized desired repositories and their local availability state.
    pub fn radicle_repositories(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radicleRepositories")
    }

    /// Permissionlessly register one public Heartwood repository.
    pub fn register_radicle_repository(&self, key: &str, repo_id: [u8; 20]) -> Result<String> {
        let tx = eth::send_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS,
            key,
            &IRadicleRegistry::registerRepositoryCall {
                repoId: repo_id.into(),
            },
            None,
        )?;
        if !self.wait_successful_receipt(&tx, 240) {
            return Err(eyre!(
                "Radicle repository registration receipt was not successful: {tx}"
            ));
        }
        Ok(tx)
    }

    /// `(count, generation, maximum)` from the finalized repository registry view.
    pub fn radicle_registry_state(&self) -> Option<(u32, u64, u32)> {
        let address = outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS;
        let count: u32 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::repositoryCountCall {},
        )?;
        let generation: u64 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::registryGenerationCall {},
        )?;
        let maximum: u32 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::maxRepositoriesCall {},
        )?;
        Some((count, generation, maximum))
    }

    /// Canonical voter-miss counter for `validator` as observed on `port`.
    pub fn voter_miss_count(&self, port: u16, validator: &str) -> Option<u64> {
        let value = eth::raw_json_with_params(
            &self.url(port),
            "outbe_getSlashInfo",
            serde_json::json!([validator]),
        )?;
        let misses = value.get("voterMissCount")?;
        misses.as_u64().or_else(|| {
            misses
                .as_str()
                .and_then(|encoded| u64::from_str_radix(encoded.trim_start_matches("0x"), 16).ok())
        })
    }

    // ---- identity + sends ----------------------------------------------------

    /// EOA address for a private key (`0x`-hex).
    pub fn address_of(&self, key: &str) -> Option<String> {
        eth::address_of(key).map(|a| format!("{a:#x}"))
    }

    /// Create one Tribute for the currently OFFERING WorldwideDay.
    pub fn create_tribute(&self, key: &str) -> Option<String> {
        const OFFERING: u8 = 2;

        let days: Vec<u32> = eth::read_call(
            &self.cfg.rpc0,
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDaysByStatusCall { status: OFFERING },
        )?;
        let worldwide_day = *days.first()?;
        if days.len() > 1 {
            eprintln!(
                "multiple OFFERING WorldwideDays {days:?}; creating Tribute for {worldwide_day}"
            );
        }

        let tx_hash = self.tribute_offer(key, &worldwide_day.to_string())?;
        self.wait_successful_receipt(&tx_hash, 240)
            .then_some(tx_hash)
    }

    /// Submit a tribute offer for worldwide-day `wwd` from `key`; returns tx hash if any.
    pub fn tribute_offer(&self, key: &str, wwd: &str) -> Option<String> {
        self.tribute_offer_with_params(key, wwd, "100", "0", 840, false)
    }

    /// Submit a Tribute offer with explicit business fields. This is used by
    /// duplicate-identity tests to prove that `(owner, worldwide_day)`, rather
    /// than the rest of the encrypted payload, is the uniqueness boundary.
    pub fn tribute_offer_with_params(
        &self,
        key: &str,
        wwd: &str,
        amount_base: &str,
        amount_atto: &str,
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
            amount_base.to_owned(),
            "--amount-atto".to_owned(),
            amount_atto.to_owned(),
            "--currency".to_owned(),
            currency.to_string(),
        ];
        if exclude_from_intex_issuance {
            args.push("--exclude-from-intex-issuance".to_owned());
        }
        let out = self.sh().cli(args.iter().map(String::as_str)).ok()?;
        let tx_hash = parse::extract_tx_hash(&out)?;
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=submitted wall_ms={} cli_elapsed_ms={} tx={tx_hash} owner={} wwd={wwd} amount_base={amount_base} amount_atto={amount_atto} currency={currency} exclude={exclude_from_intex_issuance}",
            unix_time_millis(),
            started.elapsed().as_millis(),
            self.address_of(key).unwrap_or_else(|| "unknown".to_owned()),
        );
        Some(tx_hash)
    }

    /// Submit one real encrypted Tribute whose enclave result attributes one
    /// WAA and one SRA beneficiary. This is a harness-only producer for the
    /// existing public ABI; production reward accounting remains unchanged.
    #[cfg(feature = "ocomp-integration")]
    pub fn submit_tribute_offer_with_agent_rewards(
        &self,
        key: &str,
        wwd: &str,
        wallet_addresses: &[Address],
        sra_addresses: &[Address],
    ) -> Option<String> {
        let worldwide_day = wwd.parse::<u32>().ok()?;
        let creator = eth::address_of(key)?;
        let bootstrapped: bool = eth::read_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::isBootstrappedCall {},
        )?;
        if !bootstrapped {
            return None;
        }
        let offer_public_key: U256 = eth::read_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::tributeOfferPublicKeyCall {},
        )?;
        let entropy = format!(
            "agent-reward-tribute:{creator:#x}:{worldwide_day}:{}",
            unix_time_millis()
        );
        let tribute_draft_id = keccak256(entropy.as_bytes());
        let su_hash = keccak256([entropy.as_bytes(), b":su"].concat());
        let plaintext = encode_reward_bearing_tribute_plaintext(
            creator,
            tribute_draft_id,
            "100",
            "0",
            su_hash,
            wallet_addresses,
            sra_addresses,
        )
        .ok()?;
        let (cipher_text, nonce, ephemeral_public_key) =
            outbe_tee::offer_encrypt::encrypt_tribute_offer(
                &offer_public_key.to_be_bytes::<32>(),
                &plaintext,
            )
            .ok()?;
        let call = ITributeFactory::offerTributeCall {
            cipherText: cipher_text.into(),
            nonce: nonce.to_vec().into(),
            ephemeralPubkey: U256::from_be_bytes(ephemeral_public_key),
            worldwideDay: worldwide_day,
            tributeCurrency: 840,
            referenceCurrency: 840,
            excludeFromIntexIssuance: false,
            zkProof: Bytes::new(),
            zkVerificationKey: Bytes::new(),
            zkPublicKey: Bytes::new(),
            zkMerkleRoot: Bytes::new(),
            signature: Bytes::new(),
        };
        let outcome = eth::send_call_outcome(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS,
            key,
            &call,
            Some(U256::ZERO),
        )
        .ok()?;
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=agent-reward-submitted wall_ms={} tx={} owner={creator:#x} wwd={worldwide_day} waa={} sra={}",
            unix_time_millis(),
            outcome.transaction_hash,
            wallet_addresses.len(),
            sra_addresses.len(),
        );
        Some(outcome.transaction_hash)
    }

    /// Submit one real encrypted Tribute while keeping issuance and reference
    /// currencies independent. The product CLI intentionally remains the
    /// same-currency operator path; this narrow E2E helper exercises the
    /// already-public ABI axis without adding a new product surface.
    #[allow(clippy::too_many_arguments)]
    pub fn tribute_cross_currency_offer(
        &self,
        key: &str,
        wwd: &str,
        amount_base: &str,
        amount_atto: &str,
        tribute_currency: u16,
        reference_currency: u16,
        exclude_from_intex_issuance: bool,
    ) -> Option<String> {
        let worldwide_day = wwd.parse::<u32>().ok()?;
        let creator = eth::address_of(key)?;
        let bootstrapped: bool = eth::read_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::isBootstrappedCall {},
        )?;
        if !bootstrapped {
            return None;
        }
        let offer_public_key: U256 = eth::read_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::tributeOfferPublicKeyCall {},
        )?;
        let offer_public_key: [u8; 32] = offer_public_key.to_be_bytes();
        let entropy = format!(
            "cross-currency-tribute:{creator:#x}:{worldwide_day}:{}",
            unix_time_millis()
        );
        let tribute_draft_id = keccak256(entropy.as_bytes());
        let su_hash = keccak256([entropy.as_bytes(), b":su"].concat());
        let plaintext = serde_json::to_vec(&serde_json::json!({
            "creator": format!("{creator:?}"),
            "tribute_draft_id": format!("{tribute_draft_id:#x}"),
            "amount_base": amount_base,
            "amount_atto": amount_atto,
            "su_hashes": [format!("{su_hash:#x}")],
            "wallet_addresses": [],
            "sra_addresses": [],
        }))
        .ok()?;
        let (cipher_text, nonce, ephemeral_public_key) =
            outbe_tee::offer_encrypt::encrypt_tribute_offer(&offer_public_key, &plaintext).ok()?;
        let call = ITributeFactory::offerTributeCall {
            cipherText: cipher_text.into(),
            nonce: nonce.to_vec().into(),
            ephemeralPubkey: U256::from_be_bytes(ephemeral_public_key),
            worldwideDay: worldwide_day,
            tributeCurrency: tribute_currency,
            referenceCurrency: reference_currency,
            excludeFromIntexIssuance: exclude_from_intex_issuance,
            zkProof: Bytes::new(),
            zkVerificationKey: Bytes::new(),
            zkPublicKey: Bytes::new(),
            zkMerkleRoot: Bytes::new(),
            signature: Bytes::new(),
        };
        let outcome = eth::send_call_outcome(
            &self.cfg.rpc0,
            outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS,
            key,
            &call,
            Some(U256::ZERO),
        )
        .ok()?;
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=cross-currency-submitted wall_ms={} tx={} owner={creator:#x} wwd={worldwide_day} tribute_currency={tribute_currency} reference_currency={reference_currency}",
            unix_time_millis(),
            outcome.transaction_hash,
        );
        Some(outcome.transaction_hash)
    }

    /// Read the exact WWD VWAP and the maximum active S-curve value for one
    /// COEN/ISO reference pair from the canonical Oracle precompile.
    pub fn oracle_wwd_vwap_and_scurve(
        &self,
        port: u16,
        worldwide_day: u32,
        iso_code: u16,
    ) -> Option<(U256, U256)> {
        let oracle = outbe_primitives::addresses::ORACLE_ADDRESS;
        let snapshot = eth::read_call(
            &self.url(port),
            oracle,
            &IOracle::getWorldwideDayVwapSnapshotCall {
                worldwideDay: worldwide_day,
            },
        )?;
        let quote = outbe_primitives::asset_type::currency_address(iso_code);
        let vwap = snapshot
            .bases
            .iter()
            .zip(&snapshot.quotes)
            .zip(&snapshot.vwaps)
            .find_map(|((base, candidate_quote), value)| {
                (*base == Address::ZERO && *candidate_quote == quote).then_some(*value)
            })?;
        let curve = eth::read_call(
            &self.url(port),
            oracle,
            &IOracle::getScurveValuesCall {
                base: Address::ZERO,
                quote,
                timestamp: outbe_primitives::time::date_key_to_utc_timestamp(worldwide_day),
            },
        )?;
        Some((vwap, curve.values.into_iter().max().unwrap_or(U256::ZERO)))
    }

    /// Read one canonical COEN/ISO rate together with its publication point.
    pub fn oracle_rate_data(&self, port: u16, iso_code: u16) -> Option<OracleRateDataV1> {
        let result = eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getExchangeRateDataCall {
                base: Address::ZERO,
                quote: outbe_primitives::asset_type::currency_address(iso_code),
            },
        )?;
        Some(OracleRateDataV1 {
            rate: result.rate,
            last_block: result.lastBlock,
            last_timestamp: result.lastTimestamp,
        })
    }

    /// Read the canonical chain-owned Oracle vote period used by production
    /// feeder preflight. Harness feeder config must match it exactly.
    pub fn oracle_vote_period(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            outbe_primitives::addresses::ORACLE_ADDRESS,
            &IOracle::getParamsCall {},
        )
        .map(|params| params.votePeriod)
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

    /// Public JSON-RPC receipt used by OCOMP evidence correlation.
    pub fn transaction_receipt(&self, tx_hash: &str, port: u16) -> Option<serde_json::Value> {
        eth::receipt_json(&self.url(port), tx_hash)
    }

    /// Observe a finalized `OffchainJobRequested` log through the public RPC.
    ///
    /// This is intentionally read-only: the harness cannot create a job or
    /// provide any of its bindings. The returned block hash is checked against
    /// the canonical block independently read from the same validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request(&self, from_height: u64) -> Option<OcompPublicJobRequestV1> {
        self.finalized_ocomp_job_request_on_url(&self.cfg.rpc0, from_height, None)
    }

    /// Observe the same finalized OCOMP request on one named validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request_on(
        &self,
        port: u16,
        from_height: u64,
    ) -> Option<OcompPublicJobRequestV1> {
        self.finalized_ocomp_job_request_on_url(&self.url(port), from_height, None)
    }

    /// Observe the latest finalized OCOMP request for one exact WorldwideDay.
    /// Later retry-chain events for another day cannot hide the requested job.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request_for_worldwide_day_on(
        &self,
        port: u16,
        from_height: u64,
        worldwide_day: u32,
    ) -> Option<OcompPublicJobRequestV1> {
        self.finalized_ocomp_job_request_on_url(&self.url(port), from_height, Some(worldwide_day))
    }

    #[cfg(feature = "ocomp-integration")]
    fn finalized_ocomp_job_request_on_url(
        &self,
        rpc_url: &str,
        from_height: u64,
        worldwide_day: Option<u32>,
    ) -> Option<OcompPublicJobRequestV1> {
        const EVENT_SIGNATURE: &str = "OffchainJobRequested(bytes32,uint32,uint64,uint32,bytes32)";
        let finalized_height = eth::finalized_number(rpc_url)?;
        if finalized_height < from_height {
            return None;
        }
        let topic0 = keccak256(EVENT_SIGNATURE.as_bytes());
        let logs = eth::raw_json_with_params(
            rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": format!("0x{from_height:x}"),
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}")]
            }]),
        )?;
        let logs = logs.as_array()?;
        let log = select_ocomp_job_request_log(logs, worldwide_day)?;
        let topics = log.get("topics")?.as_array()?;
        if topics.len() != 3 || topics[0].as_str()? != format!("{topic0:#x}") {
            return None;
        }
        let intent_id = topics[1].as_str()?.parse::<B256>().ok()?;
        let worldwide_day = u32::try_from(parse_rpc_word(topics[2].as_str()?)?).ok()?;
        let data = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
        if data.len() != 3 * 32 {
            return None;
        }
        let pending_nonce = u64::try_from(U256::from_be_slice(&data[0..32])).ok()?;
        let attempt = u32::try_from(U256::from_be_slice(&data[32..64])).ok()?;
        let activation_preconditions_hash = B256::from_slice(&data[64..96]);
        let request_height = u64::from_str_radix(
            log.get("blockNumber")?.as_str()?.trim_start_matches("0x"),
            16,
        )
        .ok()?;
        if request_height < from_height || request_height > finalized_height {
            return None;
        }
        let request_block_hash = log.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
        if eth::block_hash(rpc_url, request_height)?
            .parse::<B256>()
            .ok()?
            != request_block_hash
        {
            return None;
        }
        let encoded_record = eth::read_call_at(
            rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainJobCall {
                intentId: intent_id,
            },
            finalized_height,
        )?;
        let limits = poc_schema_limits();
        let record = OcompJobRecordV1::decode_canonical(encoded_record.as_ref(), &limits).ok()?;
        let finalized = record.finalized.as_ref()?;
        if record.intent.intent_id(&limits).ok()? != intent_id
            || record.intent.wwd != worldwide_day
            || record.intent.pending_nonce != pending_nonce
            || record.intent.attempt != attempt
            || record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)
                .ok()?
                != activation_preconditions_hash
            || finalized.deadline_height <= finalized.open_height
        {
            return None;
        }
        Some(OcompPublicJobRequestV1 {
            intent_id,
            worldwide_day,
            pending_nonce,
            attempt,
            finality_recorded_height: finalized.finality_recorded_height,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
            activation_preconditions_hash,
            request_height,
            request_block_hash,
            transaction_hash: log.get("transactionHash")?.as_str()?.parse::<B256>().ok()?,
        })
    }

    /// Read and decode the canonical finalized job record on one validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_record_on(
        &self,
        port: u16,
        intent_id: B256,
    ) -> Option<OcompJobRecordV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let encoded = eth::read_call_at(
            &rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainJobCall {
                intentId: intent_id,
            },
            finalized_height,
        )?;
        OcompJobRecordV1::decode_canonical(encoded.as_ref(), &poc_schema_limits()).ok()
    }

    /// Read and decode the four fixed result-vote slots at the finalized head.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_vote_accountability_on(
        &self,
        port: u16,
        job_id: B256,
    ) -> Option<OcompPublicVoteAccountabilityV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let encoded = eth::read_call_at(
            &rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainVoteAccountabilityCall { jobId: job_id },
            finalized_height,
        )?;
        let accountability =
            OcompVoteAccountabilityV1::decode_canonical(encoded.as_ref(), &poc_schema_limits())
                .ok()?;
        let quorum = accountability.quorum.as_ref();
        let closed = accountability.closed_summary.as_ref();
        Some(OcompPublicVoteAccountabilityV1 {
            job_id: accountability.job_id,
            result_validator_set_epoch: accountability.result_validator_set_epoch,
            result_committee_set_hash: accountability.result_committee_set_hash,
            result_ocomp_binding_hash: accountability.result_ocomp_binding_hash,
            member_count: accountability.member_count,
            quorum_threshold: accountability.quorum_threshold,
            slot_validator_indexes: accountability
                .slots
                .iter()
                .flatten()
                .map(|slot| slot.validator_index)
                .collect(),
            slot_first_signatures: accountability
                .slots
                .iter()
                .flatten()
                .map(|slot| (slot.validator_index, slot.first_signature_rs.to_vec()))
                .collect(),
            quorum_result_digest: quorum.map(|value| value.result_digest),
            quorum_height: quorum.map(|value| value.quorum_height),
            quorum_signer_bitmap: quorum.map(|value| value.signer_bitmap.clone()),
            closed_height: closed.map(|value| value.closed_height),
            timely_bitmap: closed.map(|value| value.timely_bitmap.clone()),
            matching_bitmap: closed.map(|value| value.matching_bitmap.clone()),
            divergent_bitmap: closed.map(|value| value.divergent_bitmap.clone()),
            missing_bitmap: closed.map(|value| value.missing_bitmap.clone()),
            equivocation_bitmap: closed.map(|value| value.equivocation_bitmap.clone()),
        })
    }

    /// Enumerate canonical finalized public result-vote transactions from a
    /// bounded block range. This observes the real RPC -> txpool -> proposal ->
    /// import path rather than accepting transaction identities from a helper.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_result_vote_transactions_on(
        &self,
        port: u16,
        from_height: u64,
        to_height: u64,
    ) -> Option<Vec<OcompPublicResultVoteTransactionV1>> {
        if from_height > to_height {
            return None;
        }
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if to_height > finalized_height {
            return None;
        }
        const MAX_PUBLIC_VOTE_SCAN_BLOCKS: usize = 256;
        let selector = outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR;
        let mut observed = Vec::new();
        let blocks = eth::blocks_with_transactions(
            &rpc_url,
            from_height,
            to_height,
            MAX_PUBLIC_VOTE_SCAN_BLOCKS,
        )?;
        for (height, block) in (from_height..=to_height).zip(blocks) {
            let block_hash = block.get("hash")?.as_str()?.parse::<B256>().ok()?;
            let rpc_block = serde_json::from_value::<alloy_rpc_types::Block>(block.clone()).ok()?;
            let consensus_block: alloy_consensus::Block<alloy_consensus::TxEnvelope> =
                rpc_block.into();
            let block_rlp_len = alloy_rlp::Encodable::length(&consensus_block);
            let transactions = block.get("transactions")?.as_array()?;
            for transaction in transactions {
                let to = transaction
                    .get("to")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<Address>().ok());
                if to != Some(addresses::WWD_ADDR) {
                    continue;
                }
                let calldata =
                    hex::decode(transaction.get("input")?.as_str()?.trim_start_matches("0x"))
                        .ok()?;
                if calldata.get(..4) != Some(selector.as_slice()) {
                    continue;
                }
                let transaction_hash = transaction.get("hash")?.as_str()?.parse::<B256>().ok()?;
                let signer = transaction.get("from")?.as_str()?.parse::<Address>().ok()?;
                let receipt = eth::receipt_json(&rpc_url, &format!("{transaction_hash:#x}"))?;
                let receipt_block = receipt.get("blockNumber")?.as_str().and_then(|value| {
                    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
                })?;
                let receipt_hash = receipt.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
                if receipt_block != height || receipt_hash != block_hash {
                    return None;
                }
                let raw_transaction_len = eth::raw_json_with_params(
                    &rpc_url,
                    "eth_getRawTransactionByHash",
                    serde_json::json!([format!("{transaction_hash:#x}")]),
                )
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .and_then(|value| hex::decode(value.trim_start_matches("0x")).ok())
                .map(|bytes| bytes.len())?;
                let gas_used = receipt.get("gasUsed")?.as_str().and_then(|value| {
                    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
                })?;
                let success = receipt.get("status")?.as_str()? == "0x1";
                observed.push(OcompPublicResultVoteTransactionV1 {
                    transaction_hash,
                    signer,
                    block_number: height,
                    block_hash,
                    calldata_len: calldata.len(),
                    raw_transaction_len,
                    block_rlp_len,
                    gas_used,
                    success,
                });
            }
        }
        Some(observed)
    }

    /// Decode the exact canonical inner vote from one public transaction.
    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_result_vote_bytes_on(&self, port: u16, transaction_hash: B256) -> Option<Vec<u8>> {
        let transaction = eth::raw_json_with_params(
            &self.url(port),
            "eth_getTransactionByHash",
            serde_json::json!([format!("{transaction_hash:#x}")]),
        )?;
        let calldata =
            hex::decode(transaction.get("input")?.as_str()?.trim_start_matches("0x")).ok()?;
        if calldata.len() < 68
            || calldata.get(..4)
                != Some(outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR.as_slice())
            || U256::from_be_slice(&calldata[4..36]) != U256::from(32)
        {
            return None;
        }
        let payload_len = usize::try_from(U256::from_be_slice(&calldata[36..68])).ok()?;
        let payload_end = 68_usize.checked_add(payload_len)?;
        let padded_end = 68_usize.checked_add(payload_len.checked_add(31)? & !31)?;
        if calldata.len() != padded_end
            || payload_end > calldata.len()
            || calldata[payload_end..].iter().any(|byte| *byte != 0)
        {
            return None;
        }
        Some(calldata[68..payload_end].to_vec())
    }

    /// Submit an adversarial or replayed canonical inner vote through a normal
    /// public validator transaction. The caller supplies only bytes previously
    /// observed from the public chain (possibly deliberately mutated); it
    /// cannot insert protocol state directly.
    #[cfg(feature = "ocomp-integration")]
    pub fn submit_ocomp_result_vote_bytes(
        &self,
        port: u16,
        signer_key: &str,
        vote_bytes: Vec<u8>,
    ) -> Result<String> {
        let calldata = IMetadosis::submitLysisResultCall {
            resultVoteV1: Bytes::from(vote_bytes),
        }
        .abi_encode();
        eth::send_calldata(
            &self.url(port),
            addresses::WWD_ADDR,
            signer_key,
            calldata,
            outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        )
    }

    /// Observe the finalized public `LysisActivated` result on one validator.
    pub fn finalized_ocomp_activation_on(
        &self,
        port: u16,
        from_height: u64,
        expected_intent_id: B256,
    ) -> Option<OcompPublicActivationV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if finalized_height < from_height {
            return None;
        }
        let topic0 = keccak256(b"LysisActivated(bytes32,bytes32,bytes32,bytes32,bytes32,uint32)");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": format!("0x{from_height:x}"),
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [
                    format!("{topic0:#x}"),
                    format!("{expected_intent_id:#x}")
                ]
            }]),
        )?;
        let log = logs.as_array()?.last()?;
        let topics = log.get("topics")?.as_array()?;
        if topics.len() != 3
            || topics[0].as_str()? != format!("{topic0:#x}")
            || topics[1].as_str()? != format!("{expected_intent_id:#x}")
        {
            return None;
        }
        let intent_id = topics[1].as_str()?.parse::<B256>().ok()?;
        let job_id = topics[2].as_str()?.parse::<B256>().ok()?;
        let data = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
        if data.len() != 4 * 32 {
            return None;
        }
        let activation_call_id = B256::from_slice(&data[0..32]);
        let result_digest = B256::from_slice(&data[32..64]);
        let terminal_receipt_hash = B256::from_slice(&data[64..96]);
        let worldwide_day = u32::try_from(U256::from_be_slice(&data[96..128])).ok()?;
        let block_number = u64::from_str_radix(
            log.get("blockNumber")?.as_str()?.trim_start_matches("0x"),
            16,
        )
        .ok()?;
        if block_number < from_height || block_number > finalized_height {
            return None;
        }
        let block_hash = log.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
        if eth::block_hash(&rpc_url, block_number)?
            .parse::<B256>()
            .ok()?
            != block_hash
        {
            return None;
        }
        Some(OcompPublicActivationV1 {
            intent_id,
            job_id,
            activation_call_id,
            result_digest,
            terminal_receipt_hash,
            worldwide_day,
            block_number,
            block_hash,
            transaction_hash: log.get("transactionHash")?.as_str()?.parse::<B256>().ok()?,
        })
    }

    /// Read and verify the Metadosis and Nod generation projections at the
    /// exact finalized activation block.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_certified_generation_on(
        &self,
        port: u16,
        activation: &OcompPublicActivationV1,
    ) -> Option<OcompCertifiedGenerationV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if finalized_height < activation.block_number {
            return None;
        }
        let block_hash = eth::block_hash(&rpc_url, activation.block_number)?
            .parse::<B256>()
            .ok()?;
        if block_hash != activation.block_hash {
            return None;
        }

        let active_bytes = eth::read_call_at(
            &rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getActiveLysisGenerationCall {
                wwd: activation.worldwide_day,
            },
            activation.block_number,
        )?;
        let limits = poc_schema_limits();
        let active = ActiveGenerationV1::decode_canonical(active_bytes.as_ref(), &limits).ok()?;

        let nod = eth::read_call_at(
            &rpc_url,
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: activation.worldwide_day,
            },
            activation.block_number,
        )?;
        if !nod.exists || nod.worldwideDay != activation.worldwide_day {
            return None;
        }
        let nod = NodCertifiedGenerationProjection {
            worldwide_day: WorldwideDay::new(nod.worldwideDay),
            generation: nod.generation,
            job_id: active.job_id,
            // The public read does not surface it, and no assertion here looks at it.
            protocol_bundle_hash: B256::ZERO,
            program_semantics_hash: active.program_semantics_hash,
            nod_root: nod.nodRoot,
            bucket_root: nod.bucketRoot,
            output_manifest_root: nod.outputManifestRoot,
            tribute_count: nod.tributeCount,
            nod_count: nod.nodCount,
            bucket_count: nod.bucketCount,
            nod_amount_total: nod.nodAmountTotal,
            nod_gratis_consumed: nod.nodGratisConsumed,
            issued_at: nod.issuedAt,
            next_nod_ordinal: 0,
            last_progress_height: activation.block_number,
        };
        let authority = active_nod_set(&active, &nod).ok()?;
        if authority.job_id != activation.job_id {
            return None;
        }

        Some(OcompCertifiedGenerationV1 {
            worldwide_day: authority.worldwide_day,
            generation: authority.generation,
            job_id: authority.job_id,
            program_semantics_hash: authority.program_semantics_hash,
            nod_root: authority.nod_root,
            bucket_root: nod.bucket_root,
            output_manifest_root: nod.output_manifest_root,
            tribute_count: nod.tribute_count,
            nod_count: authority.nod_count,
            bucket_count: nod.bucket_count,
            nod_amount_total: nod.nod_amount_total,
            nod_gratis_consumed: nod.nod_gratis_consumed,
            issued_at: nod.issued_at,
            result_evidence_hash: active.result_evidence_hash,
            block_number: activation.block_number,
            block_hash,
        })
    }

    /// Observe whether the Nod owner already had a certified generation at an
    /// exact block. A malformed projection fails closed as `None`.
    #[cfg(feature = "ocomp-integration")]
    pub fn nod_certified_generation_exists_on(
        &self,
        port: u16,
        worldwide_day: u32,
        block_number: u64,
    ) -> Option<bool> {
        eth::read_call_at(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: worldwide_day,
            },
            block_number,
        )
        .map(|generation| generation.exists)
    }

    /// Read one governed L2 registry entry.
    pub fn l2_network(&self, chain_id: u64) -> Option<(Address, Vec<u8>, bool)> {
        let network = eth::read_call(
            &self.cfg.rpc0,
            addresses::L2_REGISTRY_ADDR,
            &IL2Registry::getNetworkCall { chainId: chain_id },
        )?;
        Some((
            network.l1Address,
            network.publicKey.to_vec(),
            network.zkEnabled,
        ))
    }

    /// Submit a Tribute offer carrying explicit L2 zk fields (`0x`-hex).
    pub fn tribute_offer_with_zk(
        &self,
        key: &str,
        wwd: &str,
        zk: TributeZkOffer<'_>,
    ) -> Option<String> {
        let args = vec![
            "--private-key".to_owned(),
            key.to_owned(),
            "--rpc-url".to_owned(),
            self.cfg.rpc0.clone(),
            "tribute".to_owned(),
            "offer".to_owned(),
            wwd.to_owned(),
            "--tribute-draft-id".to_owned(),
            zk.tribute_draft_id_hex.to_owned(),
            "--su-hash".to_owned(),
            zk.su_hash_hex.to_owned(),
            "--zk-merkle-root".to_owned(),
            zk.merkle_root_hex.to_owned(),
            "--zk-proof".to_owned(),
            zk.proof_hex.to_owned(),
            "--signature".to_owned(),
            zk.signature_hex.to_owned(),
        ];
        let out = self.sh().cli(args.iter().map(String::as_str)).ok()?;
        parse::extract_tx_hash(&out)
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
                validatorAddress: v,
                amount: base_units,
            },
            Some(base_units),
        )?;
        if !self.wait_successful_receipt(&tx, 20) {
            return Err(eyre!("stake receipt was not successful: {tx}"));
        }
        Ok(tx)
    }

    /// Submit validator registration and return either success or a mined
    /// contract-level revert without treating the latter as a transport error.
    pub fn register_validator(
        &self,
        caller_key: &str,
        validator: Address,
        consensus_pubkey: &[u8],
        radicle_node_id: B256,
        bls_signature: &[u8],
    ) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            caller_key,
            &IValidatorSet::registerValidatorCall {
                validatorAddress: validator,
                consensusPubkey: Bytes::copy_from_slice(consensus_pubkey),
                radicleNodeId: radicle_node_id,
                blsRegistrationSignature: Bytes::copy_from_slice(bls_signature),
            },
            None,
        )
        .map(Into::into)
    }

    /// Set the complete versioned P2P pair and retain reverted receipts for
    /// atomicity assertions.
    pub fn set_validator_p2p_address(
        &self,
        caller_key: &str,
        validator: Address,
        version: u8,
        encoded: &[u8],
    ) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            caller_key,
            &IValidatorSet::setP2pAddressCall {
                validatorAddress: validator,
                version,
                encoded: Bytes::copy_from_slice(encoded),
            },
            None,
        )
        .map(Into::into)
    }

    /// Unstake an exact base-unit amount, preserving reverted receipts.
    pub fn unstake_base_units(&self, key: &str, amount: U256) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::STK_ADDR,
            key,
            &IStaking::unstakeCall { amount },
            None,
        )
        .map(Into::into)
    }

    /// Unstake whole COEN, preserving reverted receipts.
    pub fn unstake(&self, key: &str, amount: u64) -> Result<TxOutcome> {
        self.unstake_base_units(key, eth::coen(amount))
    }

    /// Attempt to move the caller from JAILED to PENDING.
    pub fn unjail_validator(&self, key: &str) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::STK_ADDR,
            key,
            &IStaking::unjailValidatorCall {},
            None,
        )
        .map(Into::into)
    }

    /// Direct typed stale-join confirmation, including a reverted receipt.
    pub fn confirm_ready_outcome(&self, key: &str, validator_index: usize) -> Result<TxOutcome> {
        let registration_path = self
            .cfg
            .validator_dir(validator_index)
            .join("ocomp-registration-v1.ocb1");
        let registration = std::fs::read(&registration_path).wrap_err_with(|| {
            format!(
                "read validator-{validator_index} canonical OCOMP registration {}",
                registration_path.display()
            )
        })?;
        if registration.is_empty() {
            return Err(eyre!(
                "validator-{validator_index} canonical OCOMP registration is empty: {}",
                registration_path.display()
            ));
        }
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            key,
            &IValidatorSet::confirmValidatorReadyCall {
                registration: registration.into(),
            },
            None,
        )
        .map(Into::into)
    }

    /// Invoke the privileged raw reshared-set facade.
    pub fn activate_reshared_set(
        &self,
        caller_key: &str,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            caller_key,
            &IValidatorSetRaw::activateResharedSetCall {
                newActiveSet: new_active_set.to_vec(),
                groupPublicKey: active_set_hash,
            },
            None,
        )
        .map(Into::into)
    }

    /// Submit two conflicting notarize blocks to SlashIndicator.
    pub fn submit_conflicting_notarize_evidence(
        &self,
        submitter_key: &str,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<TxOutcome> {
        eth::send_call_outcome(
            &self.cfg.rpc0,
            addresses::SLASH_ADDR,
            submitter_key,
            &ISlashIndicator::submitConflictingNotarizeEvidenceCall {
                block1: Bytes::copy_from_slice(block1),
                block2: Bytes::copy_from_slice(block2),
            },
            None,
        )
        .map(Into::into)
    }

    /// Simulate conflicting-notarize evidence without changing state, retaining
    /// the node's revert text for E2E fixture diagnostics.
    pub fn simulate_conflicting_notarize_evidence(
        &self,
        submitter: Address,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<()> {
        eth::simulate_call(
            &self.cfg.rpc0,
            addresses::SLASH_ADDR,
            submitter,
            &ISlashIndicator::submitConflictingNotarizeEvidenceCall {
                block1: Bytes::copy_from_slice(block1),
                block2: Bytes::copy_from_slice(block2),
            },
        )
    }

    /// Submit `claimUnbonded()` followed by `registerValidator()` with
    /// sequential explicit nonces before waiting for either receipt.
    ///
    /// The returned receipts expose their block numbers; D-06 asserts that they
    /// match, proving re-registration happened before the next begin-block
    /// cleanup rather than after an automatically removed record.
    pub fn claim_unbonded_then_register(
        &self,
        key: &str,
        validator: Address,
        consensus_pubkey: &[u8],
        radicle_node_id: B256,
        bls_signature: &[u8],
    ) -> Result<[TxOutcome; 2]> {
        let claim = IStaking::claimUnbondedCall {};
        let register = IValidatorSet::registerValidatorCall {
            validatorAddress: validator,
            consensusPubkey: Bytes::copy_from_slice(consensus_pubkey),
            radicleNodeId: radicle_node_id,
            blsRegistrationSignature: Bytes::copy_from_slice(bls_signature),
        };
        let outcomes = eth::send_prepared_calls_outcomes(
            &self.cfg.rpc0,
            key,
            vec![
                eth::PreparedCall {
                    to: addresses::STK_ADDR,
                    data: Bytes::from(claim.abi_encode()),
                    value: None,
                },
                eth::PreparedCall {
                    to: addresses::VS_ADDR,
                    data: Bytes::from(register.abi_encode()),
                    value: None,
                },
            ],
        )?
        .into_iter()
        .map(Into::into)
        .collect::<Vec<TxOutcome>>();
        outcomes.try_into().map_err(|outcomes: Vec<TxOutcome>| {
            eyre!("expected 2 outcomes, got {}", outcomes.len())
        })
    }

    /// Confirm a PENDING joiner is synced/ready (stale-join guard).
    pub fn confirm_ready(&self, key: &str) -> Result<String> {
        let registration = self
            .cfg
            .validator_dir(self.cfg.validators)
            .join("ocomp-registration-v1.ocb1");
        let out = self.sh().cli([
            "--private-key",
            key,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "validator",
            "confirm-ready",
            "--registration",
            &registration.display().to_string(),
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
            &IValidatorSet::deactivateValidatorCall {
                validatorAddress: validator,
            },
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
        // Seed the exact eligibility boundary: one atomic unit is 0.000001
        // COEN. The bootstrap itself must neither consume that unit nor touch
        // the daily quota.
        eth::send_value(&self.cfg.rpc0, address, &funder_key, U256::from(1))?;
        let bootstrap_balance_before = eth::balance(&self.cfg.rpc0, address)
            .ok_or_else(|| eyre!("read pre-bootstrap balance"))?;
        if bootstrap_balance_before != U256::from(1) {
            return Err(eyre!(
                "bootstrap fixture balance must be exactly one atomic unit, got {bootstrap_balance_before}"
            ));
        }
        let bootstrap_nonce_before =
            eth::nonce(&self.cfg.rpc0, address).ok_or_else(|| eyre!("read bootstrap nonce"))?;

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

        let bootstrap_hash = self
            .sh()
            .cli_required([
                "--private-key",
                key,
                "--rpc-url",
                self.cfg.rpc0.as_str(),
                "zero-fee",
                "bootstrap",
            ])?
            .trim()
            .to_owned();
        if !self.wait_successful_receipt(&bootstrap_hash, 20) {
            return Err(eyre!(
                "product CLI ZeroFee bootstrap was not mined successfully: {bootstrap_hash}"
            ));
        }
        state.zerofee_delegation_receipt = Some(
            eth::receipt_json(&self.cfg.rpc0, &bootstrap_hash)
                .ok_or_else(|| eyre!("read product CLI bootstrap receipt"))?,
        );
        let bootstrap_receipt = state
            .zerofee_delegation_receipt
            .as_ref()
            .expect("bootstrap receipt was just stored");
        if !receipt_status(bootstrap_receipt) {
            return Err(eyre!("one-unit ZeroFee bootstrap receipt failed"));
        }
        let bootstrap_balance_after = eth::balance(&self.cfg.rpc0, address)
            .ok_or_else(|| eyre!("read post-bootstrap balance"))?;
        if bootstrap_balance_after != bootstrap_balance_before {
            return Err(eyre!(
                "bootstrap changed native balance: before={bootstrap_balance_before}, after={bootstrap_balance_after}"
            ));
        }
        let bootstrap_nonce_after = eth::nonce(&self.cfg.rpc0, address)
            .ok_or_else(|| eyre!("read post-bootstrap nonce"))?;
        let expected_bootstrap_nonce = bootstrap_nonce_before
            .checked_add(2)
            .ok_or_else(|| eyre!("bootstrap nonce overflow"))?;
        if bootstrap_nonce_after != expected_bootstrap_nonce {
            return Err(eyre!(
                "bootstrap nonce must advance by two: before={bootstrap_nonce_before}, after={bootstrap_nonce_after}"
            ));
        }
        let counter_after_bootstrap = self
            .zerofee_counter(address)
            .ok_or_else(|| eyre!("read post-bootstrap ZeroFee counter"))?;
        if counter_after_bootstrap.1 != 0 {
            return Err(eyre!(
                "bootstrap must not consume quota, got {counter_after_bootstrap:?}"
            ));
        }

        // Top up only after bootstrap evidence is captured; the main scenario
        // later needs enough COEN for its deliberately paid fallback call.
        eth::send_value(&self.cfg.rpc0, address, &funder_key, eth::coen(10))?;
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

    pub fn replay_zerofee_bootstrap_transaction(&self, state: &FixtureState) -> Result<()> {
        let raw = state
            .zerofee_delegation_raw
            .as_deref()
            .ok_or_else(|| eyre!("missing exact included bootstrap transaction"))?;
        let address = zerofee_address(state);
        let before_balance = eth::balance(&self.cfg.rpc0, address);
        let before_nonce = eth::nonce(&self.cfg.rpc0, address);
        let before_counter = self.zerofee_counter(address);
        let error = eth::raw_json_result(
            &self.cfg.rpc0,
            "eth_sendRawTransaction",
            serde_json::json!([raw]),
        )
        .expect_err("exact included bootstrap transaction replay unexpectedly accepted");
        if error.to_string().is_empty() {
            return Err(eyre!("bootstrap replay returned an empty RPC error"));
        }
        if eth::balance(&self.cfg.rpc0, address) != before_balance {
            return Err(eyre!("bootstrap replay changed signer balance"));
        }
        if eth::nonce(&self.cfg.rpc0, address) != before_nonce {
            return Err(eyre!("bootstrap replay changed signer nonce"));
        }
        if self.zerofee_counter(address) != before_counter {
            return Err(eyre!("bootstrap replay changed ZeroFee counter"));
        }
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
            1,
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
        // The negative lane deliberately submits several ordinary paid
        // authorization/call envelopes; funding one COEN only covered a single
        // envelope's gas reservation on the fresh-chain base fee.
        let funding = self.fund_key(funder, key, 10)?;
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

        let start_timestamp = self
            .latest_block_timestamp(self.cfg.primary_port())
            .ok_or_else(|| eyre!("read canonical timestamp before ZeroFee rollover"))?;
        let wait_budget_secs = zerofee_rollover_wait_budget_secs(start_timestamp);
        let mut reset = None;
        let mut latest_observation = None;
        for _ in 0..wait_budget_secs {
            let latest_timestamp = self.latest_block_timestamp(self.cfg.primary_port());
            let current = self.zerofee_counter(address);
            latest_observation = Some((latest_timestamp, current));
            if latest_timestamp.is_some_and(|timestamp| timestamp % 86_400 < 200)
                && current.is_some_and(|value| value.0 != before.0 && value.1 == 0)
            {
                reset = current;
                break;
            }
            sleep(Duration::from_secs(1));
        }
        let _reset = reset.ok_or_else(|| {
            eyre!(
                "ZeroFee counter did not lazily reset within {wait_budget_secs}s: \
                 start_timestamp={start_timestamp}, last={latest_observation:?}"
            )
        })?;
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

#[cfg(feature = "ocomp-integration")]
fn classify_owner_index_result(
    index: u64,
    result: std::result::Result<Vec<u8>, String>,
) -> std::result::Result<Option<Vec<u8>>, String> {
    match result {
        Ok(nod_id) if nod_id.len() == outbe_compressed_entities::WwdEntityId::len_bytes() => {
            Ok(Some(nod_id))
        }
        Ok(nod_id) => Err(format!(
            "tokenOfOwnerByIndex returned a {}-byte NOD id",
            nod_id.len()
        )),
        Err(error) if index > 0 && error.to_ascii_lowercase().contains("index out of bounds") => {
            Ok(None)
        }
        Err(error) => Err(error),
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

#[cfg(feature = "ocomp-integration")]
fn parse_rpc_word(encoded: &str) -> Option<U256> {
    U256::from_str_radix(encoded.trim_start_matches("0x"), 16).ok()
}

#[cfg(feature = "ocomp-integration")]
fn select_ocomp_job_request_log(
    logs: &[serde_json::Value],
    worldwide_day: Option<u32>,
) -> Option<&serde_json::Value> {
    logs.iter().rev().find(|log| {
        worldwide_day.is_none_or(|expected| {
            log.get("topics")
                .and_then(serde_json::Value::as_array)
                .and_then(|topics| topics.get(2))
                .and_then(serde_json::Value::as_str)
                .and_then(parse_rpc_word)
                .and_then(|word| u32::try_from(word).ok())
                == Some(expected)
        })
    })
}

#[cfg(feature = "ocomp-integration")]
fn decode_rpc_data_words(log: &serde_json::Value, expected: usize) -> Option<Vec<U256>> {
    let bytes = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
    if bytes.len() != expected.checked_mul(32)? {
        return None;
    }
    Some(
        bytes
            .chunks_exact(32)
            .map(U256::from_be_slice)
            .collect::<Vec<_>>(),
    )
}

#[cfg(feature = "ocomp-integration")]
fn rpc_log_block_number(log: &serde_json::Value) -> Option<u64> {
    u64::from_str_radix(
        log.get("blockNumber")?.as_str()?.trim_start_matches("0x"),
        16,
    )
    .ok()
}

#[cfg(feature = "ocomp-integration")]
fn canonical_rpc_log_block_hash(
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

#[cfg(feature = "ocomp-integration")]
fn decode_nod_materialization_progress(
    log: &serde_json::Value,
) -> Option<NodMaterializationProgressV1> {
    let topics = log.get("topics")?.as_array()?;
    if topics.len() != 3 {
        return None;
    }
    let topic_word = |index: usize| {
        let value = topics.get(index)?.as_str()?;
        let bytes = hex::decode(value.trim_start_matches("0x")).ok()?;
        (bytes.len() == 32).then(|| U256::from_be_slice(&bytes))
    };
    let _queue_sequence = u64::try_from(topic_word(1)?).ok()?;
    let worldwide_day = u32::try_from(topic_word(2)?).ok()?;
    let data = log
        .get("data")?
        .as_str()
        .and_then(|value| hex::decode(value.trim_start_matches("0x")).ok())?;
    if data.len() != 5 * 32 {
        return None;
    }
    let word = |index: usize| {
        let start = index * 32;
        U256::from_be_slice(&data[start..start + 32])
    };
    let completed = match word(3) {
        value if value.is_zero() => false,
        value if value == U256::from(1) => true,
        _ => return None,
    };
    Some(NodMaterializationProgressV1 {
        worldwide_day,
        generation: u64::try_from(word(0)).ok()?,
        next_nod_ordinal: u32::try_from(word(2)).ok()?,
        completed,
        block_number: u64::try_from(word(4)).ok()?,
    })
}

#[cfg(test)]
mod ocomp_tests {
    use alloy_primitives::B256;
    use outbe_primitives::reshare_artifact::{
        encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
    };

    use super::*;

    fn package(root: B256) -> CompressedEntityAtHeader {
        let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: 1,
                r_sealed: root,
            }),
            ..OutbeBlockArtifacts::default()
        })
        .unwrap();
        CompressedEntityAtHeader {
            result: PointReadResultV1::Unavailable,
            header: SelectedHeaderV1 {
                block_number: 42,
                block_hash: B256::repeat_byte(0x11),
                extra_data: extra_data.to_vec(),
            },
        }
    }

    #[test]
    fn compressed_entity_evidence_binds_transport_hash_and_header_root() {
        let first = package(B256::repeat_byte(0x22));
        let second = package(B256::repeat_byte(0x33));

        let (first_root, first_proof) = first.evidence_identity().unwrap();
        let (second_root, second_proof) = second.evidence_identity().unwrap();

        assert_eq!(first_root, B256::repeat_byte(0x22).to_string());
        assert_eq!(second_root, B256::repeat_byte(0x33).to_string());
        assert_ne!(first_root, second_root);
        assert_eq!(first_proof, second_proof);
        assert_eq!(
            first_proof,
            sha256_hex(&serde_json::to_vec(&PointReadResultV1::Unavailable).unwrap())
        );
    }

    #[test]
    fn compressed_entity_evidence_rejects_header_without_ce_root() {
        let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
        let package = CompressedEntityAtHeader {
            result: PointReadResultV1::Unavailable,
            header: SelectedHeaderV1 {
                block_number: 42,
                block_hash: B256::repeat_byte(0x11),
                extra_data: extra_data.to_vec(),
            },
        };

        assert!(package.evidence_identity().is_err());
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn job_request_selection_keeps_the_requested_day_visible_after_later_retries() {
        let day = |worldwide_day: u32| {
            serde_json::json!({
                "topics": [
                    "0x00",
                    "0x00",
                    format!("0x{worldwide_day:064x}")
                ]
            })
        };
        let requested = day(20260807);
        let later_retry = day(20260806);
        let logs = vec![requested.clone(), later_retry];

        assert_eq!(
            select_ocomp_job_request_log(&logs, Some(20260807)),
            Some(&requested)
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn materialization_progress_decodes_indexed_identity_and_nonindexed_progress() {
        let mut data = Vec::new();
        for value in [7_u64, 0, 8, 1, 42] {
            data.extend_from_slice(&U256::from(value).to_be_bytes::<32>());
        }
        let log = serde_json::json!({
            "topics": [
                format!("{:#x}", keccak256(b"NodMaterializationProgress(uint64,uint32,uint64,uint32,uint32,bool,uint64)")),
                format!("0x{:064x}", 3),
                format!("0x{:064x}", 20_260_813),
            ],
            "data": format!("0x{}", hex::encode(data)),
        });

        assert_eq!(
            decode_nod_materialization_progress(&log),
            Some(NodMaterializationProgressV1 {
                worldwide_day: 20_260_813,
                generation: 7,
                next_nod_ordinal: 8,
                completed: true,
                block_number: 42,
            })
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn materialization_stall_deadline_resets_only_for_strict_progress() {
        let started = Instant::now();
        let stall = Duration::from_secs(10);
        let mut deadline = MaterializationStallDeadline::new(started, stall);

        assert!(!deadline.observe(started + Duration::from_secs(9), 0));
        assert!(!deadline.observe(started + Duration::from_secs(9), 8));
        assert!(!deadline.observe(started + Duration::from_secs(18), 8));
        assert!(deadline.observe(started + Duration::from_secs(19), 8));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn materialization_stall_deadline_ignores_regressing_observations() {
        let started = Instant::now();
        let stall = Duration::from_secs(10);
        let mut deadline = MaterializationStallDeadline::new(started, stall);

        assert!(!deadline.observe(started + Duration::from_secs(5), 16));
        assert!(!deadline.observe(started + Duration::from_secs(9), 8));
        assert!(deadline.observe(started + Duration::from_secs(15), 16));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn first_owner_index_revert_is_not_reported_as_an_absent_nod() {
        let error = classify_owner_index_result(
            0,
            Err("execution reverted: index out of bounds".to_owned()),
        )
        .expect_err("index zero must preserve the execution failure");

        assert!(error.contains("index out of bounds"));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn second_owner_index_transport_failure_does_not_prove_uniqueness() {
        let error = classify_owner_index_result(
            1,
            Err("compressed-entity tree unavailable: exact parent mismatch".to_owned()),
        )
        .expect_err("a readiness failure must not be treated as an absent second NOD");

        assert!(error.contains("exact parent mismatch"));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn exact_second_owner_index_out_of_bounds_proves_uniqueness() {
        assert_eq!(
            classify_owner_index_result(
                1,
                Err("execution reverted: index out of bounds".to_owned()),
            )
            .expect("the canonical bounds error is an expected absence"),
            None,
        );
    }

    #[test]
    fn zerofee_rollover_wait_budget_covers_the_canonical_distance_to_boundary() {
        assert_eq!(zerofee_rollover_wait_budget_secs(1_787_615_641), 419);
        assert_eq!(zerofee_rollover_wait_budget_secs(1_787_615_950), 150);
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn encoded_reward_bearing_tribute_plaintext_preserves_waa_and_sra_beneficiaries() {
        let creator = Address::repeat_byte(0x11);
        let waa = Address::repeat_byte(0x22);
        let sra = Address::repeat_byte(0x33);
        let plaintext = encode_reward_bearing_tribute_plaintext(
            creator,
            B256::repeat_byte(0x44),
            "100",
            "0",
            B256::repeat_byte(0x55),
            &[waa],
            &[sra],
        )
        .expect("encode reward-bearing Tribute plaintext");
        let payload: serde_json::Value =
            serde_json::from_slice(&plaintext).expect("decode Tribute plaintext");

        assert_eq!(
            payload["wallet_addresses"],
            serde_json::json!([format!("{waa:#x}")])
        );
        assert_eq!(
            payload["sra_addresses"],
            serde_json::json!([format!("{sra:#x}")])
        );
    }
}
