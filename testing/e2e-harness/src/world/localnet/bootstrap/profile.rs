use super::*;

/// Valid, scenario-local genesis choices used by lifecycle E2E tests.
///
/// The fields are deliberately private. Callers use the checked builders, and
/// [`Localnet::bootstrap_with_profile`] validates the values again against the
/// actual committee size before generating any files. ValidatorSet and Staking
/// overrides are written to a scenario-local seed copy that the normal genesis
/// seeder consumes. They are never installed through raw storage writes.
#[derive(Debug, Clone)]
pub struct BootstrapProfile {
    pub(super) chain_id: Option<NonZeroU64>,
    pub(super) validator_set_owner_index: Option<usize>,
    pub(super) max_validators: Option<u32>,
    pub(super) reregistration_cooldown_blocks: Option<u32>,
    pub(super) epoch_length_blocks: NonZeroU64,
    pub(super) dkg_prepare_window_blocks: NonZeroU64,
    pub(super) dkg_activation_grace_blocks: NonZeroU64,
    pub(super) dev_felony_threshold: u64,
    pub(super) unbonding_period_secs: Option<NonZeroU64>,
    pub(super) slashed_withdrawal_delay_secs: Option<NonZeroU64>,
    pub(super) validator_balance_coen: Option<u64>,
    pub(super) metadosis_forming_seconds: Option<u64>,
    pub(super) metadosis_offering_seconds: u64,
    pub(super) ocomp_vote_window_blocks: u64,
    pub(super) governance_voting_window_blocks: u64,
    pub(super) governments: bool,
    pub(super) oracle_pairs: Option<Vec<(String, String, String)>>,
    pub(super) credis_accounts: Option<(alloy_primitives::Address, alloy_primitives::Address)>,
}

