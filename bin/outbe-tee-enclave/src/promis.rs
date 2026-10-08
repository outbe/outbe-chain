//! Enclave-side Promis confidential balance engine (secret-bearing).
//!
//! The Promis analogue of [`crate::gratis`], restricted to Mint/Burn over an
//! encrypted per-account balance. Promis has no pledge/credis machinery. All key
//! derivation, amount AEAD, and modify-auth verification is the shared
//! [`crate::confidential`] core under the [`crate::confidential::PROMIS`] domain,
//! so Promis keys are cryptographically independent from Gratis's. Every function
//! is a pure transform of its inputs + the resident state key (consensus
//! determinism).

use alloy_primitives::{Address, B256, U256};

use outbe_tee::protocol::{PromisOp, PromisOpRequest, PromisOpResult, PromisOpStatus};

use crate::confidential::{FIELD_BALANCE, PROMIS};
use crate::errors::Result;

/// Derive the resident Promis state key from the DKG group signature. See
/// [`crate::confidential::Domain::derive_state_key`].
pub fn derive_promis_state_key(group_sig: &[u8], chain_id: B256, epoch: u64) -> Result<[u8; 32]> {
    PROMIS.derive_state_key(group_sig, chain_id, epoch)
}

/// Per-account view key: read capability AND the AEAD key for the account's
/// balance blob. See [`crate::confidential::AccountKeyDerivation::derive_view_key`].
pub fn derive_view_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    PROMIS.account_keys.derive_view_key(state_key, account)
}

/// Per-account modify key: authorizes writes (via HMAC). It never decrypts state.
/// See [`crate::confidential::AccountKeyDerivation::derive_modify_key`].
pub fn derive_modify_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    PROMIS.account_keys.derive_modify_key(state_key, account)
}

/// `HMAC-SHA256(modify_key, preimage)` - the write authorization the client sends
/// and the enclave re-checks. See [`crate::confidential::ModifyDomain::modify_mac`].
pub fn modify_mac(
    modify_key: &[u8; 32],
    account: Address,
    op: PromisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
) -> [u8; 32] {
    PROMIS.authorization.modify_mac(
        modify_key,
        &crate::confidential::ModifyAuthorization {
            account,
            op_tag: op as u8,
            amount,
            op_nonce,
            chain_id,
        },
    )
}

/// Client-side helper: decrypt an account's Promis balance blob with its view key
/// (the key delivered by `DeriveAccountKeys`). Same primitive the enclave uses, so
/// a client reproduces the plaintext without ever touching the state key.
pub fn decrypt_balance(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    PROMIS
        .cipher
        .slot(view_key, account, FIELD_BALANCE)
        .read_amount(blob)
        .map(|(_, v)| v)
}

fn base_result() -> PromisOpResult {
    PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: Vec::new(),
        event_amount: U256::ZERO,
        next_op_nonce: 0,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    }
}

fn reject(reason: impl Into<String>) -> PromisOpResult {
    let mut r = base_result();
    r.status = PromisOpStatus::Rejected {
        reason: reason.into(),
    };
    r
}

