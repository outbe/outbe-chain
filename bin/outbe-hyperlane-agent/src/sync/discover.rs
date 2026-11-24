//! Domain discovery: the domains and their contracts from the
//! HyperlaneController, checked against the origin chain.

use alloy_eips::BlockId;
use alloy_primitives::Address;
use alloy_provider::Provider;
use alloy_sol_types::SolCall;
use eyre::Result;

use super::{connect, Origin};
use crate::abi::{IHyperlaneController, IMerkleTreeHook, IValidatorAnnounce};
use crate::client::eth_call;
use crate::config::OriginConfig;
use crate::errors::SyncError;
use crate::liveness::HYPERLANE_CONTROLLER_ADDRESS;

/// One domain of the HyperlaneController.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerDomain {
    pub domain: u32,
    pub hook: Address,
    pub validator_announce: Address,
}

/// Reads the domains and their contracts from the HyperlaneController.
/// Returns `None` while `icaRouter()` is zero: the controller is not
/// initialized yet.
pub async fn controller_domains(outbe: &impl Provider) -> Result<Option<Vec<ControllerDomain>>> {
    let call =
        |data: Vec<u8>| eth_call(outbe, HYPERLANE_CONTROLLER_ADDRESS, data, BlockId::latest());
    let out = call(IHyperlaneController::icaRouterCall {}.abi_encode()).await?;
    if IHyperlaneController::icaRouterCall::abi_decode_returns(&out)? == Address::ZERO {
        return Ok(None);
    }
    let out = call(IHyperlaneController::domainsCall {}.abi_encode()).await?;
    let mut domains = Vec::new();
    for domain in IHyperlaneController::domainsCall::abi_decode_returns(&out)? {
        let out = call(IHyperlaneController::hookByDomainCall { domain }.abi_encode()).await?;
        let hook = IHyperlaneController::hookByDomainCall::abi_decode_returns(&out)?;
        let out = call(IHyperlaneController::validatorAnnounceByDomainCall { domain }.abi_encode())
            .await?;
        let validator_announce =
            IHyperlaneController::validatorAnnounceByDomainCall::abi_decode_returns(&out)?;
        domains.push(ControllerDomain {
            domain,
            hook,
            validator_announce,
        });
    }
    Ok(Some(domains))
}

/// Checks the contracts of a controller domain on its origin chain and
/// returns the origin.
pub async fn check_origin(d: &ControllerDomain, config: &OriginConfig) -> Result<Origin> {
    let (domain, hook, va) = (d.domain, d.hook, d.validator_announce);
    let provider = connect(&config.rpc_urls[0])?;
    let call = |to: Address, data: Vec<u8>| eth_call(&provider, to, data, BlockId::latest());
    let mismatch = |what: String| SyncError::OriginMismatch { domain, what };
    let out = call(va, IValidatorAnnounce::localDomainCall {}.abi_encode()).await?;
    let local = IValidatorAnnounce::localDomainCall::abi_decode_returns(&out)?;
    if local != domain {
        return Err(mismatch(format!("ValidatorAnnounce localDomain() is {local}")).into());
    }
    let out = call(va, IValidatorAnnounce::mailboxCall {}.abi_encode()).await?;
    let mailbox = IValidatorAnnounce::mailboxCall::abi_decode_returns(&out)?;
    let out = call(hook, IMerkleTreeHook::mailboxCall {}.abi_encode()).await?;
    let hook_mailbox = IMerkleTreeHook::mailboxCall::abi_decode_returns(&out)?;
    if hook_mailbox != mailbox {
        return Err(mismatch(format!(
            "hook mailbox {hook_mailbox} differs from ValidatorAnnounce mailbox {mailbox}"
        ))
        .into());
    }
    let start_block = match config.index_from {
        Some(block) => block,
        None => {
            let out = call(hook, IMerkleTreeHook::deployedBlockCall {}.abi_encode()).await?;
            let block = IMerkleTreeHook::deployedBlockCall::abi_decode_returns(&out)?;
            u64::try_from(block).map_err(|_| mismatch("deployedBlock() exceeds u64".into()))?
        }
    };
    Ok(Origin {
        domain,
        hook,
        validator_announce: va,
        mailbox,
        start_block,
    })
}
