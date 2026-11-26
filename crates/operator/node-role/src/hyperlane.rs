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
        let (hash, submitted) = CliTransaction {
            key: request.validator_key,
            rpc_url: request.rpc_url,
            args: &[
                "validator",
                "delegate",
                "hyperlane",
                &format!("{hyperlane:#x}"),
            ],
            drop_privileges: true,
            bin_dir: None,
        }
        .submit_once(state.delegate_transaction.as_deref())?;
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
        let (hash, submitted) = CliTransaction {
            key: request.hyperlane_key,
            rpc_url: request.rpc_url,
            args: &[
                "validator",
                "set-hyperlane-signer",
                &format!("{hyperlane:#x}"),
            ],
            drop_privileges: true,
            bin_dir: None,
        }
        .submit_once(state.signer_transaction.as_deref())?;
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

/// Existing RPC client, signing keys, and signer funding amount for [`ensure_setup_via_rpc`].
pub struct RpcSetupRequest<'a> {
    pub rpc: &'a RpcClient,
    pub validator_key: &'a Path,
    pub hyperlane_key: &'a Path,
    pub funding: U256,
}

/// Perform Hyperlane delegate, funding, and signer registration directly over `rpc`
/// with a cooperative deadline that never cancels an in-flight transaction broadcast.
///
/// Returns `Ok((state, true))` when all steps complete, or `Ok((state, false))` when
/// `deadline` expires (with any newly broadcast transaction hash already journaled).
pub async fn ensure_setup_via_rpc(
    request: RpcSetupRequest<'_>,
    state: SetupState,
    deadline: Option<Instant>,
    progress: &mut Progress,
    save: impl FnMut(&SetupState) -> Result<()>,
) -> Result<(SetupState, bool)> {
    let target = SetupTarget {
        validator: validator_address(request.validator_key)?,
        hyperlane: validator_address(request.hyperlane_key)?,
        funding: request.funding,
    };
    let transport = RealHyperlaneRpc {
        rpc: request.rpc,
        validator_key: request.validator_key,
        hyperlane_key: request.hyperlane_key,
    };
    CooperativeSetup {
        transport: &transport,
        target,
        state,
        save,
        deadline,
        progress,
    }
    .run()
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

/// Validator, Hyperlane signer, and signer funding amount that the setup brings on chain.
#[derive(Clone, Copy, Debug)]
struct SetupTarget {
    validator: Address,
    hyperlane: Address,
    funding: U256,
}

/// One on-chain step of the Hyperlane setup, in execution order.
#[derive(Clone, Copy, Debug)]
enum SetupStep {
    Delegate,
    Fund,
    RegisterSigner,
}

impl SetupStep {
    const ALL: [Self; 3] = [Self::Delegate, Self::Fund, Self::RegisterSigner];

    const fn stage(self) -> &'static str {
        match self {
            Self::Delegate => "delegating Hyperlane operations to the Hyperlane signer",
            Self::Fund => "funding the Hyperlane signer",
            Self::RegisterSigner => "registering the Hyperlane checkpoint signer",
        }
    }

    const fn not_visible_error(self) -> &'static str {
        match self {
            Self::Delegate => "Hyperlane delegation is not visible after finality",
            Self::Fund => "Hyperlane signer funding is not visible after finality",
            Self::RegisterSigner => "Hyperlane signer registration is not visible after finality",
        }
    }

    fn journal_slot(self, state: &mut SetupState) -> &mut Option<String> {
        match self {
            Self::Delegate => &mut state.delegate_transaction,
            Self::Fund => &mut state.funding_transaction,
            Self::RegisterSigner => &mut state.signer_transaction,
        }
    }

    /// Signer registration applies only to an active validator (status 2).
    async fn needed<T: HyperlaneSetupTransport>(
        self,
        transport: &T,
        target: &SetupTarget,
    ) -> Result<bool> {
        if matches!(self, Self::RegisterSigner)
            && transport.validator(target.validator).await?.status != 2
        {
            return Ok(false);
        }
        Ok(!self.is_done(transport, target).await?)
    }

    async fn is_done<T: HyperlaneSetupTransport>(
        self,
        transport: &T,
        target: &SetupTarget,
    ) -> Result<bool> {
        Ok(match self {
            Self::Delegate => {
                transport.validator_delegate(target.validator).await? == target.hyperlane
            }
            Self::Fund => !transport.balance(target.hyperlane).await?.is_zero(),
            Self::RegisterSigner => {
                transport.hyperlane_signer(target.validator).await? == target.hyperlane
            }
        })
    }

    async fn send<T: HyperlaneSetupTransport>(
        self,
        transport: &T,
        target: &SetupTarget,
    ) -> Result<String> {
        match self {
            Self::Delegate => transport.send_delegate(target.hyperlane).await,
            Self::Fund => {
                transport
                    .send_funding(target.hyperlane, target.funding)
                    .await
            }
            Self::RegisterSigner => transport.send_set_signer(target.hyperlane).await,
        }
    }
}

