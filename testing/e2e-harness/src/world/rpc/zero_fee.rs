use crate::world::rpc::*;

impl Rpc {
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
        state.zerofee_balance_before = Some(
            eth::balance_result(&self.cfg.rpc0, address)
                .wrap_err("read funded ZeroFee signer balance")?,
        );
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
        let before_balance = eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
            .wrap_err("read signer balance before sponsored replay")?;
        let before_counter = self
            .zerofee_counter(zerofee_address(state))
            .ok_or_else(|| eyre!("read counter before sponsored replay"))?;
        let error = eth::raw_json_result(
            &self.cfg.rpc0,
            "eth_sendRawTransaction",
            serde_json::json!([raw]),
        )
        .expect_err("exact included EIP-7702 transaction replay unexpectedly accepted");
        state.zerofee_replay_error = Some(error.to_string());
        assert_eq!(
            eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
                .wrap_err("read signer balance after sponsored replay")?,
            before_balance,
            "replay changed signer balance"
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state))
                .ok_or_else(|| eyre!("read counter after sponsored replay"))?,
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
        let before_balance = eth::balance_result(&self.cfg.rpc0, address)
            .wrap_err("read signer balance before bootstrap replay")?;
        let before_nonce = eth::nonce(&self.cfg.rpc0, address)
            .ok_or_else(|| eyre!("read nonce before bootstrap replay"))?;
        let before_counter = self
            .zerofee_counter(address)
            .ok_or_else(|| eyre!("read counter before bootstrap replay"))?;
        let error = eth::raw_json_result(
            &self.cfg.rpc0,
            "eth_sendRawTransaction",
            serde_json::json!([raw]),
        )
        .expect_err("exact included bootstrap transaction replay unexpectedly accepted");
        if error.to_string().is_empty() {
            return Err(eyre!("bootstrap replay returned an empty RPC error"));
        }
        if eth::balance_result(&self.cfg.rpc0, address)
            .wrap_err("read signer balance after bootstrap replay")?
            != before_balance
        {
            return Err(eyre!("bootstrap replay changed signer balance"));
        }
        if eth::nonce(&self.cfg.rpc0, address)
            .ok_or_else(|| eyre!("read nonce after bootstrap replay"))?
            != before_nonce
        {
            return Err(eyre!("bootstrap replay changed signer nonce"));
        }
        if self
            .zerofee_counter(address)
            .ok_or_else(|| eyre!("read counter after bootstrap replay"))?
            != before_counter
        {
            return Err(eyre!("bootstrap replay changed ZeroFee counter"));
        }
        self.assert_zerofee_delegation(state);
        Ok(())
    }

    pub fn assert_zerofee_persisted_on_ports(&self, state: &FixtureState, ports: &[u16]) {
        let address = zerofee_address(state);
        let expected_code = [&[0xef, 0x01, 0x00][..], addresses::ZEROFEE_ADDR.as_slice()].concat();
        let height = self
            .finalized_result(self.cfg.primary_port())
            .expect("read finalized height for preserved ZeroFee state");
        let checkpoint = self
            .wait_finalized_checkpoint(ports, height, 60)
            .expect("all ZeroFee observers agree on one finalized checkpoint");
        let (_, expected_counter, _) = self
            .zerofee_state_at(ports[0], address, checkpoint)
            .expect("read preserved ZeroFee state at the finalized checkpoint");
        let expected_balance = state
            .zerofee_balance_after_quota
            .expect("retained signer balance after quota exhaustion");
        assert_eq!(expected_counter.1, 8, "primary quota must remain exhausted");
        for &port in ports {
            let (code, counter, balance) = self
                .zerofee_state_at(port, address, checkpoint)
                .unwrap_or_else(|error| {
                    panic!("read preserved ZeroFee state on RPC {port}: {error:#}")
                });
            assert_eq!(
                code.as_ref(),
                expected_code.as_slice(),
                "delegation was not preserved on RPC port {port}"
            );
            assert_eq!(
                counter, expected_counter,
                "quota/day changed on RPC port {port}"
            );
            assert_eq!(
                balance, expected_balance,
                "delegated-account COEN balance changed on RPC port {port}"
            );
        }
    }

    /// Read coupled account and quota state at one previously finalized identity.
    pub(super) fn zerofee_state_at(
        &self,
        port: u16,
        address: Address,
        checkpoint: FinalizedCheckpoint,
    ) -> Result<(Bytes, (u32, u32), U256)> {
        ensure!(
            self.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "ZeroFee checkpoint changed before reading RPC {port}"
        );
        let url = self.url(port);
        let selector =
            serde_json::json!({"blockHash": checkpoint.block_hash, "requireCanonical": true});
        let code = serde_json::from_value(eth::raw_json_result(
            &url,
            "eth_getCode",
            serde_json::json!([address, selector]),
        )?)
        .wrap_err("decode finalized ZeroFee delegation")?;
        let balance = serde_json::from_value(eth::raw_json_result(
            &url,
            "eth_getBalance",
            serde_json::json!([address, selector]),
        )?)
        .wrap_err("decode finalized ZeroFee balance")?;
        let counter = eth::read_call_at_result(
            &url,
            addresses::ZEROFEE_ADDR,
            &IZeroFee::getCounterCall { signer: address },
            checkpoint.height,
        )
        .map_err(|error| eyre!("read finalized ZeroFee counter: {error}"))?;
        ensure!(
            self.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "ZeroFee checkpoint changed while reading RPC {port}"
        );
        Ok((code, (counter.day, counter.count), balance))
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
        state.zerofee_balance_after_quota = Some(
            eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
                .wrap_err("read signer balance after sponsored quota")?,
        );
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
            state
                .zerofee_balance_after_quota
                .expect("balance after sponsored quota"),
            state
                .zerofee_balance_before
                .expect("balance before sponsored quota"),
            "sponsored calls charged the signer"
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)).map(|v| v.1),
            Some(8)
        );
    }

    pub fn submit_zerofee_ninth(&self, state: &mut FixtureState) -> Result<()> {
        let before = eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
            .wrap_err("read signer balance before ninth call")?;
        state.zerofee_balance_after_quota = Some(before);
        state.zerofee_ninth_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_balance_after_ninth = Some(
            eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
                .wrap_err("read signer balance after ninth call")?,
        );
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
            state
                .zerofee_balance_after_ninth
                .expect("balance after ninth call"),
            state
                .zerofee_balance_after_quota
                .expect("balance before ninth call")
        );
        assert_eq!(
            self.zerofee_counter(zerofee_address(state)).map(|v| v.1),
            Some(8)
        );
    }

    pub fn submit_zerofee_paid(&self, state: &mut FixtureState) -> Result<()> {
        state.zerofee_balance_after_ninth = Some(
            eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
                .wrap_err("read signer balance before paid fallback")?,
        );
        state.zerofee_paid_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            1,
        )?);
        state.zerofee_balance_after_paid = Some(
            eth::balance_result(&self.cfg.rpc0, zerofee_address(state))
                .wrap_err("read signer balance after paid fallback")?,
        );
        Ok(())
    }

    pub fn assert_zerofee_paid(&self, state: &FixtureState) {
        let receipt = state.zerofee_paid_receipt.as_ref().expect("paid receipt");
        assert!(receipt_status(receipt), "paid fallback failed");
        assert!(
            state
                .zerofee_balance_after_paid
                .expect("balance after paid fallback")
                < state
                    .zerofee_balance_after_ninth
                    .expect("balance before paid fallback"),
            "paid fallback did not charge a fee"
        );
        let charged = state
            .zerofee_balance_after_ninth
            .expect("balance before paid fallback")
            .checked_sub(
                state
                    .zerofee_balance_after_paid
                    .expect("balance after paid fallback"),
            )
            .expect("paid fallback balance must not increase");
        assert_eq!(
            charged,
            Self::receipt_gas_cost(receipt).expect("valid paid fallback receipt fee"),
            "paid fallback balance delta differs from its exact receipt fee"
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
        state.zerofee_wrong_target_balance_before = Some(
            eth::balance_result(&self.cfg.rpc0, address)
                .wrap_err("read signer balance before wrong-target call")?,
        );
        state.zerofee_wrong_target_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            &key,
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_wrong_target_balance_after = Some(
            eth::balance_result(&self.cfg.rpc0, address)
                .wrap_err("read signer balance after wrong-target call")?,
        );
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
            state
                .zerofee_wrong_target_balance_after
                .expect("balance after wrong-target call")
                < state
                    .zerofee_wrong_target_balance_before
                    .expect("balance before wrong-target call"),
            "wrong-target call did not pay its own COEN gas charge"
        );
        let charged = state
            .zerofee_wrong_target_balance_before
            .expect("balance before wrong-target call")
            .checked_sub(
                state
                    .zerofee_wrong_target_balance_after
                    .expect("balance after wrong-target call"),
            )
            .expect("wrong-target call balance must not increase");
        assert_eq!(
            charged,
            Self::receipt_gas_cost(receipt).expect("valid wrong-target receipt fee"),
            "wrong-target balance delta differs from its exact receipt fee"
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
        state.zerofee_new_day_balance_before = Some(
            eth::balance_result(&self.cfg.rpc0, address)
                .wrap_err("read signer balance before new-day call")?,
        );
        state.zerofee_new_day_receipt = Some(eth::send_reward_call(
            &self.cfg.rpc0,
            zerofee_key(state),
            addresses::AGENT_REWARD_ADDR,
            0,
        )?);
        state.zerofee_new_day_balance_after = Some(
            eth::balance_result(&self.cfg.rpc0, address)
                .wrap_err("read signer balance after new-day call")?,
        );
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
            state
                .zerofee_new_day_balance_after
                .expect("balance after new-day call"),
            state
                .zerofee_new_day_balance_before
                .expect("balance before new-day call"),
            "first new-day sponsored call charged the signer COEN"
        );
        let outcome = TxOutcome {
            transaction_hash: receipt["transactionHash"]
                .as_str()
                .expect("new-day receipt transaction hash")
                .to_owned(),
            success: true,
            receipt: receipt.clone(),
        };
        let checkpoint = self
            .finalize_outcome(&outcome, ports, 60)
            .expect("new-day sponsored receipt is canonical and finalized on every observer");
        let (_, expected, _) = self
            .zerofee_state_at(ports[0], address, checkpoint)
            .expect("read new-day counter at the finalized receipt checkpoint");
        assert_ne!(expected.0, old_day, "worldwide day did not change");
        assert_eq!(expected.1, 1, "new-day quota must restart at one use");
        let expected_code = [&[0xef, 0x01, 0x00][..], addresses::ZEROFEE_ADDR.as_slice()].concat();
        for &port in ports {
            let (code, counter, balance) = self
                .zerofee_state_at(port, address, checkpoint)
                .unwrap_or_else(|error| {
                    panic!("read finalized new-day state on RPC {port}: {error:#}")
                });
            assert_eq!(
                counter, expected,
                "new-day quota differs on RPC port {port}"
            );
            assert_eq!(
                code.as_ref(),
                expected_code.as_slice(),
                "delegation changed across day rollover on RPC port {port}"
            );
            assert_eq!(
                balance,
                state
                    .zerofee_new_day_balance_before
                    .expect("pre-rollover call balance"),
                "finalized new-day call charged the signer on RPC port {port}"
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

pub(in crate::world::rpc) fn zerofee_rollover_wait_budget_secs(latest_timestamp: u64) -> u64 {
    const SECONDS_PER_DAY: u64 = 86_400;
    const MINIMUM_WAIT_SECONDS: u64 = 150;
    const FINALITY_SLACK_SECONDS: u64 = 60;

    let remaining = SECONDS_PER_DAY - latest_timestamp % SECONDS_PER_DAY;
    remaining
        .saturating_add(FINALITY_SLACK_SECONDS)
        .max(MINIMUM_WAIT_SECONDS)
}

pub(in crate::world::rpc) const SPONSORSHIP_TOPIC: &str =
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
