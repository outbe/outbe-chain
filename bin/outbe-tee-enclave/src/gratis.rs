//! Gratis key derivation and mint/burn authorization for the unified PledgeLedger.
//! Account balances, notes and collateral transitions live in `pledgenote`.

use crate::confidential::GRATIS;
use crate::errors::Result;
use alloy_primitives::{Address, B256, U256};
use outbe_tee::protocol::GratisOp;

/// Derive the resident Gratis state key from the DKG group signature. See
/// [`crate::confidential::Domain::derive_state_key`].
pub fn derive_gratis_state_key(group_sig: &[u8], chain_id: B256, epoch: u64) -> Result<[u8; 32]> {
    GRATIS.derive_state_key(group_sig, chain_id, epoch)
}

/// Per-account view key: decrypts private PledgeLedger receipts client-side.
pub fn derive_view_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    GRATIS.derive_view_key(state_key, account)
}

/// Per-account modify key: authorizes writes (via HMAC); never decrypts state.
pub fn derive_modify_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    GRATIS.derive_modify_key(state_key, account)
}

/// `HMAC-SHA256(modify_key, preimage)` - the write authorization the client sends
/// and the enclave re-checks. See [`crate::confidential::Domain::modify_mac`].
pub fn modify_mac(
    modify_key: &[u8; 32],
    account: Address,
    op: GratisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
) -> [u8; 32] {
    GRATIS.modify_mac(modify_key, account, op as u8, amount, op_nonce, chain_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    const CHAIN: B256 = B256::repeat_byte(0xC1);
    fn alice() -> Address {
        Address::repeat_byte(0x11)
    }

    #[test]
    fn gratis_known_answer_vectors() {
        const KAT_GROUP_SIG: &[u8] = b"kat-fixed-gratis-group-signature-48b-padding";
        let sk = derive_gratis_state_key(KAT_GROUP_SIG, CHAIN, 0).unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(sk),
            "ee0bcead11e31dbafbf16c5b7fb2aa659045c38a7259db9002aa66bc9d9b08b3"
        );
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let mac = modify_mac(&mk, alice(), GratisOp::Mint, U256::from(1000u64), 0, CHAIN);
        assert_eq!(
            alloy_primitives::hex::encode(mac),
            "688879e6e80acafeb78b7804de6edbd1032d95b2b654a049824c269c4aace152"
        );
    }
}
