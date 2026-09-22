use crate::internal::l2_fixture::{self, TributeOfferZk};
use crate::world::rpc::*;

pub struct TributeZkOffer<'a> {
    pub tribute_draft_id_hex: &'a str,
    pub su_hash_hex: &'a str,
    pub merkle_root_hex: &'a str,
    pub proof_hex: &'a str,
    pub l2_chain_id: u32,
    pub circuit_version: &'a str,
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
    ///
    /// The offer carries a real proof for exactly these fields, its caller, this
    /// chain's id and its own draft; the encrypted payload the CLI builds uses
    /// the same draft fields, so the enclave's `nft_hash` matches the proof.
    pub fn tribute_offer_with_params(
        &self,
        key: &str,
        wwd: &str,
        amount_base: &str,
        amount_micro: &str,
        currency: u16,
        exclude_from_intex_issuance: bool,
    ) -> Option<String> {
        let caller = eth::address_of(key)?;
        let l2_chain_id = self.l2_chain_by_l1_address(caller)?;
        self.tribute_offer_for_network_with_params(
            key,
            l2_chain_id,
            wwd,
            amount_base,
            amount_micro,
            currency,
            exclude_from_intex_issuance,
        )
    }

    /// Submit through a registered network independently of the caller's own
    /// operator mapping, preserving the real caller-bound proof and CLI path.
    #[allow(clippy::too_many_arguments)]
    pub fn tribute_offer_for_network_with_params(
        &self,
        key: &str,
        l2_chain_id: u64,
        wwd: &str,
        amount_base: &str,
        amount_micro: &str,
        currency: u16,
        exclude_from_intex_issuance: bool,
    ) -> Option<String> {
        let started = Instant::now();
        let caller = eth::address_of(key)?;
        let worldwide_day = wwd.parse::<u32>().expect("numeric worldwide day");
        let (draft_id, su_hash) = l2_fixture::offer_identifiers("cli-offer", caller, worldwide_day);
        let zk = self.prove_offer_for_network(
            caller,
            l2_chain_id,
            worldwide_day,
            currency,
            (amount_base, amount_micro),
            draft_id,
            su_hash,
        );
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
            "--amount-micro".to_owned(),
            amount_micro.to_owned(),
            "--currency".to_owned(),
            currency.to_string(),
            "--tribute-draft-id".to_owned(),
            zk.tribute_draft_id_hex.clone(),
            "--su-hash".to_owned(),
            zk.su_hash_hex.clone(),
            "--zk-proof".to_owned(),
            zk.proof_hex(),
            "--zk-merkle-root".to_owned(),
            zk.merkle_root_hex(),
            "--signature".to_owned(),
            zk.signature_hex(),
            "--l2-chain-id".to_owned(),
            zk.l2_chain_id.to_string(),
            "--circuit-version".to_owned(),
            zk.circuit_version.to_owned(),
        ];
        if exclude_from_intex_issuance {
            args.push("--exclude-from-intex-issuance".to_owned());
        }
        let out = self.sh().cli(args.iter().map(String::as_str)).ok()?;
        let tx_hash = parse::extract_tx_hash(&out)?;
        eprintln!(
            "E2E_TRIBUTE_TIMELINE stage=submitted wall_ms={} cli_elapsed_ms={} tx={tx_hash} owner={} wwd={wwd} amount_base={amount_base} amount_micro={amount_micro} currency={currency} exclude={exclude_from_intex_issuance} l2_chain_id={}",
            unix_time_millis(),
            started.elapsed().as_millis(),
            self.address_of(key).unwrap_or_else(|| "unknown".to_owned()),
            zk.l2_chain_id,
        );
        Some(tx_hash)
    }

    /// Submit one real encrypted Tribute whose enclave result attributes one
    /// WAA and one SRA beneficiary. This is a harness-only producer for the
    /// existing public ABI; production reward accounting remains unchanged.
    ///
    /// The offer is ZK-verified: the proof is bound to the caller, this chain,
    /// the day, the issuance currency, the declared amounts and exactly the
    /// draft and SU hash the enclave decrypts.
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
        let (tribute_draft_id, su_hash) =
            l2_fixture::offer_identifiers("agent-reward-tribute", creator, worldwide_day);
        // The proof and the encrypted payload must declare the same claim, so
        // both are built from these literals.
        const AMOUNT_BASE: &str = "100";
        const AMOUNT_MICRO: &str = "0";
        let zk = self.prove_offer(
            creator,
            worldwide_day,
            840,
            (AMOUNT_BASE, AMOUNT_MICRO),
            tribute_draft_id,
            su_hash,
        );
        let plaintext = encode_reward_bearing_tribute_plaintext(
            creator,
            tribute_draft_id,
            AMOUNT_BASE,
            AMOUNT_MICRO,
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
            zkProof: zk.proof.into(),
            chainId: zk.l2_chain_id,
            version: zk.circuit_version.to_owned(),
            zkPublicKey: Bytes::new(),
            zkMerkleRoot: zk.merkle_root.to_vec().into(),
            signature: zk.signature.into(),
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
            "E2E_TRIBUTE_TIMELINE stage=agent-reward-submitted wall_ms={} tx={} owner={creator:#x} wwd={worldwide_day} waa={} sra={} l2_chain_id={}",
            unix_time_millis(),
            outcome.transaction_hash,
            wallet_addresses.len(),
            sra_addresses.len(),
            zk.l2_chain_id,
        );
        Some(outcome.transaction_hash)
    }

    /// Submit one real encrypted Tribute while keeping issuance and reference
    /// currencies independent. The product CLI intentionally remains the
    /// same-currency operator path; this narrow E2E helper exercises the
    /// already-public ABI axis without adding a new product surface.
    ///
    /// The proof binds the issuance currency (the reference currency is not part
    /// of the TributeDraft claim) and the declared amounts, so the golden
    /// nominal/reference price this scenario asserts comes from a fully
    /// ZK-verified offer.
    #[allow(clippy::too_many_arguments)]
    pub fn tribute_cross_currency_offer(
        &self,
        key: &str,
        wwd: &str,
        amount_base: &str,
        amount_micro: &str,
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
        let (tribute_draft_id, su_hash) =
            l2_fixture::offer_identifiers("cross-currency-tribute", creator, worldwide_day);
        let zk = self.prove_offer(
            creator,
            worldwide_day,
            tribute_currency,
            (amount_base, amount_micro),
            tribute_draft_id,
            su_hash,
        );
        let plaintext = serde_json::to_vec(&serde_json::json!({
            "creator": format!("{creator:?}"),
            "tribute_draft_id": format!("{tribute_draft_id:#x}"),
            "amount_base": amount_base,
            "amount_micro": amount_micro,
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
            zkProof: zk.proof.into(),
            chainId: zk.l2_chain_id,
            version: zk.circuit_version.to_owned(),
            zkPublicKey: Bytes::new(),
            zkMerkleRoot: zk.merkle_root.to_vec().into(),
            signature: zk.signature.into(),
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
            "E2E_TRIBUTE_TIMELINE stage=cross-currency-submitted wall_ms={} tx={} owner={creator:#x} wwd={worldwide_day} tribute_currency={tribute_currency} reference_currency={reference_currency} l2_chain_id={}",
            unix_time_millis(),
            outcome.transaction_hash,
            zk.l2_chain_id,
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

    /// Prove one real Demo Tribute offer for `caller`'s registered L2 fixture
    /// network: the proof is bound to `caller`, this host chain's id, the
    /// selected L2 chain id, the offer's day, currency, amounts and draft, and
    /// its Merkle root is signed with the key that network registered.
    fn prove_offer(
        &self,
        caller: Address,
        worldwide_day: u32,
        tribute_currency: u16,
        (amount_base, amount_micro): (&str, &str),
        draft_id: B256,
        su_hash: B256,
    ) -> TributeOfferZk {
        let l2_chain_id = self
            .l2_chain_by_l1_address(caller)
            .expect("read the offering operator's L2Registry entry");
        assert_ne!(
            l2_chain_id, 0,
            "offer fixtures register the operator's L2 network before offering: {caller:#x}"
        );
        self.prove_offer_for_network(
            caller,
            l2_chain_id,
            worldwide_day,
            tribute_currency,
            (amount_base, amount_micro),
            draft_id,
            su_hash,
        )
    }

    /// A caller-bound Demo Tribute proof for a selected registered network. Network
    /// ownership and the user submitting the Tribute are independent identities.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prove_offer_for_network(
        &self,
        caller: Address,
        l2_chain_id: u64,
        worldwide_day: u32,
        tribute_currency: u16,
        (amount_base, amount_micro): (&str, &str),
        draft_id: B256,
        su_hash: B256,
    ) -> TributeOfferZk {
        assert_ne!(
            l2_chain_id, 0,
            "selected fixture network must be registered"
        );
        let (_, registered_key) = self.l2_network(l2_chain_id).expect("selected L2 network");
        assert_eq!(
            registered_key,
            l2_fixture::root_signing_public_key(l2_chain_id)
        );
        let host_chain_id = self
            .chain_id(self.cfg.primary_port())
            .expect("read the chain id the offer executes on");
        l2_fixture::prove_tribute_offer(l2_fixture::TributeOfferStatement {
            host_chain_id,
            caller,
            l2_chain_id,
            worldwide_day: u64::from(worldwide_day),
            tribute_currency,
            amount_base,
            amount_micro,
            draft_id,
            su_hash,
        })
    }

    /// Submit a Tribute offer with an explicit circuit selector and L2 zk
    /// fields (`0x`-hex).
    ///
    /// Proofs and circuit selectors are mandatory. Invalid signatures may still
    /// be submitted to exercise the node's admission checks.
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
            "--l2-chain-id".to_owned(),
            zk.l2_chain_id.to_string(),
            "--circuit-version".to_owned(),
            zk.circuit_version.to_owned(),
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
    amount_micro: &str,
    su_hash: B256,
    wallet_addresses: &[Address],
    sra_addresses: &[Address],
) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "creator": format!("{creator:#x}"),
        "tribute_draft_id": format!("{tribute_draft_id:#x}"),
        "amount_base": amount_base,
        "amount_micro": amount_micro,
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
