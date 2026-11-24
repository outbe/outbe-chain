use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::Result;
use outbe_vaultrouter::settlement::SettlementPayment;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/INodFactory.sol"
    );
}
pub use abi::INodFactory;

/// Every `issueNod` runs the pinned UltraHonk verifier, so its base gas covers verification.
pub fn base_gas(input: &[u8]) -> u64 {
    use alloy_sol_types::SolCall;
    match input.get(..4) {
        Some(selector) if selector == INodFactory::issueNodCall::SELECTOR => {
            outbe_primitives::storage::gas::ZK_VERIFY_GAS
        }
        _ => outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS,
    }
}

/// Dispatches NodFactory calls against EVM state.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            issueNod(c) => mutate(&storage, c, caller, |sender, c| {
                crate::offer::issue_nod(
                    &storage,
                    crate::offer::IssueNodInput {
                        caller: sender,
                        cipher_text: c.cipherText,
                        nonce: c.nonce,
                        ephemeral_pubkey: c.ephemeralPubkey,
                        worldwide_day: c.worldwideDay.into(),
                        issuance_currency: c.issuanceCurrency,
                        reference_currency: c.referenceCurrency,
                        zk_proof: c.zkProof,
                        l2_chain_id: c.chainId,
                        circuit_version: c.version,
                        zk_merkle_root: c.zkMerkleRoot,
                        signature: c.signature,
                    },
                )
            }),
            settleNod(c) => mutate(&storage, c, caller, |sender, c| {
                runtime::settle_nod(
                    &storage,
                    c.nodId,
                    SettlementPayment {
                        payer: sender,
                        asset: c.asset,
                        snapshot_id: c.snapshotId,
                    },
                    c.amountCapMinor,
                )?;
                Ok(INodFactory::settleNodReturn {})
            }),
            quoteSettlement(c) => view(c, |c| {
                let (settlement_currency, encrypted_payment_minor, snapshot_id) =
                    runtime::quote_settlement(&storage, caller, c.nodId, c.asset)?;
                Ok(INodFactory::quoteSettlementReturn {
                    settlementCurrency: settlement_currency,
                    encryptedPaymentMinor: encrypted_payment_minor,
                    snapshotId: snapshot_id,
                })
            }),
            getCallProgress(c) => view(c, |c| {
                crate::call_progress::call_progress(&storage, c.nodId)
            }),
            mineGratis(c) => mutate(&storage, c, caller, |sender, c| {
                let auth = outbe_gratisfactory::api::ModifyAuth {
                    mac: c.mac.0,
                    op_nonce: c.opNonce,
                };
                let encrypted_balance = runtime::mine_gratis(
                    &storage,
                    runtime::MineGratisRequest {
                        caller: sender,
                        nod_id: c.nodId,
                        nonce: c.nonce,
                        auth,
                    },
                )?;
                Ok(encrypted_balance)
            }),
        }
    })
}
