use crate::world::rpc::*;

pub struct TributeZkOffer<'a> {
    pub tribute_draft_id_hex: &'a str,
    pub su_hash_hex: &'a str,
    pub merkle_root_hex: &'a str,
    pub proof_hex: &'a str,
    pub signature_hex: &'a str,
}

impl Rpc {
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
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn encode_reward_bearing_tribute_plaintext(
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
