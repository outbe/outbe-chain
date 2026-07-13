use alloy_primitives::{Address, B256, U256};
use outbe_agentreward::AgentRewardContract;
use outbe_common::WorldwideDay;
use outbe_metadosis::schema::{status, MetadosisContract, WorldwideDayEntryExt};
use outbe_oracle::{contract::OracleContract, scurve};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::protocol::{EncryptedTributeOffer, TributeOfferStatus};
use outbe_tribute::{TributeContract, TributeData};

use crate::errors::TributeFactoryError;
use crate::schema::TributeFactoryContract;

impl TributeFactoryContract<'_> {
    /// Single live offer path: an encrypted offer arrives; the host reads the
    /// current USDC/COEN oracle rate at this block, hands the offer + rate to the
    /// enclave (`ProcessTributeOfferBatch`), and issues the Tribute from the returned
    /// `TributeOfferResult` without recomputing economics or `token_id`.
    /// `worldwide_day`/`currency` are NOT ABI args — they live in the encrypted
    /// payload; the enclave reads them and they come back in the result.
    pub fn offer_tribute(
        &mut self,
        caller: Address,
        cipher_text: &[u8],
        nonce: &[u8],
        ephemeral_pubkey: U256,
        reference_currency: u16,
        exclude_from_intex_issuance: bool,
    ) -> Result<U256> {
        check_currency(reference_currency)?;

        // Current USDC/COEN rate at this block. There is a single active OFFERING
        // day, so its committed oracle price is the current rate (identical on
        // every validator).
        let metadosis = MetadosisContract::new(self.storage.clone());
        let offering_day = *metadosis
            .get_active_wwd_by_status(status::OFFERING)?
            .first()
            .ok_or_else(|| PrecompileError::Revert("no worldwide day is OFFERING".to_string()))?;
        let tribute_price =
            resolve_tribute_price(self.storage.clone(), reference_currency, offering_day)?;
        if tribute_price.is_zero() {
            return Err(TributeFactoryError::NominalPriceUnavailable {
                worldwide_day: offering_day,
            }
            .into());
        }

        // Hand the encrypted offer + rate to the enclave. It decrypts, applies the
        // rate, computes economics (U256) + Poseidon token_id, and returns the
        // public result. The host does NOT recompute economics or token_id.
        let offer = EncryptedTributeOffer {
            owner: caller,
            cipher_text: cipher_text.to_vec(),
            nonce: nonce.to_vec(),
            ephemeral_pubkey,
            reference_currency,
            exclude_from_intex_issuance,
            tribute_price_minor: tribute_price,
        };
        let results = crate::enclave_offer::process_tribute_offer_batch_via_enclave(&[offer])
            .map_err(|e| TributeFactoryError::DecryptionFailed(e.to_string()))?;
        let result = results
            .into_iter()
            .next()
            .ok_or_else(|| TributeFactoryError::DecryptionFailed("empty enclave result".into()))?;

        if let TributeOfferStatus::Rejected { reason } = &result.status {
            return Err(TributeFactoryError::DecryptionFailed(reason.clone()).into());
        }

        // The offer's (decrypted) day must be OFFERING.
        let result_day = WorldwideDay::from(result.worldwide_day);
        let wwd_status = metadosis.worldwide_days.entry(result_day).status().read()?;
        if wwd_status != status::OFFERING {
            return Err(TributeFactoryError::WorldwideDayNotOffering {
                worldwide_day: result_day,
                status: wwd_status,
            }
            .into());
        }

        let tribute_id = U256::from_be_bytes(result.token_id.0);

        let tribute = TributeContract::new(self.storage.clone());
        if tribute.get_tribute(tribute_id)?.is_some() {
            return Err(TributeFactoryError::TributeAlreadyExists.into());
        }

        let su_hashes = parse_su_hashes(&result.su_hashes)?;
        self.mark_su_hashes_used(&su_hashes)?;

        validate_agent_reward_addresses(&result.wallet_addresses, &result.sra_addresses)?;

        let mut tribute = TributeContract::new(self.storage.clone());
        tribute.issue(&TributeData {
            token_id: tribute_id,
            owner: caller,
            worldwide_day: result_day,
            issuance_amount_minor: result.issuance_amount_minor,
            issuance_currency: result.issuance_currency,
            nominal_amount_minor: result.nominal_amount_minor,
            reference_currency: result.reference_currency,
            exclude_from_intex_issuance: result.exclude_from_intex_issuance,
            tribute_price_minor: result.tribute_price_minor,
        })?;

        if !result.wallet_addresses.is_empty() && !result.sra_addresses.is_empty() {
            let mut agent_reward = AgentRewardContract::new(self.storage.clone());
            for addr_str in &result.wallet_addresses {
                let addr: Address =
                    addr_str
                        .parse()
                        .map_err(|_| TributeFactoryError::InvalidWalletAddress {
                            address: addr_str.clone(),
                        })?;
                agent_reward.increment_waa_tribute(result_day, addr)?;
            }
            for addr_str in &result.sra_addresses {
                let addr: Address =
                    addr_str
                        .parse()
                        .map_err(|_| TributeFactoryError::InvalidSraAddress {
                            address: addr_str.clone(),
                        })?;
                agent_reward.increment_sra_tribute(result_day, addr)?;
            }
        }

        Ok(tribute_id)
    }
}