/// Apply a Promis op over encrypted state. Pure and deterministic given
/// `state_key` + `req`. Sets `inputs_canonical_hash`. The caller (dispatch) signs
/// and fills `attestation_tag`. Business rejections come back as
/// `PromisOpStatus::Rejected` (-> precompile revert), never a panic.
pub fn apply_op(state_key: &[u8; 32], req: &PromisOpRequest) -> PromisOpResult {
    let inputs_canonical_hash = outbe_tee::protocol::promis_op_canonical_hash(req);
    let mut result = match apply_op_inner(state_key, req) {
        Ok(r) => r,
        Err(e) => reject(e.to_string()),
    };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

/// Mint/Burn - modify-key gated and keyed by `req.account`.
fn apply_op_inner(state_key: &[u8; 32], req: &PromisOpRequest) -> Result<PromisOpResult> {
    if req.amount.is_zero() {
        return Ok(reject("amount must be positive"));
    }
    if req.account.is_zero() {
        return Ok(reject("invalid address"));
    }
    let modify_key = PROMIS
        .account_keys
        .derive_modify_key(state_key, req.account)?;
    if !PROMIS.authorization.verify_modify_auth(
        &modify_key,
        &crate::confidential::ModifyAuthorization {
            account: req.account,
            op_tag: req.op as u8,
            amount: req.amount,
            op_nonce: req.modify_auth.op_nonce,
            chain_id: req.chain_id,
        },
        &req.modify_auth.mac,
    ) {
        return Ok(reject("invalid modify authorization"));
    }

    let view_key = PROMIS
        .account_keys
        .derive_view_key(state_key, req.account)?;
    let (bver, balance) = PROMIS
        .cipher
        .slot(&view_key, req.account, FIELD_BALANCE)
        .read_amount(&req.current_balance)?;

    let mut r = base_result();
    r.event_amount = req.amount;
    r.next_op_nonce = req.modify_auth.op_nonce.saturating_add(1);

    match req.op {
        PromisOp::Mint => {
            let new_balance = match balance.checked_add(req.amount) {
                Some(v) => v,
                None => return Ok(reject("promis balance overflow")),
            };
            r.new_balance = PROMIS
                .cipher
                .slot(&view_key, req.account, FIELD_BALANCE)
                .write_amount(bver, new_balance)?;
        }
        PromisOp::Burn => {
            if balance < req.amount {
                return Ok(reject("insufficient balance"));
            }
            r.new_balance = PROMIS
                .cipher
                .slot(&view_key, req.account, FIELD_BALANCE)
                .write_amount(bver, balance - req.amount)?;
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::protocol::ModifyAuth;

    const CHAIN: B256 = B256::repeat_byte(0xC2);

    fn state_key() -> [u8; 32] {
        derive_promis_state_key(b"a-group-threshold-signature-~48-bytes-long!!", CHAIN, 0).unwrap()
    }
    fn alice() -> Address {
        Address::repeat_byte(0x11)
    }
    fn auth(sk: &[u8; 32], acct: Address, op: PromisOp, amount: U256, nonce: u64) -> ModifyAuth {
        let mk = PROMIS.account_keys.derive_modify_key(sk, acct).unwrap();
        ModifyAuth {
            mac: PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: acct,
                    op_tag: op as u8,
                    amount,
                    op_nonce: nonce,
                    chain_id: CHAIN,
                },
            ),
            op_nonce: nonce,
        }
    }
    fn req(op: PromisOp, acct: Address, amount: U256, nonce: u64) -> PromisOpRequest {
        PromisOpRequest {
            op,
            chain_id: CHAIN,
            account: acct,
            amount,
            current_balance: Vec::new(),
            modify_auth: ModifyAuth {
                mac: [0u8; 32],
                op_nonce: nonce,
            },
        }
    }

    #[test]
    fn mint_is_deterministic_across_calls() {
        let sk = state_key();
        let mut r = req(PromisOp::Mint, alice(), U256::from(1000u64), 0);
        r.modify_auth = auth(&sk, alice(), PromisOp::Mint, r.amount, 0);
        let a = apply_op(&sk, &r);
        let b = apply_op(&sk, &r);
        assert_eq!(a, b);
        assert_eq!(a.status, PromisOpStatus::Applied);
    }

    #[test]
    fn view_key_decrypts_minted_balance() {
        let sk = state_key();
        let mut r = req(PromisOp::Mint, alice(), U256::from(4242u64), 0);
        r.modify_auth = auth(&sk, alice(), PromisOp::Mint, r.amount, 0);
        let res = apply_op(&sk, &r);
        let vk = PROMIS.account_keys.derive_view_key(&sk, alice()).unwrap();
        assert_eq!(
            decrypt_balance(&vk, alice(), &res.new_balance).unwrap(),
            U256::from(4242u64)
        );
    }

    #[test]
    fn mint_rejects_forged_modify_auth() {
        let sk = state_key();
        let mut r = req(PromisOp::Mint, alice(), U256::from(1u64), 0);
        r.modify_auth = ModifyAuth {
            mac: [7u8; 32],
            op_nonce: 0,
        };
        assert!(matches!(
            apply_op(&sk, &r).status,
            PromisOpStatus::Rejected { .. }
        ));
    }

    #[test]
    fn burn_requires_sufficient_balance() {
        let sk = state_key();
        let mut m = req(PromisOp::Mint, alice(), U256::from(100u64), 0);
        m.modify_auth = auth(&sk, alice(), PromisOp::Mint, m.amount, 0);
        let minted = apply_op(&sk, &m);
        let mut b = req(PromisOp::Burn, alice(), U256::from(200u64), 1);
        b.current_balance = minted.new_balance.clone();
        b.modify_auth = auth(&sk, alice(), PromisOp::Burn, b.amount, 1);
        assert!(matches!(
            apply_op(&sk, &b).status,
            PromisOpStatus::Rejected { .. }
        ));
    }

    #[test]
    fn burn_reduces_balance() {
        let sk = state_key();
        let mut m = req(PromisOp::Mint, alice(), U256::from(1000u64), 0);
        m.modify_auth = auth(&sk, alice(), PromisOp::Mint, m.amount, 0);
        let minted = apply_op(&sk, &m);
        let mut b = req(PromisOp::Burn, alice(), U256::from(300u64), 1);
        b.current_balance = minted.new_balance.clone();
        b.modify_auth = auth(&sk, alice(), PromisOp::Burn, b.amount, 1);
        let burned = apply_op(&sk, &b);
        assert_eq!(burned.status, PromisOpStatus::Applied);
        let vk = PROMIS.account_keys.derive_view_key(&sk, alice()).unwrap();
        assert_eq!(
            decrypt_balance(&vk, alice(), &burned.new_balance).unwrap(),
            U256::from(700u64)
        );
    }

    #[test]
    fn modify_mac_binds_every_input_and_is_separated_from_the_gratis_domain() {
        use crate::confidential::GRATIS;
        let sk = state_key();
        let mk = PROMIS.account_keys.derive_modify_key(&sk, alice()).unwrap();
        let base = PROMIS.authorization.modify_mac(
            &mk,
            &crate::confidential::ModifyAuthorization {
                account: alice(),
                op_tag: PromisOp::Mint as u8,
                amount: U256::from(10u64),
                op_nonce: 3,
                chain_id: CHAIN,
            },
        );
        let variants = [
            PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: Address::repeat_byte(0x22),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(10u64),
                    op_nonce: 3,
                    chain_id: CHAIN,
                },
            ),
            PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Burn as u8,
                    amount: U256::from(10u64),
                    op_nonce: 3,
                    chain_id: CHAIN,
                },
            ),
            PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(11u64),
                    op_nonce: 3,
                    chain_id: CHAIN,
                },
            ),
            PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(10u64),
                    op_nonce: 4,
                    chain_id: CHAIN,
                },
            ),
            PROMIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(10u64),
                    op_nonce: 3,
                    chain_id: B256::repeat_byte(0xC3),
                },
            ),
            // Same key bytes and inputs under the Gratis tag must not authorize a Promis write.
            GRATIS.authorization.modify_mac(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(10u64),
                    op_nonce: 3,
                    chain_id: CHAIN,
                },
            ),
        ];
        for (i, other) in variants.iter().enumerate() {
            assert_ne!(base, *other, "variant {i} must change the authorization");
            assert!(!PROMIS.authorization.verify_modify_auth(
                &mk,
                &crate::confidential::ModifyAuthorization {
                    account: alice(),
                    op_tag: PromisOp::Mint as u8,
                    amount: U256::from(10u64),
                    op_nonce: 3,
                    chain_id: CHAIN
                },
                other
            ));
        }
        assert!(PROMIS.authorization.verify_modify_auth(
            &mk,
            &crate::confidential::ModifyAuthorization {
                account: alice(),
                op_tag: PromisOp::Mint as u8,
                amount: U256::from(10u64),
                op_nonce: 3,
                chain_id: CHAIN
            },
            &base
        ));
    }

    /// Known-answer modify authorizations for wallets, computed outside this implementation.
    #[test]
    fn modify_mac_matches_the_known_answer_vectors() {
        use crate::confidential::GRATIS;
        use alloy_primitives::{address, b256};
        use outbe_tee::protocol::GratisOp;

        let key = [0x5a; 32];
        let chain_one = B256::from(U256::from(1u64));
        let vectors = [
            (
                &GRATIS,
                alice(),
                GratisOp::Mint as u8,
                U256::from(1_000u64),
                0,
                chain_one,
                b256!("0xec11651be45952198f63a0c982bbe045c3f3a31131c8afe655f76b243dc50aa1"),
            ),
            (
                &GRATIS,
                alice(),
                GratisOp::Pledge as u8,
                U256::from(10_000_000u64),
                3,
                chain_one,
                b256!("0xf7d19e989e00dc99df7168cbca111ac11c7328fa0e12c4b0e7873b3ce4e84046"),
            ),
            (
                &PROMIS,
                alice(),
                PromisOp::Mint as u8,
                U256::from(1_000u64),
                0,
                chain_one,
                b256!("0x3d33ac4e239f595c5d87e9db9bd4e180ff4f080811f22150cfc0b7b7eba2467b"),
            ),
            (
                &PROMIS,
                address!("0xabcdef0123456789abcdef0123456789abcdef01"),
                PromisOp::Burn as u8,
                U256::from(123_456_789_012_345_678_901_234_567_890u128),
                7,
                CHAIN,
                b256!("0x94bbfa7708394cf45466594357d2dc942f65e92ef0aa2509e03dfec5a5861f84"),
            ),
        ];
        for (ledger, account, op, amount, op_nonce, chain_id, expected) in vectors {
            let mac = ledger.authorization.modify_mac(
                &key,
                &crate::confidential::ModifyAuthorization {
                    account,
                    op_tag: op,
                    amount,
                    op_nonce,
                    chain_id,
                },
            );
            assert_eq!(B256::from(mac), expected);
            assert!(ledger.authorization.verify_modify_auth(
                &key,
                &crate::confidential::ModifyAuthorization {
                    account,
                    op_tag: op,
                    amount,
                    op_nonce,
                    chain_id
                },
                &mac
            ));
        }
    }
}