/// One deadline-bounded run of the Hyperlane setup. `save` journals `state` after each new
/// broadcast so that a retry reuses the hash and never broadcasts the step again.
struct CooperativeSetup<'a, T, S> {
    transport: &'a T,
    target: SetupTarget,
    state: SetupState,
    save: S,
    deadline: Option<Instant>,
    progress: &'a mut Progress,
}

impl<T, S> CooperativeSetup<'_, T, S>
where
    T: HyperlaneSetupTransport,
    S: FnMut(&SetupState) -> Result<()>,
{
    /// Returns the journaled state and true when all steps are complete. The flag is false when
    /// the deadline expires first.
    async fn run(mut self) -> Result<(SetupState, bool)> {
        for step in SetupStep::ALL {
            if self.expired() {
                return Ok((self.state, false));
            }
            if !step.needed(self.transport, &self.target).await? {
                continue;
            }
            self.progress.stage(step.stage());
            if !self.complete_step(step).await? {
                return Ok((self.state, false));
            }
        }
        Ok((self.state, true))
    }

    fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    /// Returns false when the deadline expires before the step transaction is final.
    async fn complete_step(&mut self, step: SetupStep) -> Result<bool> {
        let Some(hash) = self.step_transaction(step).await? else {
            return Ok(false);
        };
        if !self
            .transport
            .wait_finalized(&hash, self.deadline, self.progress)
            .await?
        {
            return Ok(false);
        }
        ensure!(
            step.is_done(self.transport, &self.target).await?,
            "{}",
            step.not_visible_error()
        );
        Ok(true)
    }

    /// Reuse the journaled hash, or broadcast the step once and journal the new hash. Returns
    /// `None` when the deadline expires before a broadcast.
    async fn step_transaction(&mut self, step: SetupStep) -> Result<Option<String>> {
        if let Some(existing) = step.journal_slot(&mut self.state) {
            return Ok(Some(existing.clone()));
        }
        if self.expired() {
            return Ok(None);
        }
        // Let an in-flight broadcast finish and journal its hash before honoring deadline.
        let hash = step.send(self.transport, &self.target).await?;
        *step.journal_slot(&mut self.state) = Some(hash.clone());
        (self.save)(&self.state)?;
        Ok(Some(hash))
    }
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
        let (hash, submitted) = CliTransaction {
            key: &hyperlane_key,
            rpc_url: &rpc_url,
            args: &["validator", "set-hyperlane-signer", &format!("{signer:#x}")],
            drop_privileges: false,
            bin_dir,
        }
        .submit_once(state.signer_transaction.as_deref())?;
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

/// One `outbe-cli` transaction command signed with `key`.
struct CliTransaction<'a> {
    key: &'a Path,
    rpc_url: &'a str,
    args: &'a [&'a str],
    /// Run `outbe-cli` as the `outbe` user through `runuser`.
    drop_privileges: bool,
    /// Directory of `outbe-cli`. The default is the Outbe install root.
    bin_dir: Option<&'a Path>,
}

impl CliTransaction<'_> {
    /// Reuse the `existing` journaled hash, or run the command once and parse its transaction
    /// hash. The flag is true only for a new broadcast.
    fn submit_once(&self, existing: Option<&str>) -> Result<(String, bool)> {
        if let Some(hash) = existing {
            return Ok((hash.to_owned(), false));
        }
        let binary = self
            .bin_dir
            .unwrap_or_else(|| Path::new(process::ROOT))
            .join("outbe-cli");
        let binary = path_text(&binary)?;
        let mut full = vec![
            "--rpc-url",
            self.rpc_url,
            "--private-key-file",
            path_text(self.key)?,
        ];
        full.extend_from_slice(self.args);
        let output = if self.drop_privileges {
            let mut runuser = vec!["--user", "outbe", "--", binary];
            runuser.extend(full);
            process::capture("runuser", &runuser)?
        } else {
            process::capture(binary, &full)?
        };
        Ok((transaction_hash(&output)?, true))
    }
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
mod tests;