// TODO implement mature checks
fn check_currency(currency: u16) -> Result<()> {
    if currency != 840 {
        return Err(PrecompileError::Revert(format!(
            "iso_code {currency} is not a valid currency"
        )));
    }
    Ok(())
}

fn parse_su_hashes(su_hashes: &[String]) -> Result<Vec<B256>> {
    su_hashes
        .iter()
        .map(|hash| {
            let hex_str = hash.strip_prefix("0x").unwrap_or(hash);
            let bytes = hex::decode(hex_str)
                .map_err(|_| TributeFactoryError::InvalidSuHashHex { hash: hash.clone() })?;
            if bytes.len() != 32 {
                return Err(TributeFactoryError::InvalidSuHashLength {
                    length: bytes.len(),
                }
                .into());
            }
            Ok(B256::from_slice(&bytes))
        })
        .collect()
}

fn resolve_tribute_price(
    storage: outbe_primitives::storage::StorageHandle,
    issuance_currency: u16,
    worldwide_day: WorldwideDay,
) -> Result<U256> {
    let oracle = OracleContract::new(storage);
    let pair_hash = oracle.settlement_iso_to_pair.read(&issuance_currency)?;
    if pair_hash.is_zero() {
        return Err(
            TributeFactoryError::IssuanceCurrencyNotRegistered { issuance_currency }.into(),
        );
    }

    let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
    if pair_id == 0 {
        return Err(TributeFactoryError::SettlementCurrencyPairNotRegistered.into());
    }

    let vwap = oracle
        .get_worldwide_day_vwap_for_pair_id(worldwide_day, pair_id)?
        .unwrap_or(U256::ZERO);
    let scurve_timestamp = worldwide_day.to_timestamp_utc();
    let max_scurve = scurve::get_max_active_scurve_value(&oracle, pair_id, scurve_timestamp)?;

    Ok(vwap.max(max_scurve))
}

pub(crate) fn validate_agent_reward_addresses(
    wallet_addresses: &[String],
    sra_addresses: &[String],
) -> Result<()> {
    let has_wallets = !wallet_addresses.is_empty();
    let has_sras = !sra_addresses.is_empty();

    if !has_wallets && !has_sras {
        return Ok(());
    }
    if !has_wallets {
        return Err(TributeFactoryError::WalletAddressesRequiredWhenSraProvided.into());
    }
    if !has_sras {
        return Err(TributeFactoryError::SraAddressesRequiredWhenWalletProvided.into());
    }

    for (i, addr) in wallet_addresses.iter().enumerate() {
        if addr.parse::<Address>().is_err() {
            return Err(TributeFactoryError::InvalidWalletAddressAtIndex {
                index: i,
                address: addr.clone(),
            }
            .into());
        }
    }
    for (i, addr) in sra_addresses.iter().enumerate() {
        if addr.parse::<Address>().is_err() {
            return Err(TributeFactoryError::InvalidSraAddressAtIndex {
                index: i,
                address: addr.clone(),
            }
            .into());
        }
    }

    Ok(())
}

// Amount normalization now lives in the enclave (`compute::normalize_amount`),
// which is the single producer of the canonical economics. The host no longer
// recomputes amounts.