impl Default for BootstrapProfile {
    fn default() -> Self {
        Self {
            chain_id: None,
            validator_set_owner_index: None,
            max_validators: None,
            reregistration_cooldown_blocks: None,
            epoch_length_blocks: NonZeroU64::new(DEFAULT_EPOCH_LENGTH_BLOCKS)
                .expect("non-zero default epoch"),
            dkg_prepare_window_blocks: NonZeroU64::new(DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
                .expect("non-zero default prepare window"),
            dkg_activation_grace_blocks: NonZeroU64::new(DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
                .expect("non-zero default activation grace"),
            dev_felony_threshold: DEV_FELONY_THRESHOLD,
            unbonding_period_secs: None,
            slashed_withdrawal_delay_secs: None,
            validator_balance_coen: None,
            metadosis_forming_seconds: None,
            metadosis_offering_seconds: LOCALNET_METADOSIS_OFFERING_SECONDS,
            ocomp_vote_window_blocks: LOCALNET_OCOMP_VOTE_WINDOW_BLOCKS,
            governance_voting_window_blocks: 6,
            governments: false,
            oracle_pairs: None,
            credis_accounts: None,
        }
    }
}

impl BootstrapProfile {
    /// Enable the network-owned repository through the normal genesis seeder.
    pub fn with_governments(mut self) -> Result<Self> {
        self.governments = true;
        self.validate_timing()?;
        Ok(self)
    }

    /// Fund only this scenario's actors and seed its user's mineable Gem.
    #[cfg(any(test, feature = "ocomp-integration"))]
    pub(crate) fn with_credis_accounts(
        mut self,
        user: alloy_primitives::Address,
        cca: alloy_primitives::Address,
    ) -> Result<Self> {
        if user.is_zero() || cca.is_zero() || user == cca {
            bail!("Credis needs distinct, nonzero user and CCA addresses");
        }
        self.credis_accounts = Some((user, cca));
        Ok(self)
    }
    /// Bind the scenario's governance deadline to genesis before identities are prepared.
    pub fn with_governance_voting_window(mut self, blocks: u64) -> Result<Self> {
        outbe_chain_constants::GenesisProtocolParametersV1::resolve(Some(&json!({
            "governance": { "votingWindowBlocks": blocks }
        })))?;
        self.governance_voting_window_blocks = blocks;
        Ok(self)
    }

    /// Replace the seed's Oracle registry through the normal genesis seeder.
    /// Each tuple is `(base, quote, initial_rate)` in configured orientation.
    pub fn with_oracle_pairs(mut self, pairs: Vec<(String, String, String)>) -> Result<Self> {
        if pairs.is_empty() {
            bail!("Oracle pair profile must not be empty");
        }
        let mut markets = HashSet::new();
        for (base, quote, initial_rate) in &pairs {
            if base.trim().is_empty() || quote.trim().is_empty() || base.eq_ignore_ascii_case(quote)
            {
                bail!("invalid Oracle pair {base}/{quote}");
            }
            if quote.eq_ignore_ascii_case("COEN")
                && base
                    .parse::<u16>()
                    .is_ok_and(|code| (1..=999).contains(&code))
            {
                bail!("Oracle COEN/ISO pair must use COEN base and ISO quote");
            }
            if initial_rate.is_empty() || !initial_rate.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("Oracle initial rate must be an unsigned decimal integer");
            }
            let mut market = [base.to_ascii_lowercase(), quote.to_ascii_lowercase()];
            market.sort();
            if !markets.insert(market) {
                bail!("duplicate or inverse Oracle pair {base}/{quote}");
            }
        }
        self.oracle_pairs = Some(pairs);
        Ok(self)
    }

    /// Override the EIP-155 chain identity. Zero is rejected.
    pub fn with_chain_id(mut self, chain_id: u64) -> Result<Self> {
        self.chain_id =
            Some(NonZeroU64::new(chain_id).ok_or_else(|| eyre!("chain id must be > 0"))?);
        Ok(self)
    }

    /// Make an existing genesis validator the ValidatorSet owner.
    ///
    /// The index is checked against the requested committee size at bootstrap.
    pub fn with_validator_set_owner(mut self, validator_index: usize) -> Self {
        self.validator_set_owner_index = Some(validator_index);
        self
    }

    /// Override the registry capacity. It must cover the genesis committee.
    pub fn with_max_validators(mut self, max_validators: u32) -> Result<Self> {
        if max_validators == 0 {
            bail!("max validators must be > 0");
        }
        self.max_validators = Some(max_validators);
        Ok(self)
    }

    /// Override the re-registration cooldown. Zero intentionally means no
    /// cooldown and is a valid acceleration setting.
    pub fn with_reregistration_cooldown_blocks(mut self, blocks: u32) -> Self {
        self.reregistration_cooldown_blocks = Some(blocks);
        self
    }

    /// Set the positive periodic epoch/DKG schedule.
    pub fn with_dkg_timing(
        mut self,
        epoch_length_blocks: u64,
        prepare_window_blocks: u64,
        activation_grace_blocks: u64,
    ) -> Result<Self> {
        self.epoch_length_blocks = positive(epoch_length_blocks, "epoch length")?;
        self.dkg_prepare_window_blocks = positive(prepare_window_blocks, "DKG prepare window")?;
        self.dkg_activation_grace_blocks =
            positive(activation_grace_blocks, "DKG activation grace")?;
        if prepare_window_blocks > epoch_length_blocks {
            bail!(
                "DKG prepare window {prepare_window_blocks} exceeds epoch length \
                 {epoch_length_blocks}"
            );
        }
        Ok(self)
    }

    /// Set the localnet felony threshold. It may be zero, but must remain below
    /// the epoch length so the per-epoch reset cannot hide a test offense.
    pub fn with_dev_felony_threshold(mut self, blocks: u64) -> Result<Self> {
        if blocks >= self.epoch_length_blocks.get() {
            bail!(
                "dev felony threshold {blocks} must be < epoch length {}",
                self.epoch_length_blocks
            );
        }
        self.dev_felony_threshold = blocks;
        Ok(self)
    }

    /// Set positive Staking delays through the public genesis seed schema.
    pub fn with_staking_timing(
        mut self,
        unbonding_period_secs: Option<u64>,
        slashed_withdrawal_delay_secs: Option<u64>,
    ) -> Result<Self> {
        self.unbonding_period_secs = unbonding_period_secs
            .map(|value| positive(value, "unbonding period"))
            .transpose()?;
        self.slashed_withdrawal_delay_secs = slashed_withdrawal_delay_secs
            .map(|value| positive(value, "slashed withdrawal delay"))
            .transpose()?;
        Ok(self)
    }

    /// Parse the legacy `TESTNET_*` slice used by existing feature steps.
    ///
    /// Parsing is strict: malformed or unsupported knobs fail before bootstrap
    /// instead of silently falling back to a different genesis.
    pub(crate) fn from_tuning(tuning: &[(&str, String)]) -> Result<Self> {
        let mut profile = Self::default();
        for (key, value) in tuning {
            let parsed = || {
                value
                    .parse::<u64>()
                    .wrap_err_with(|| format!("{key} must be an unsigned integer"))
            };
            match *key {
                "TESTNET_EPOCH_LENGTH_BLOCKS" => {
                    profile.epoch_length_blocks = positive(parsed()?, "epoch length")?;
                }
                "TESTNET_DKG_PREPARE_WINDOW_BLOCKS" => {
                    profile.dkg_prepare_window_blocks = positive(parsed()?, "DKG prepare window")?;
                }
                "TESTNET_DKG_ACTIVATION_GRACE_BLOCKS" => {
                    profile.dkg_activation_grace_blocks =
                        positive(parsed()?, "DKG activation grace")?;
                }
                "TESTNET_DEV_FELONY_THRESHOLD" => {
                    profile.dev_felony_threshold = parsed()?;
                }
                "TESTNET_UNBONDING_PERIOD_SECS" => {
                    profile.unbonding_period_secs = Some(positive(parsed()?, "unbonding period")?);
                }
                "TESTNET_SLASHED_WITHDRAWAL_DELAY_SECS" => {
                    profile.slashed_withdrawal_delay_secs =
                        Some(positive(parsed()?, "slashed withdrawal delay")?);
                }
                "TESTNET_VALIDATOR_BALANCE_COEN" => {
                    profile.validator_balance_coen = Some(parsed()?);
                }
                "TESTNET_METADOSIS_FORMING_SECONDS" => {
                    profile.metadosis_forming_seconds = Some(parsed()?);
                }
                "TESTNET_METADOSIS_OFFERING_SECONDS" => {
                    profile.metadosis_offering_seconds = parsed()?;
                }
                "TESTNET_OCOMP_VOTE_WINDOW_BLOCKS" => {
                    profile.ocomp_vote_window_blocks = parsed()?;
                }
                other => {
                    bail!("unsupported localnet tuning key {other}");
                }
            }
        }
        profile.validate_timing()?;
        Ok(profile)
    }

    pub(super) fn validate_timing(&self) -> Result<()> {
        if self.governments {
            outbe_primitives::radicle_genesis::validate_publication_window(
                self.epoch_length_blocks.get(),
                self.dkg_prepare_window_blocks.get(),
            )?;
        }
        if self.dkg_prepare_window_blocks > self.epoch_length_blocks {
            bail!(
                "DKG prepare window {} exceeds epoch length {}",
                self.dkg_prepare_window_blocks,
                self.epoch_length_blocks
            );
        }
        if self.epoch_length_blocks.get() > u64::from(u32::MAX) {
            bail!("epoch length exceeds the on-chain u32 range");
        }
        if self.dev_felony_threshold >= self.epoch_length_blocks.get() {
            bail!(
                "dev felony threshold {} must be < epoch length {}",
                self.dev_felony_threshold,
                self.epoch_length_blocks
            );
        }
        Ok(())
    }

    pub(super) fn validate_for_committee(&self, n: usize) -> Result<()> {
        self.validate_timing()?;
        if n == 0 {
            bail!("bootstrap committee must contain at least one validator");
        }
        if n > u32::MAX as usize {
            bail!("bootstrap committee size exceeds u32 range");
        }
        if let Some(index) = self.validator_set_owner_index {
            if index >= n {
                bail!("ValidatorSet owner index {index} is outside committee size {n}");
            }
        }
        if let Some(max) = self.max_validators {
            if max < n as u32 {
                bail!("max validators {max} is smaller than genesis committee size {n}");
            }
        }
        Ok(())
    }
}

fn positive(value: u64, label: &str) -> Result<NonZeroU64> {
    NonZeroU64::new(value).ok_or_else(|| eyre!("{label} must be > 0"))
}
