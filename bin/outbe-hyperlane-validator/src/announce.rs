//! Announces the storage location of one domain on its origin chain.
//!
//! The announce flow is ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/validator/src/validator.rs` (`announce`, Apache-2.0, see
//! `LICENSE-APACHE`): sign, store, check `getAnnouncedStorageLocations`, send,
//! retry with exponential backoff (30 s to 15 min). Changes: no funding
//! preflight, no gas escalator, no jitter.

use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use eyre::Result;
use tracing::{error, info, warn};

use crate::abi::IValidatorAnnounce;
use crate::checkpoint::{Announcement, Signed};
use crate::client::{eth_call, send_tx, TxSender};
use crate::store::{Meta, Store};
use crate::sync::{connect, Origin};

const ANNOUNCE_MIN_DELAY: Duration = Duration::from_secs(30);
const ANNOUNCE_MAX_DELAY: Duration = Duration::from_secs(900);

/// Signs the announcement, stores it for `announcement.json`, and sends
/// `announce` on the origin chain until `getAnnouncedStorageLocations`
/// contains `location`. The key `H` pays the gas on that chain, also on
/// Outbe: the ZeroFee policy waives the fee only for `submitCheckpoint`.
pub async fn announce(
    origin: Origin,
    rpc_urls: Vec<String>,
    key: PrivateKeySigner,
    store: Arc<Store>,
    location: String,
) {
    let domain = origin.domain;
    let value = Announcement {
        validator: key.address(),
        mailbox_address: origin.mailbox.into_word(),
        mailbox_domain: domain,
        storage_location: location.clone(),
    };
    let signed = match Signed::sign(value, &key) {
        Ok(signed) => signed,
        Err(error) => return error!(domain, %error, "announcement signing failed"),
    };
    let stored = serde_json::to_vec_pretty(&signed)
        .map_err(eyre::Report::from)
        .and_then(|json| store.set_meta(domain, Meta::Announcement, &json));
    if let Err(error) = stored {
        warn!(domain, %error, "announcement store write failed");
    }
    let wallet = EthereumWallet::from(key.clone());
    let mut delay = ANNOUNCE_MIN_DELAY;
    loop {
        match try_announce(&origin, &rpc_urls, &signed, &wallet).await {
            Ok(true) => return info!(domain, location, "storage location is announced"),
            Ok(false) => {}
            Err(error) => warn!(
                domain,
                signer = %key.address(),
                %error,
                "announce failed; the signer needs native gas on this chain"
            ),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(ANNOUNCE_MAX_DELAY);
    }
}

/// True when the location is on-chain. Otherwise sends `announce` through
/// the first endpoint that answers the read.
async fn try_announce(
    origin: &Origin,
    rpc_urls: &[String],
    signed: &Signed<Announcement>,
    wallet: &EthereumWallet,
) -> Result<bool> {
    let mut last_error = None;
    for url in rpc_urls {
        let provider = connect(url)?;
        match announced(&provider, origin.validator_announce, &signed.value).await {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        }
        let calldata = IValidatorAnnounce::announceCall {
            validator: signed.value.validator,
            storageLocation: signed.value.storage_location.clone(),
            signature: signed.signature_bytes().to_vec().into(),
        }
        .abi_encode();
        let sender = TxSender {
            provider: &provider,
            wallet,
            chain_id: provider.get_chain_id().await?,
            gasless: false,
        };
        let tx_hash = send_tx(&sender, origin.validator_announce, &calldata).await?;
        info!(domain = origin.domain, %tx_hash, "announce transaction sent");
        return Ok(false);
    }
    Err(last_error.unwrap_or_else(|| eyre::eyre!("no rpc_urls")))
}

async fn announced(provider: &DynProvider, va: Address, value: &Announcement) -> Result<bool> {
    let call = IValidatorAnnounce::getAnnouncedStorageLocationsCall {
        validators: vec![value.validator],
    };
    let out = eth_call(provider, va, call.abi_encode(), BlockId::latest()).await?;
    let locations = IValidatorAnnounce::getAnnouncedStorageLocationsCall::abi_decode_returns(&out)?;
    Ok(locations
        .first()
        .is_some_and(|list| list.contains(&value.storage_location)))
}
