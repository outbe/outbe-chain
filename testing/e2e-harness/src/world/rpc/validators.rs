use crate::world::rpc::*;

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

impl Rpc {
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
    pub fn is_validator(&self, port: u16, addr: &str) -> Result<bool> {
        let v: Address = addr
            .parse()
            .wrap_err_with(|| format!("parse validator address {addr}"))?;
        eth::read_call_result(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::isValidatorCall { addr: v },
        )
        .map_err(|error| eyre!("read isValidator on RPC {port}: {error}"))
    }

    /// Whether consensus should schedule another validator-set change.
    pub fn has_pending_set_change(&self, port: u16) -> Result<bool> {
        eth::read_call_result(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::hasPendingSetChangeCall {},
        )
        .map_err(|error| eyre!("read hasPendingSetChange on RPC {port}: {error}"))
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
    pub fn epoch_start_block(&self, port: u16) -> Result<u64> {
        eth::read_call_result(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::getEpochStartBlockCall {},
        )
        .map_err(|error| eyre!("read epoch start block on RPC {port}: {error}"))
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
        let tx_hash = eth::send_call_with_gas_reserve(
            &self.cfg.rpc0,
            addresses::AGENT_REWARD_ADDR,
            key,
            &IAgentReward::claimRewardCall {
                pool,
                amount: U256::ZERO,
            },
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
    pub fn has_voter_felony_event(
        &self,
        port: u16,
        validator: &str,
        from_block: u64,
    ) -> Result<bool> {
        let validator: Address = validator
            .parse()
            .wrap_err_with(|| format!("parse validator address {validator}"))?;
        let signature = keccak256("VoterFelony(address,uint64,uint64)");
        let indexed_validator = format!("0x{:0>64}", hex::encode(validator));
        let value = eth::raw_json_result(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::SLASH_ADDR),
                "fromBlock": format!("0x{from_block:x}"),
                "toBlock": "finalized",
                "topics": [format!("{signature:#x}"), indexed_validator],
            }]),
        )
        .wrap_err_with(|| format!("read finalized VoterFelony logs from RPC port {port}"))?;
        let logs = value
            .as_array()
            .ok_or_else(|| eyre!("eth_getLogs returned a non-array response on RPC port {port}"))?;
        Ok(!logs.is_empty())
    }

    /// Number of finalized evidence-felony applications for `validator` at or
    /// after `from_block`.
    pub fn evidence_felony_event_count(
        &self,
        port: u16,
        validator: &str,
        from_block: u64,
    ) -> Result<usize> {
        let validator: Address = validator
            .parse()
            .wrap_err_with(|| format!("parse validator address {validator}"))?;
        let signature = keccak256("EvidenceFelonyApplied(address,address,uint256,uint256)");
        let indexed_validator = format!("0x{:0>64}", hex::encode(validator));
        let value = eth::raw_json_result(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::SLASH_ADDR),
                "fromBlock": format!("0x{from_block:x}"),
                "toBlock": "finalized",
                "topics": [format!("{signature:#x}"), indexed_validator],
            }]),
        )
        .wrap_err_with(|| {
            format!("read finalized EvidenceFelonyApplied logs from RPC port {port}")
        })?;
        value
            .as_array()
            .map(Vec::len)
            .ok_or_else(|| eyre!("eth_getLogs returned a non-array response on RPC port {port}"))
    }

    /// Whether the validator holds a live DKG share.
    pub fn has_share(&self, port: u16, addr: &str) -> Option<bool> {
        self.validator_record(port, addr).map(|r| r.has_bls_share)
    }

    /// Whether `addr` is a current consensus participant (ACTIVE or EXITING-with-share).
    pub fn is_participant(&self, port: u16, addr: &str) -> Result<bool> {
        let v = addr
            .parse::<Address>()
            .wrap_err_with(|| format!("parse consensus participant address {addr}"))?;
        eth::read_call_result(
            &self.url(port),
            addresses::VS_ADDR,
            &IValidatorSet::isConsensusParticipantCall { addr: v },
        )
        .map_err(|error| eyre!("read consensus participation from RPC port {port}: {error}"))
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

    /// Submit the absent activation selector to verify public ABI reachability.
    pub fn submit_unsupported_activation(
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

    /// Replay the unsupported activation calldata at its finalized receipt height.
    /// The actual transaction is checked separately; this call identifies its
    /// state-independent ABI rejection reason without claiming boundary validation.
    pub fn unsupported_activation_revert_reason_at(
        &self,
        port: u16,
        caller: Address,
        requested: &[Address],
        group_hash: B256,
        height: u64,
    ) -> Result<String> {
        eth::read_call_revert_reason_at(
            &self.url(port),
            addresses::VS_ADDR,
            caller,
            &IValidatorSetRaw::activateResharedSetCall {
                newActiveSet: requested.to_vec(),
                groupPublicKey: group_hash,
            },
            height,
        )
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
    pub fn wait_participant(&self, port: u16, addr: &str, tries: u32) -> Result<bool> {
        let mut last_error = None;
        for _ in 0..tries {
            match self.is_participant(port, addr) {
                Ok(true) => return Ok(true),
                Ok(false) => last_error = None,
                Err(error) => last_error = Some(error),
            }
            sleep(Duration::from_secs(10));
        }
        if let Some(error) = last_error {
            return Err(error.wrap_err("consensus participation remained unobservable"));
        }
        Ok(false)
    }

    /// Poll until ACTIVE validator count equals `want` (10s polls).
    #[must_use = "active-validator count wait must be checked"]
    pub fn wait_active_count(&self, port: u16, want: u64, tries: u32) -> bool {
        for _ in 0..tries {
            if self.active_count(port) == Some(want) {
                return true;
            }
            sleep(Duration::from_secs(10));
        }
        false
    }
}
