use super::*;
use outbe_primitives::storage::StorageHandle;

pub(super) struct PreparedOffer {
    pub offer: EncryptedTributeOffer,
    pub host_chain_id: u64,
    pub public: TributePublicInputs,
    pub verification_key: &'static [u8],
    pub zk_proof: Bytes,
    pub worldwide_day: WorldwideDay,
}

pub(super) fn prepare(
    storage: &StorageHandle<'_>,
    input: OfferTributeInput,
) -> Result<PreparedOffer> {
    validate_currency_code(input.tribute_currency)?;
    validate_currency_code(input.reference_currency)?;
    let (host_chain_id, public, verification_key) = verified_proof(storage, &input)?;
    let pricing = pricing(storage, &input)?;
    let zk_context = Some(TributeZkContext {
        derived_owner: public.derived_owner,
        chain_id: host_chain_id,
        l2_chain_id: u64::from(input.l2_chain_id),
    });

    // The caller remains bound to the proof. The enclave encrypts the
    // resulting amounts and binds the creator's owner/day identity.
    let offer = EncryptedTributeOffer {
        owner: input.caller,
        cipher_text: input.cipher_text.to_vec(),
        nonce: input.nonce.to_vec(),
        ephemeral_pubkey: input.ephemeral_pubkey,
        worldwide_day: input.worldwide_day,
        tribute_currency: input.tribute_currency,
        reference_currency: input.reference_currency,
        exclude_from_intex_issuance: input.exclude_from_intex_issuance,
        issuance_wwd_vwap_minor: pricing.issuance_wwd_vwap_minor,
        reference_wwd_vwap_minor: pricing.reference_wwd_vwap_minor,
        reference_scurve_minor: pricing.reference_scurve_minor,
        zk_context,
    };
    Ok(PreparedOffer {
        offer,
        host_chain_id,
        public,
        verification_key,
        zk_proof: input.zk_proof,
        worldwide_day: input.worldwide_day,
    })
}

fn verified_proof(
    storage: &StorageHandle<'_>,
    input: &OfferTributeInput,
) -> Result<(u64, TributePublicInputs, &'static [u8])> {
    // Every offer requires a registered L2 chain, a valid root signature,
    // and a proof under that chain's selected circuit, regardless of caller.
    let zk_check = outbe_l2registry::api::check_zk_merkle_root_signature(
        storage.clone(),
        u64::from(input.l2_chain_id),
        &input.zk_merkle_root,
        &input.signature,
    )?;
    let host_chain_id = storage.chain_id()?;
    let (public, verification_key) = match zk_check {
        outbe_l2registry::api::ZkOfferCheck::Verified { .. } => {
            if input.zk_proof.is_empty() {
                return Err(TributeFactoryError::ZkProofRequired.into());
            }
            let verification_key =
                resolve_verification_key(host_chain_id, input.l2_chain_id, &input.circuit_version)?;
            let public = decode_zk_public_inputs(&input.zk_proof, verification_key)?;
            if public.merkle_root.as_slice() != input.zk_merkle_root.as_ref() {
                return Err(TributeFactoryError::ZkPublicInputMismatch {
                    field: "merkle_root",
                }
                .into());
            }
            (public, verification_key)
        }
        outbe_l2registry::api::ZkOfferCheck::NotRegistered => {
            return Err(
                outbe_l2registry::errors::L2RegistryError::NetworkNotRegistered {
                    chain_id: u64::from(input.l2_chain_id),
                }
                .into(),
            );
        }
    };

    Ok((host_chain_id, public, verification_key))
}

fn pricing(
    storage: &StorageHandle<'_>,
    input: &OfferTributeInput,
) -> Result<outbe_oracle::api::TributePricingInputs> {
    // The code below settles everything from chain state before it contacts the
    // enclave, so a bad day or an unpriceable currency costs no round trip.
    if !input.worldwide_day.is_valid() {
        return Err(TributeFactoryError::InvalidWorldwideDay {
            worldwide_day: input.worldwide_day,
        }
        .into());
    }
    if !outbe_metadosis::api::is_offering_day(storage.clone(), input.worldwide_day)? {
        let status = outbe_metadosis::api::worldwide_day(storage.clone(), input.worldwide_day)?
            .map(|projection| projection.status);
        return Err(TributeFactoryError::WorldwideDayNotOffering {
            worldwide_day: input.worldwide_day,
            // Preserve the established diagnostic byte without granting
            // TributeFactory raw Metadosis schema access.
            status: status.map_or(u8::MAX, |status| status as u8),
        }
        .into());
    }

    outbe_oracle::api::check_reference_currency_with_storage(
        storage.clone(),
        input.reference_currency,
    )?;

    // Price the tribute against its own day, not whichever day happens to be
    // first in the OFFERING list.
    let pricing = outbe_oracle::api::tribute_pricing_inputs(
        storage.clone(),
        input.tribute_currency,
        input.reference_currency,
        input.worldwide_day,
    )?
    .ok_or(TributeFactoryError::IssuanceCurrencyNotRegistered {
        issuance_currency: input.tribute_currency,
    })?;
    if pricing.issuance_wwd_vwap_minor.is_zero() || pricing.reference_wwd_vwap_minor.is_zero() {
        return Err(TributeFactoryError::NominalPriceUnavailable {
            worldwide_day: input.worldwide_day,
        }
        .into());
    }

    Ok(pricing)
}
