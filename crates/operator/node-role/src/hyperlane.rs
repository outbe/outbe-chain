use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use alloy_consensus::{SignableTransaction as _, TxLegacy};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, Signature, TxKind, U256};
use eyre::{ensure, Context, Result};
use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    path_text,
    process::{self, HYPERLANE_STATE_ROOT},
    progress::Progress,
    rpc::{
        encode_set_delegate, encode_set_hyperlane_signer, RpcClient, ValidatorRecord,
        HYPERLANE_CONTROLLER, VALIDATOR_SET,
    },
    validator_address,
    wait::{wait_transaction_finalized, wait_transaction_finalized_until},
};

const HYPERLANE_DELEGATE_ROLE: u8 = 2;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupState {
    #[serde(default)]
    pub delegate_transaction: Option<String>,
    #[serde(default)]
    pub funding_transaction: Option<String>,
    #[serde(default)]
    pub signer_transaction: Option<String>,
}

pub(crate) struct SetupRequest<'a> {
    pub(crate) rpc_url: &'a str,
    pub(crate) validator_key: &'a Path,
    pub(crate) hyperlane_key: &'a Path,
    pub(crate) funding: U256,
}

pub(crate) async fn ensure_setup(
    request: SetupRequest<'_>,
    mut state: SetupState,
    progress: &mut Progress,
    mut save: impl FnMut(&SetupState) -> Result<()>,
) -> Result<SetupState> {
    let rpc = RpcClient::new(request.rpc_url)?;
    let validator = validator_address(request.validator_key)?;
    let hyperlane = validator_address(request.hyperlane_key)?;

    if rpc
        .validator_delegate(validator, HYPERLANE_DELEGATE_ROLE)
        .await?
        != hyperlane
    {
        progress.stage("delegating Hyperlane operations to the Hyperlane signer");
        let (hash, submitted) = transaction(
            state.delegate_transaction.as_deref(),
            request.validator_key,
            request.rpc_url,
            &[
                "validator",
                "delegate",
                "hyperlane",
                &format!("{hyperlane:#x}"),
            ],
            true,
        )?;
        if submitted {
            state.delegate_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            rpc.validator_delegate(validator, HYPERLANE_DELEGATE_ROLE)
                .await?
                == hyperlane,
            "Hyperlane delegation is not visible after finality"
        );
    }

    if rpc.balance(hyperlane).await?.is_zero() {
        progress.stage("funding the Hyperlane signer");
        let (hash, submitted) = match state.funding_transaction.as_deref() {
            Some(existing) => (existing.to_owned(), false),
            None => (
                send_value_transfer(&rpc, request.validator_key, hyperlane, request.funding)
                    .await?,
                true,
            ),
        };
        if submitted {
            state.funding_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            !rpc.balance(hyperlane).await?.is_zero(),
            "Hyperlane signer funding is not visible after finality"
        );
    }

    let record = rpc.validator(validator).await?;
    if record.status == 2 && rpc.hyperlane_signer(validator).await? != hyperlane {
        progress.stage("registering the Hyperlane checkpoint signer");
        let (hash, submitted) = transaction(
            state.signer_transaction.as_deref(),
            request.hyperlane_key,
            request.rpc_url,
            &[
                "validator",
                "set-hyperlane-signer",
                &format!("{hyperlane:#x}"),
            ],
            true,
        )?;
        if submitted {
            state.signer_transaction = Some(hash.clone());
            save(&state)?;
        }
        wait_transaction_finalized(&rpc, &hash, progress).await?;
        ensure!(
            rpc.hyperlane_signer(validator).await? == hyperlane,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    Ok(state)
}

/// Perform Hyperlane delegate, funding, and signer registration directly over `rpc`
/// with a cooperative deadline that never cancels an in-flight transaction broadcast.
///
/// Returns `Ok((state, true))` when all steps complete, or `Ok((state, false))` when
/// `deadline` expires (with any newly broadcast transaction hash already journaled).
#[allow(clippy::too_many_arguments)]
pub async fn ensure_setup_via_rpc(
    rpc: &RpcClient,
    validator_key: &Path,
    hyperlane_key: &Path,
    funding: U256,
    state: SetupState,
    deadline: Option<Instant>,
    progress: &mut Progress,
    save: impl FnMut(&SetupState) -> Result<()>,
) -> Result<(SetupState, bool)> {
    let validator = validator_address(validator_key)?;
    let hyperlane = validator_address(hyperlane_key)?;
    let transport = RealHyperlaneRpc {
        rpc,
        validator_key,
        hyperlane_key,
    };
    ensure_setup_cooperative(
        &transport, validator, hyperlane, funding, state, deadline, progress, save,
    )
    .await
}

trait HyperlaneSetupTransport {
    fn validator_delegate(&self, validator: Address) -> impl Future<Output = Result<Address>>;
    fn balance(&self, address: Address) -> impl Future<Output = Result<U256>>;
    fn validator(&self, validator: Address) -> impl Future<Output = Result<ValidatorRecord>>;
    fn hyperlane_signer(&self, validator: Address) -> impl Future<Output = Result<Address>>;
    fn send_delegate(&self, hyperlane: Address) -> impl Future<Output = Result<String>>;
    fn send_funding(
        &self,
        hyperlane: Address,
        funding: U256,
    ) -> impl Future<Output = Result<String>>;
    fn send_set_signer(&self, hyperlane: Address) -> impl Future<Output = Result<String>>;
    fn wait_finalized(
        &self,
        hash: &str,
        deadline: Option<Instant>,
        progress: &mut Progress,
    ) -> impl Future<Output = Result<bool>>;
}

struct RealHyperlaneRpc<'a> {
    rpc: &'a RpcClient,
    validator_key: &'a Path,
    hyperlane_key: &'a Path,
}

impl HyperlaneSetupTransport for RealHyperlaneRpc<'_> {
    async fn validator_delegate(&self, validator: Address) -> Result<Address> {
        self.rpc
            .validator_delegate(validator, HYPERLANE_DELEGATE_ROLE)
            .await
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        self.rpc.balance(address).await
    }

    async fn validator(&self, validator: Address) -> Result<ValidatorRecord> {
        self.rpc.validator(validator).await
    }

    async fn hyperlane_signer(&self, validator: Address) -> Result<Address> {
        self.rpc.hyperlane_signer(validator).await
    }

    async fn send_delegate(&self, hyperlane: Address) -> Result<String> {
        let data = encode_set_delegate(HYPERLANE_DELEGATE_ROLE, hyperlane);
        send_signed_tx(
            self.rpc,
            self.validator_key,
            VALIDATOR_SET,
            data,
            U256::ZERO,
        )
        .await
    }

    async fn send_funding(&self, hyperlane: Address, funding: U256) -> Result<String> {
        send_signed_tx(self.rpc, self.validator_key, hyperlane, Vec::new(), funding).await
    }

    async fn send_set_signer(&self, hyperlane: Address) -> Result<String> {
        let data = encode_set_hyperlane_signer(hyperlane);
        send_signed_tx(
            self.rpc,
            self.hyperlane_key,
            HYPERLANE_CONTROLLER,
            data,
            U256::ZERO,
        )
        .await
    }

    async fn wait_finalized(
        &self,
        hash: &str,
        deadline: Option<Instant>,
        progress: &mut Progress,
    ) -> Result<bool> {
        wait_transaction_finalized_until(self.rpc, hash, deadline, progress).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn ensure_setup_cooperative<T: HyperlaneSetupTransport>(
    transport: &T,
    validator: Address,
    hyperlane: Address,
    funding: U256,
    mut state: SetupState,
    deadline: Option<Instant>,
    progress: &mut Progress,
    mut save: impl FnMut(&SetupState) -> Result<()>,
) -> Result<(SetupState, bool)> {
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);

    if expired() {
        return Ok((state, false));
    }
    if transport.validator_delegate(validator).await? != hyperlane {
        progress.stage("delegating Hyperlane operations to the Hyperlane signer");
        let hash = match state.delegate_transaction.as_deref() {
            Some(existing) => existing.to_owned(),
            None => {
                if expired() {
                    return Ok((state, false));
                }
                // Let an in-flight broadcast finish and journal its hash before honoring deadline.
                let hash = transport.send_delegate(hyperlane).await?;
                state.delegate_transaction = Some(hash.clone());
                save(&state)?;
                hash
            }
        };
        if !transport.wait_finalized(&hash, deadline, progress).await? {
            return Ok((state, false));
        }
        ensure!(
            transport.validator_delegate(validator).await? == hyperlane,
            "Hyperlane delegation is not visible after finality"
        );
    }

    if expired() {
        return Ok((state, false));
    }
    if transport.balance(hyperlane).await?.is_zero() {
        progress.stage("funding the Hyperlane signer");
        let hash = match state.funding_transaction.as_deref() {
            Some(existing) => existing.to_owned(),
            None => {
                if expired() {
                    return Ok((state, false));
                }
                // Let an in-flight broadcast finish and journal its hash before honoring deadline.
                let hash = transport.send_funding(hyperlane, funding).await?;
                state.funding_transaction = Some(hash.clone());
                save(&state)?;
                hash
            }
        };
        if !transport.wait_finalized(&hash, deadline, progress).await? {
            return Ok((state, false));
        }
        ensure!(
            !transport.balance(hyperlane).await?.is_zero(),
            "Hyperlane signer funding is not visible after finality"
        );
    }

    if expired() {
        return Ok((state, false));
    }
    let record = transport.validator(validator).await?;
    if record.status == 2 && transport.hyperlane_signer(validator).await? != hyperlane {
        progress.stage("registering the Hyperlane checkpoint signer");
        let hash = match state.signer_transaction.as_deref() {
            Some(existing) => existing.to_owned(),
            None => {
                if expired() {
                    return Ok((state, false));
                }
                // Let an in-flight broadcast finish and journal its hash before honoring deadline.
                let hash = transport.send_set_signer(hyperlane).await?;
                state.signer_transaction = Some(hash.clone());
                save(&state)?;
                hash
            }
        };
        if !transport.wait_finalized(&hash, deadline, progress).await? {
            return Ok((state, false));
        }
        ensure!(
            transport.hyperlane_signer(validator).await? == hyperlane,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    Ok((state, true))
}

/// Sign and broadcast a plain native value transfer from `key_path` to `to` via `eth_sendRawTransaction`.
pub async fn send_value_transfer(
    rpc: &RpcClient,
    key_path: &Path,
    to: Address,
    amount: U256,
) -> Result<String> {
    send_signed_tx(rpc, key_path, to, Vec::new(), amount).await
}

async fn send_signed_tx(
    rpc: &RpcClient,
    key_path: &Path,
    to: Address,
    data: Vec<u8>,
    value: U256,
) -> Result<String> {
    let from = validator_address(key_path)?;
    let signing_key = load_signing_key(key_path)?;
    let chain_id = rpc.chain_id().await?;
    let nonce = rpc.nonce(from).await?;
    let base_fee = rpc.latest_base_fee().await?;
    let gas_price = base_fee
        .saturating_mul(U256::from(2))
        .max(U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE));
    let gas_price: u128 = gas_price
        .try_into()
        .map_err(|_| eyre::eyre!("gas price exceeds u128"))?;
    let gas_estimate = rpc.estimate_gas(from, to, &data, value).await?;
    let gas_limit = gas_estimate
        .checked_add(gas_estimate / 5)
        .ok_or_else(|| eyre::eyre!("gas limit overflow"))?;

    let tx = TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price,
        gas_limit,
        to: TxKind::Call(to),
        value,
        input: data.into(),
    };
    let hash = tx.signature_hash();
    let (sig, rec_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing_key
        .sign_prehash(hash.as_slice())
        .wrap_err("sign transaction")?;
    let signature =
        Signature::from_bytes_and_parity(sig.to_bytes().as_slice(), rec_id.to_byte() != 0)
            .normalized_s();
    let signed = tx.into_signed(signature);
    let mut raw = Vec::with_capacity(signed.encode_2718_len());
    signed.encode_2718(&mut raw);
    rpc.send_raw_transaction(&raw).await
}

fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let encoded = Zeroizing::new(
        fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read EVM key: {}", path.display()))?,
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(
        encoded.trim().strip_prefix("0x").unwrap_or(encoded.trim()),
        key.as_mut(),
    )
    .wrap_err("EVM key is not 32-byte hex")?;
    SigningKey::from_slice(key.as_ref()).wrap_err("invalid EVM signing key")
}

/// Joining validators become active only after the background DKG activation.
/// Keep the service alive, register `H` at that boundary, then replace this
/// process with the actual daemon.
pub async fn run_service(bin_dir: Option<&Path>) -> Result<()> {
    let wait_for_active = std::env::var_os("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE").is_some()
        || std::env::var("OUTBE_NODE_ROLE").as_deref()
            == Ok(crate::NodeRole::JoiningValidator.as_str());
    if !wait_for_active {
        return super::service::hyperlane(bin_dir);
    }
    let rpc_url = super::service::local_rpc_url()?;
    let rpc = RpcClient::new(&rpc_url)?;
    let validator = required_env("OUTBE_VALIDATOR_ADDRESS")?.parse()?;
    let hyperlane_key = std::env::var_os("OUTBE_HYPERLANE_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(process::ROOT).join("keys/hyperlane-key.hex"));
    ensure!(
        hyperlane_key.is_file(),
        "Hyperlane key does not name a file: {}",
        hyperlane_key.display()
    );
    let signer = validator_address(&hyperlane_key)?;
    let mut progress = Progress::new();

    loop {
        if rpc.validator(validator).await?.status == 2 {
            break;
        }
        progress.update("waiting for validator activation before Hyperlane registration");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    if rpc.hyperlane_signer(validator).await? != signer {
        progress.stage("registering the Hyperlane checkpoint signer");
        let state_path = std::env::var_os("STATE_DIRECTORY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(HYPERLANE_STATE_ROOT))
            .join("setup.json");
        let mut state: SetupState = if state_path.is_file() {
            serde_json::from_slice(&fs::read(&state_path)?)?
        } else {
            SetupState::default()
        };
        let (hash, submitted) = transaction_with_bin_dir(
            state.signer_transaction.as_deref(),
            &hyperlane_key,
            &rpc_url,
            &["validator", "set-hyperlane-signer", &format!("{signer:#x}")],
            false,
            bin_dir,
        )?;
        if submitted {
            state.signer_transaction = Some(hash.clone());
            save_state(&state_path, &state)?;
        }
        wait_transaction_finalized(&rpc, &hash, &mut progress).await?;
        ensure!(
            rpc.hyperlane_signer(validator).await? == signer,
            "Hyperlane signer registration is not visible after finality"
        );
    }
    progress.done("Hyperlane signer is registered");
    super::service::hyperlane(bin_dir)
}

pub async fn setup_founder() -> Result<()> {
    let rpc_url = super::service::local_rpc_url()?;
    let validator_key = required_path("OUTBE_VALIDATOR_KEY_PATH")?;
    let hyperlane_key = required_path("OUTBE_HYPERLANE_KEY_PATH")?;
    let funding = required_env("OUTBE_HYPERLANE_FUNDING_WEI")?
        .parse::<U256>()
        .wrap_err("OUTBE_HYPERLANE_FUNDING_WEI must be an integer")?;
    let state_path = Path::new(HYPERLANE_STATE_ROOT).join("setup.json");
    let state = if state_path.is_file() {
        serde_json::from_slice(&fs::read(&state_path)?)?
    } else {
        SetupState::default()
    };
    let mut progress = Progress::new();
    let state = ensure_setup(
        SetupRequest {
            rpc_url: &rpc_url,
            validator_key: &validator_key,
            hyperlane_key: &hyperlane_key,
            funding,
        },
        state,
        &mut progress,
        |state| save_state(&state_path, state),
    )
    .await?;
    save_state(&state_path, &state)?;
    progress.done("Hyperlane signer is delegated, funded, and registered");
    Ok(())
}

fn transaction(
    existing: Option<&str>,
    key: &Path,
    rpc_url: &str,
    args: &[&str],
    drop_privileges: bool,
) -> Result<(String, bool)> {
    transaction_with_bin_dir(existing, key, rpc_url, args, drop_privileges, None)
}

fn transaction_with_bin_dir(
    existing: Option<&str>,
    key: &Path,
    rpc_url: &str,
    args: &[&str],
    drop_privileges: bool,
    bin_dir: Option<&Path>,
) -> Result<(String, bool)> {
    if let Some(hash) = existing {
        return Ok((hash.to_owned(), false));
    }
    let binary = bin_dir
        .unwrap_or_else(|| Path::new(process::ROOT))
        .join("outbe-cli");
    let binary = path_text(&binary)?;
    let mut full = vec!["--rpc-url", rpc_url, "--private-key-file", path_text(key)?];
    full.extend_from_slice(args);
    let output = if drop_privileges {
        let mut runuser = vec!["--user", "outbe", "--", binary];
        runuser.extend(full);
        process::capture("runuser", &runuser)?
    } else {
        process::capture(binary, &full)?
    };
    Ok((transaction_hash(&output)?, true))
}

fn save_state(path: &Path, state: &SetupState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    process::write_file(path, &bytes, 0o600)
}

fn transaction_hash(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .find(|word| {
            word.len() == 66
                && word.starts_with("0x")
                && word[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .ok_or_else(|| eyre::eyre!("command did not return a transaction hash: {output}"))
}

fn required_env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| eyre::eyre!("missing environment variable: {name}"))
}

fn required_path(name: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(required_env(name)?);
    ensure!(path.is_file(), "{name} does not name a file");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use alloy_primitives::address;

    use super::*;

    struct FakeTransport {
        hyperlane: Address,
        funded: AtomicBool,
        signer_set: AtomicBool,
        funding_sends: AtomicUsize,
        send_delay: Duration,
    }

    impl HyperlaneSetupTransport for FakeTransport {
        async fn validator_delegate(&self, _validator: Address) -> Result<Address> {
            Ok(self.hyperlane)
        }

        async fn balance(&self, _address: Address) -> Result<U256> {
            if self.funded.load(Ordering::SeqCst) {
                Ok(U256::from(1_000_u64))
            } else {
                Ok(U256::ZERO)
            }
        }

        async fn validator(&self, _validator: Address) -> Result<ValidatorRecord> {
            Ok(ValidatorRecord {
                registered: true,
                status: 2,
                has_bls_share: true,
            })
        }

        async fn hyperlane_signer(&self, _validator: Address) -> Result<Address> {
            if self.signer_set.load(Ordering::SeqCst) {
                Ok(self.hyperlane)
            } else {
                Ok(Address::ZERO)
            }
        }

        async fn send_delegate(&self, _hyperlane: Address) -> Result<String> {
            unreachable!("delegate is already set in fixture")
        }

        async fn send_funding(&self, _hyperlane: Address, _funding: U256) -> Result<String> {
            self.funding_sends.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.send_delay).await;
            Ok("0x1111111111111111111111111111111111111111111111111111111111111111".into())
        }

        async fn send_set_signer(&self, _hyperlane: Address) -> Result<String> {
            self.signer_set.store(true, Ordering::SeqCst);
            Ok("0x2222222222222222222222222222222222222222222222222222222222222222".into())
        }

        async fn wait_finalized(
            &self,
            hash: &str,
            deadline: Option<Instant>,
            _progress: &mut Progress,
        ) -> Result<bool> {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(false);
            }
            if hash.starts_with("0x1111") {
                self.funded.store(true, Ordering::SeqCst);
            }
            Ok(true)
        }
    }

    #[tokio::test]
    async fn cooperative_deadline_journals_in_flight_send_and_retry_does_not_resend() {
        let validator = address!("1000000000000000000000000000000000000001");
        let hyperlane = address!("2000000000000000000000000000000000000002");
        let transport = FakeTransport {
            hyperlane,
            funded: AtomicBool::new(false),
            signer_set: AtomicBool::new(false),
            funding_sends: AtomicUsize::new(0),
            send_delay: Duration::from_millis(60),
        };
        let mut journaled: Option<SetupState> = None;
        let mut progress = Progress::new();

        // Deadline expires while `send_funding` is in flight: broadcast must finish and journal.
        let short_deadline = Some(Instant::now() + Duration::from_millis(20));
        let (state, completed) = ensure_setup_cooperative(
            &transport,
            validator,
            hyperlane,
            U256::from(1_000_u64),
            SetupState::default(),
            short_deadline,
            &mut progress,
            |next| {
                journaled = Some(next.clone());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(!completed, "must report timeout after in-flight send");
        assert_eq!(transport.funding_sends.load(Ordering::SeqCst), 1);
        assert!(
            state.funding_transaction.is_some(),
            "in-flight funding hash must be returned"
        );
        assert_eq!(
            journaled
                .as_ref()
                .and_then(|s| s.funding_transaction.as_deref()),
            state.funding_transaction.as_deref(),
            "in-flight funding hash must be journaled via save"
        );

        // Retry with the journaled state must NOT send a second funding transaction.
        let generous_deadline = Some(Instant::now() + Duration::from_secs(5));
        let (final_state, completed) = ensure_setup_cooperative(
            &transport,
            validator,
            hyperlane,
            U256::from(1_000_u64),
            state,
            generous_deadline,
            &mut progress,
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert!(completed);
        assert_eq!(
            transport.funding_sends.load(Ordering::SeqCst),
            1,
            "retry must reuse the journaled funding hash without rebroadcasting"
        );
        assert!(final_state.signer_transaction.is_some());
    }
}
