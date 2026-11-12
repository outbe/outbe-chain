//! The daemon loop: reads the HyperlaneController domains every poll, runs
//! one signer per domain, and announces the storage location on each origin
//! chain.
//!
//! The announce flow is ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/validator/src/validator.rs` (`announce`, Apache-2.0, see
//! `LICENSE-APACHE`): sign, store, check `getAnnouncedStorageLocations`, send,
//! retry with exponential backoff (30 s to 15 min). Changes: no funding
//! preflight, no gas escalator, no jitter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use eyre::Result;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::abi::IValidatorAnnounce;
use crate::checkpoint::{Announcement, Signed};
use crate::client::{eth_call, send_tx, TxSender};
use crate::config::OriginConfig;
use crate::errors::ValidatorError;
use crate::health::{Controller, ValidatorHealth};
use crate::signer::DomainSigner;
use crate::store::{Meta, Store};
use crate::sync::{check_origin, connect, controller_domains, ws_wake, ControllerDomain, Origin};

const ANNOUNCE_MIN_DELAY: Duration = Duration::from_secs(30);
const ANNOUNCE_MAX_DELAY: Duration = Duration::from_secs(900);

pub struct Daemon {
    pub origins: Vec<OriginConfig>,
    pub poll_interval: Duration,
    pub key: PrivateKeySigner,
    pub validator: Address,
    pub store: Arc<Store>,
    pub health: Arc<ValidatorHealth>,
    /// `s3+http(s)://host[:port]/<bucket>`. The domain follows.
    pub location: String,
}

/// A controller domain that this process started.
enum Domain {
    /// Dropping the tasks stops the signer, the backfill, the WebSocket
    /// waker and the announce loop.
    Running { hook: Address, _tasks: JoinSet<()> },
    /// The hook changed. The domain does not start again in this process.
    Stopped,
}

