use crate::world::rpc::*;

impl Rpc {
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
    pub fn scheduled_update(&self, id: u64) -> Result<ScheduledUpdate> {
        self.scheduled_update_on_url(&self.cfg.rpc0, id)
    }

    /// Scheduled update tuple for `id` on the node at `port`.
    pub fn scheduled_update_on(&self, port: u16, id: u64) -> Result<ScheduledUpdate> {
        self.scheduled_update_on_url(&self.url(port), id)
    }

    fn scheduled_update_on_url(&self, rpc_url: &str, id: u64) -> Result<ScheduledUpdate> {
        let r = eth::read_call_result(
            rpc_url,
            addresses::UPDATE_ADDR,
            &IUpdate::getScheduledUpdateCall {
                proposalId: U256::from(id),
            },
        )
        .map_err(|error| eyre!("read scheduled update {id} at {rpc_url}: {error}"))?;
        Ok(ScheduledUpdate {
            version: r.version as u64,
            activation: r.activationHeight,
            status: r.status as u64,
        })
    }

    /// Prove that `getScheduledUpdate(id)` rejected with the protocol's exact
    /// not-found reason. Transport, decode, and unrelated reverts stay errors.
    pub fn scheduled_update_absent_on(&self, port: u16, id: u64) -> Result<bool> {
        match eth::read_call_result(
            &self.url(port),
            addresses::UPDATE_ADDR,
            &IUpdate::getScheduledUpdateCall {
                proposalId: U256::from(id),
            },
        ) {
            Ok(_) => Ok(false),
            Err(error) if error.contains("scheduled update not found") => Ok(true),
            Err(error) => Err(eyre!(
                "scheduled update {id} absence was not observable on RPC port {port}: {error}"
            )),
        }
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
    pub fn vote_status(&self, id: u64) -> Result<VoteStatus> {
        self.vote_status_on_url(&self.cfg.rpc0, id)
    }

    /// Parsed `outbe-cli vote status` from the node at `port`.
    pub fn vote_status_on(&self, port: u16, id: u64) -> Result<VoteStatus> {
        self.vote_status_on_url(&self.url(port), id)
    }

    fn vote_status_on_url(&self, rpc_url: &str, id: u64) -> Result<VoteStatus> {
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
            .wrap_err_with(|| format!("read proposal {id} through outbe-cli at {rpc_url}"))?;
        Ok(parse::parse_vote_status(&out, id))
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
    ) -> Result<Vec<u64>> {
        let signature = keccak256(signature.as_bytes());
        let indexed_id = format!("0x{proposal_id:064x}");
        let value = eth::raw_json_result(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{address:#x}"),
                "fromBlock": "0x0",
                "toBlock": "finalized",
                "topics": [format!("{signature:#x}"), indexed_id],
            }]),
        )
        .wrap_err_with(|| format!("read finalized proposal events from RPC {port}"))?;
        let logs = value
            .as_array()
            .ok_or_else(|| eyre!("eth_getLogs on RPC {port} returned a non-array result"))?;
        logs.iter()
            .map(|log| {
                let encoded = log
                    .get("blockNumber")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| eyre!("proposal event on RPC {port} has no blockNumber"))?;
                u64::from_str_radix(encoded.trim_start_matches("0x"), 16).wrap_err_with(|| {
                    format!("decode proposal event block {encoded} on RPC {port}")
                })
            })
            .collect()
    }

    pub fn proposal_approved_event_blocks(&self, port: u16, proposal_id: u64) -> Result<Vec<u64>> {
        self.proposal_event_blocks(
            port,
            addresses::VOTE_ADDR,
            "ProposalApproved(uint256,(uint64,uint64))",
            proposal_id,
        )
    }

    pub fn scheduled_update_created_event_blocks(
        &self,
        port: u16,
        proposal_id: u64,
    ) -> Result<Vec<u64>> {
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

    /// Wait until proposal `id` reports `status=want`.
    #[must_use = "proposal status wait must be checked"]
    pub fn wait_vote_status(&self, id: u64, want: &str, tries: u32) -> Result<bool> {
        let mut observed = false;
        let mut last_error = None;
        for _ in 0..tries {
            match self.vote_status(id) {
                Ok(status) => {
                    observed = true;
                    if status.status == want {
                        return Ok(true);
                    }
                }
                Err(error) => last_error = Some(error),
            }
            sleep(Duration::from_secs(3));
        }
        if observed {
            Ok(false)
        } else {
            Err(last_error.unwrap_or_else(|| eyre!("proposal {id} was never observable")))
        }
    }

    /// Wait until the active protocol version equals `want`.
    #[must_use = "protocol-version wait must be checked"]
    pub fn wait_active_version(&self, want: u64, tries: u32) -> Option<u64> {
        self.wait_active_version_on(self.cfg.primary_port(), want, tries)
    }

    /// Wait until one validator reports the requested active protocol version.
    #[must_use = "protocol-version wait must be checked"]
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
}
