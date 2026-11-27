//! Domain-specific elevated sponsored gas ceilings for EIP-7702 paymaster transactions.

use alloy_primitives::Address;
use outbe_system_common::addresses::{NOD_FACTORY_ADDRESS, PAYNOTE_ADDRESS};

/// Maximum sponsored gas limit for `NodFactory.issueNod`.
pub const FREE_TX_ISSUE_NOD_GAS_LIMIT: u64 = 8_000_000;

/// `issueNod(bytes,bytes,uint256,uint32,uint16,uint16,bytes,uint32,string,bytes,bytes)`.
const ISSUE_NOD_SELECTOR: [u8; 4] = [0x8d, 0x62, 0x15, 0x42];

/// Maximum sponsored gas limit for a PayNote proof call.
///
/// A transfer with change is the costliest: it uses about 1,660,000 gas on a
/// young tree, for the calldata, one proof verification and two leaf appends.
pub const FREE_TX_PAYNOTE_PROOF_GAS_LIMIT: u64 = 2_000_000;

/// `transfer(bytes)`.
const PAYNOTE_TRANSFER_SELECTOR: [u8; 4] = [0x06, 0x6c, 0xfa, 0xd1];
/// `resolveTransfer(bytes)`.
const PAYNOTE_RESOLVE_TRANSFER_SELECTOR: [u8; 4] = [0x8d, 0x01, 0x18, 0x6c];
/// `mergePayNotes(bytes)`.
const PAYNOTE_MERGE_SELECTOR: [u8; 4] = [0x40, 0x92, 0x4b, 0x8f];
/// `withdraw(bytes,address)`.
const PAYNOTE_WITHDRAW_SELECTOR: [u8; 4] = [0xc8, 0xf9, 0x8d, 0x91];

/// Operation-specific sponsored gas ceilings, keyed by `(target, selector)`.
///
/// The PayNote grants let a one-time key with no balance send a proof. A
/// transaction from the holder's own account would show the other party of the
/// payment who they are. `deposit` has no grant: it moves the caller's own
/// funds, so its sender is public anyway.
///
/// TODO: a one-time key costs nothing, so the daily quota of a signer does not
/// bound PayNote traffic. Only `SPONSORED_GAS_PER_BLOCK` does. Make each free
/// PayNote transaction pay for itself: add a public `fee` input to the
/// transfer, claim, claim-spend, spend and merge circuits, have the pool pay
/// it from the note to the block producer, and waive gas through a zero-fee
/// hook only when the proof carries a sufficient fee. Until then, a holder who
/// finds the sponsored budget full needs an external relayer or an ERC-4337
/// paymaster through the predeployed EntryPoint.
pub const ELEVATED_SPONSORED_GAS: &[(Address, [u8; 4], u64)] = &[
    (
        NOD_FACTORY_ADDRESS,
        ISSUE_NOD_SELECTOR,
        FREE_TX_ISSUE_NOD_GAS_LIMIT,
    ),
    (
        PAYNOTE_ADDRESS,
        PAYNOTE_TRANSFER_SELECTOR,
        FREE_TX_PAYNOTE_PROOF_GAS_LIMIT,
    ),
    (
        PAYNOTE_ADDRESS,
        PAYNOTE_RESOLVE_TRANSFER_SELECTOR,
        FREE_TX_PAYNOTE_PROOF_GAS_LIMIT,
    ),
    (
        PAYNOTE_ADDRESS,
        PAYNOTE_MERGE_SELECTOR,
        FREE_TX_PAYNOTE_PROOF_GAS_LIMIT,
    ),
    (
        PAYNOTE_ADDRESS,
        PAYNOTE_WITHDRAW_SELECTOR,
        FREE_TX_PAYNOTE_PROOF_GAS_LIMIT,
    ),
];