impl Daemon {
    /// Polls the controller forever. A poll error is logged and retried.
    pub async fn run(self, outbe: DynProvider) {
        let mut domains = HashMap::new();
        loop {
            if let Err(error) = self.poll(&outbe, &mut domains).await {
                warn!(error = format!("{error:#}"), "controller poll failed");
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn poll(&self, outbe: &DynProvider, domains: &mut HashMap<u32, Domain>) -> Result<()> {
        let Some(listed) = controller_domains(outbe).await? else {
            self.health.set_controller(Controller::WaitingForController);
            info!("HyperlaneController is not initialized (icaRouter() is zero); waiting");
            return Ok(());
        };
        self.health.set_controller(Controller::Ready);
        let listed_has = |domain: u32| listed.iter().any(|l| l.domain == domain);
        domains.retain(|domain, _| {
            let keep = listed_has(*domain);
            if !keep {
                info!(domain, "domain removed from the controller; its tasks stop");
            }
            keep
        });
        self.health.retain_domains(listed_has);
        for d in &listed {
            match domains.get(&d.domain) {
                Some(Domain::Running { hook, .. }) if *hook != d.hook => {
                    let message = format!(
                        "hookByDomain changed from {hook} to {}: signing of this domain stopped",
                        d.hook
                    );
                    error!(domain = d.domain, "{message}");
                    domains.insert(d.domain, Domain::Stopped);
                    self.health.set_error(d.domain, message);
                }
                Some(_) => {}
                None => match self.start(outbe, d).await {
                    Ok(domain) => {
                        domains.insert(d.domain, domain);
                    }
                    Err(error) => {
                        warn!(domain = d.domain, %error, "domain start failed; retry on the next poll");
                        self.health.set_error(d.domain, format!("{error:#}"));
                    }
                },
            }
        }
        Ok(())
    }

    async fn start(&self, outbe: &DynProvider, d: &ControllerDomain) -> Result<Domain> {
        let config = self
            .origins
            .iter()
            .find(|c| c.domain == d.domain)
            .ok_or(ValidatorError::MissingOrigin(d.domain))?;
        // The stored history is signed for one hook. Another hook must not
        // continue it.
        let stored_hook = self.store.meta(d.domain, Meta::Hook)?;
        if let Some(stored) = &stored_hook {
            if stored.as_slice() != d.hook.as_slice() {
                return Err(ValidatorError::HookMismatch {
                    domain: d.domain,
                    stored: Address::try_from(stored.as_slice())
                        .map_err(|_| ValidatorError::CorruptRecord("hook"))?,
                    current: d.hook,
                }
                .into());
            }
        }
        let origin = check_origin(d, config).await?;
        if stored_hook.is_none() {
            self.store
                .set_meta(d.domain, Meta::Hook, d.hook.as_slice())?;
        }
        let signer = Arc::new(DomainSigner::new(
            origin.clone(),
            self.store.clone(),
            self.key.clone(),
        )?);
        // A warning only: signing continues (see `history_lost`).
        if let Err(error) = signer.check_history(outbe, self.validator).await {
            warn!(domain = d.domain, %error, "submittedIndex() check failed");
        }
        let mut tasks = JoinSet::new();
        let wake = ws_wake(config, origin.hook, &mut tasks);
        tasks.spawn(signer.clone().run(config.clone(), wake));
        tasks.spawn(announce(
            origin,
            config.rpc_urls.clone(),
            self.key.clone(),
            self.store.clone(),
            format!("{}/{}", self.location, d.domain),
        ));
        self.health.set_running(signer);
        info!(domain = d.domain, hook = %d.hook, "domain started");
        Ok(Domain::Running {
            hook: d.hook,
            _tasks: tasks,
        })
    }
}

/// Signs the announcement, stores it for `announcement.json`, and sends
/// `announce` on the origin chain until `getAnnouncedStorageLocations`
/// contains `location`. The key `H` pays the gas on that chain, also on
/// Outbe: the ZeroFee policy waives the fee only for `submitCheckpoint`.
async fn announce(
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
            rpc_endpoint: url,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy_primitives::{Bytes, B256};
    use alloy_sol_types::SolValue;
    use serde_json::{json, Value};

    use super::*;
    use crate::abi::{IHyperlaneController, IMerkleTreeHook, IValidatorSet};
    use crate::liveness::{HYPERLANE_CONTROLLER_ADDRESS, VALIDATOR_SET_ADDRESS};
    use crate::sync::tests::serve_json_rpc;

    const VA: Address = Address::repeat_byte(0xa1);
    const MAILBOX: Address = Address::repeat_byte(0xb2);

    /// What the controller returns: icaRouter, domains, hook of each domain.
    type Controller = Arc<Mutex<(Address, Vec<u32>, Address)>>;

    /// One endpoint for Outbe and the origin chain of domain 5. The
    /// announcement of `location` is already on-chain.
    async fn chain(state: Controller, location: String) -> String {
        serve_json_rpc(move |req: &Value| {
            let id = req["id"].clone();
            let result = match req["method"].as_str().unwrap() {
                "eth_blockNumber" => json!("0x64"),
                "eth_chainId" => json!("0x5"),
                "eth_getLogs" => json!([]),
                "eth_call" => {
                    let call = &req["params"][0];
                    let to: Address = serde_json::from_value(call["to"].clone()).unwrap();
                    let input = if call["input"].is_null() {
                        &call["data"]
                    } else {
                        &call["input"]
                    };
                    let data: Bytes = serde_json::from_value(input.clone()).unwrap();
                    let selector: [u8; 4] = data[..4].try_into().unwrap();
                    let (ica, domains, hook) = state.lock().unwrap().clone();
                    let out = if to == HYPERLANE_CONTROLLER_ADDRESS {
                        match selector {
                            IHyperlaneController::icaRouterCall::SELECTOR => ica.abi_encode(),
                            IHyperlaneController::domainsCall::SELECTOR => domains.abi_encode(),
                            IHyperlaneController::hookByDomainCall::SELECTOR => hook.abi_encode(),
                            IHyperlaneController::validatorAnnounceByDomainCall::SELECTOR => {
                                VA.abi_encode()
                            }
                            IHyperlaneController::submittedIndexCall::SELECTOR
                            | IHyperlaneController::missCountCall::SELECTOR => 0u32.abi_encode(),
                            other => panic!("controller selector {other:?}"),
                        }
                    } else if to == VALIDATOR_SET_ADDRESS {
                        assert_eq!(selector, IValidatorSet::getActiveValidatorsCall::SELECTOR);
                        Vec::<Address>::new().abi_encode()
                    } else if to == VA {
                        match selector {
                            IValidatorAnnounce::localDomainCall::SELECTOR => 5u32.abi_encode(),
                            IValidatorAnnounce::mailboxCall::SELECTOR => MAILBOX.abi_encode(),
                            IValidatorAnnounce::getAnnouncedStorageLocationsCall::SELECTOR => {
                                vec![vec![location.clone()]].abi_encode()
                            }
                            other => panic!("VA selector {other:?}"),
                        }
                    } else {
                        match selector {
                            IMerkleTreeHook::mailboxCall::SELECTOR => MAILBOX.abi_encode(),
                            IMerkleTreeHook::deployedBlockCall::SELECTOR => {
                                alloy_primitives::U256::from(1).abi_encode()
                            }
                            IMerkleTreeHook::latestCheckpointCall::SELECTOR => {
                                (B256::ZERO, 0u32).abi_encode_params()
                            }
                            other => panic!("hook selector {other:?}"),
                        }
                    };
                    json!(Bytes::from(out))
                }
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        })
        .await
    }

    #[tokio::test]
    async fn poll_starts_stops_and_removes_domains() {
        let hook1 = Address::repeat_byte(0x11);
        let state: Controller = Arc::new(Mutex::new((Address::ZERO, vec![], hook1)));
        let url = chain(state.clone(), "s3+http://h:9000/0xb/5".into()).await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        let origin: OriginConfig = toml::from_str(&format!(
            "domain = 5\nrpc_urls = [\"{url}\"]\nreorg_period = 0\ninterval_secs = 1"
        ))
        .unwrap();
        let daemon = Daemon {
            origins: vec![origin],
            poll_interval: Duration::from_secs(1),
            key: PrivateKeySigner::random(),
            validator: Address::repeat_byte(0x77),
            store: store.clone(),
            health: health.clone(),
            location: "s3+http://h:9000/0xb".into(),
        };
        let outbe = connect(&url).unwrap();
        let mut domains = HashMap::new();
        let status = |key: &str| health.status_json()[key].clone();

        // icaRouter() is zero: wait, start nothing.
        daemon.poll(&outbe, &mut domains).await.unwrap();
        assert_eq!(status("controller"), "waiting_for_controller");
        assert!(domains.is_empty());

        // Domain 5 starts. Domain 6 has no [[origin]] block: an error.
        state.lock().unwrap().0 = Address::repeat_byte(1);
        state.lock().unwrap().1 = vec![5, 6];
        daemon.poll(&outbe, &mut domains).await.unwrap();
        assert_eq!(status("controller"), "ready");
        assert!(matches!(domains.get(&5), Some(Domain::Running { .. })));
        assert!(!domains.contains_key(&6));
        assert_eq!(health.running().len(), 1);
        let errors = status("domains");
        assert!(errors["6"]["error"]
            .as_str()
            .unwrap()
            .contains("[[origin]]"));
        assert_eq!(errors["5"]["error"], Value::Null);
        assert_eq!(
            store.meta(5, Meta::Hook).unwrap().as_deref(),
            Some(hook1.as_slice())
        );

        // The announcement is signed and stored for `announcement.json`.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let stored = loop {
            if let Some(json) = store.meta(5, Meta::Announcement).unwrap() {
                break json;
            }
            assert!(tokio::time::Instant::now() < deadline, "no announcement");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let signed: Signed<Announcement> = serde_json::from_slice(&stored).unwrap();
        assert_eq!(signed.value.storage_location, "s3+http://h:9000/0xb/5");
        assert_eq!(signed.value.mailbox_address, MAILBOX.into_word());
        assert_eq!(signed.recover().unwrap(), daemon.key.address());

        // An inactive validator does not submit.
        let wallet = EthereumWallet::from(daemon.key.clone());
        let chain = crate::config::ChainConfig {
            rpc_endpoint: url.clone(),
            chain_id: 5,
        };
        let liveness = crate::liveness::Liveness::new(
            Default::default(),
            &chain,
            wallet,
            daemon.validator,
            health.clone(),
        );
        liveness.tick().await.unwrap();
        assert_eq!(status("active"), false);
        assert_eq!(status("submitted"), 0);

        // A hook change stops the domain and its tasks. No restart.
        state.lock().unwrap().2 = Address::repeat_byte(0x22);
        daemon.poll(&outbe, &mut domains).await.unwrap();
        daemon.poll(&outbe, &mut domains).await.unwrap();
        assert!(matches!(domains.get(&5), Some(Domain::Stopped)));
        assert!(health.running().is_empty());
        let error = status("domains")["5"]["error"].clone();
        assert!(error.as_str().unwrap().contains("hookByDomain changed"));
        let signer = health.signer(5).expect("kept for the bucket");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        // Only the health map and this test hold the signer: the tasks ended.
        while Arc::strong_count(&signer) > 2 {
            assert!(tokio::time::Instant::now() < deadline, "tasks still run");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // The controller removes the domains.
        state.lock().unwrap().1 = vec![];
        daemon.poll(&outbe, &mut domains).await.unwrap();
        assert!(domains.is_empty());
        assert!(health.signer(5).is_none());
        assert_eq!(status("domains"), json!({}));
    }

    #[tokio::test]
    async fn stored_history_of_another_hook_blocks_the_start() {
        let state: Controller = Arc::new(Mutex::new((
            Address::repeat_byte(1),
            vec![5],
            Address::repeat_byte(0x22),
        )));
        let url = chain(state, "s3+http://h:9000/0xb/5".into()).await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let old_hook = Address::repeat_byte(0x11);
        store.set_meta(5, Meta::Hook, old_hook.as_slice()).unwrap();
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        let origin: OriginConfig = toml::from_str(&format!(
            "domain = 5\nrpc_urls = [\"{url}\"]\nreorg_period = 0\ninterval_secs = 1"
        ))
        .unwrap();
        let daemon = Daemon {
            origins: vec![origin],
            poll_interval: Duration::from_secs(1),
            key: PrivateKeySigner::random(),
            validator: Address::repeat_byte(0x77),
            store: store.clone(),
            health: health.clone(),
            location: "s3+http://h:9000/0xb".into(),
        };
        let mut domains = HashMap::new();
        daemon
            .poll(&connect(&url).unwrap(), &mut domains)
            .await
            .unwrap();

        assert!(domains.is_empty());
        assert!(health.running().is_empty());
        let status = health.status_json();
        let error = status["domains"]["5"]["error"].as_str().unwrap();
        assert!(error.contains("belongs to hook"), "{error}");
        assert!(error.contains("delete the database directory"), "{error}");
        assert_eq!(status["healthy"], false);
        assert_eq!(
            store.meta(5, Meta::Hook).unwrap().as_deref(),
            Some(old_hook.as_slice())
        );
    }
}
