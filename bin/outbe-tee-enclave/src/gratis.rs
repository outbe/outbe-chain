//! Stateless encrypted Gratis balance transitions. Pledge ownership is committed inside the enclave.
use crate::confidential::{FIELD_BALANCE, GRATIS};
use crate::errors::Result;
use alloy_primitives::{Address, B256, U256};
use outbe_tee::protocol::{GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus};
/// Derive the resident Gratis state key from the DKG group signature. See
/// [`crate::confidential::Domain::derive_state_key`].
pub fn derive_gratis_state_key(group_sig: &[u8], chain_id: B256, epoch: u64) -> Result<[u8; 32]> {
    GRATIS.derive_state_key(group_sig, chain_id, epoch)
}

/// Per-account view key: read capability AND the AEAD key for the account's
/// balance blobs, so a holder can decrypt its own state client-side.
pub fn derive_view_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    GRATIS.derive_view_key(state_key, account)
}

/// Per-account modify key: authorizes writes (via HMAC); never decrypts state.
pub fn derive_modify_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    GRATIS.derive_modify_key(state_key, account)
}

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

fn verify_modify_auth(
    modify_key: &[u8; 32],
    account: Address,
    op: GratisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
    mac: &[u8; 32],
) -> bool {
    GRATIS.verify_modify_auth(
        modify_key, account, op as u8, amount, op_nonce, chain_id, mac,
    )
}

/// Decrypt a `version || ct` amount blob; an empty blob is a fresh slot (`0`).
fn read_amount(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    crate::gratis_cipher::read_amount(view_key, account, field, blob)
}

/// Client-side helper: decrypt an account's balance blob with its view key (the
/// key delivered by `DeriveAccountKeys`). Same primitive the enclave uses, so a
/// client reproduces the plaintext without ever touching the state key.
pub fn decrypt_balance(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    read_amount(view_key, account, FIELD_BALANCE, blob).map(|(_, v)| v)
}

fn base_result() -> GratisOpResult {
    GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: Vec::new(),
        note_serial: B256::ZERO,
        event_amount: U256::ZERO,
        next_op_nonce: 0,
        fidelity: None,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    }
}
pub fn rejected_result(reason: String, inputs_canonical_hash: B256) -> GratisOpResult {
    let mut r = base_result();
    r.status = GratisOpStatus::Rejected { reason };
    r.inputs_canonical_hash = inputs_canonical_hash;
    r
}

fn reject(reason: impl Into<String>) -> GratisOpResult {
    let mut r = base_result();
    r.status = GratisOpStatus::Rejected {
        reason: reason.into(),
    };
    r
}

/// Apply a Gratis op over encrypted state. Pure and deterministic given
/// `state_key` + `req`. Sets `inputs_canonical_hash`; the caller (dispatch) signs
/// and fills `attestation_tag`. Business rejections come back as
/// `GratisOpStatus::Rejected` (-> precompile revert), never a panic.
pub fn apply_op(state_key: &[u8; 32], req: &GratisOpRequest) -> GratisOpResult {
    let inputs_canonical_hash = outbe_tee::protocol::gratis_op_canonical_hash(req);
    let mut result = match apply_op_inner(state_key, req) {
        Ok(r) => r,
        Err(e) => reject(e.to_string()),
    };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

fn apply_op_inner(state_key: &[u8; 32], req: &GratisOpRequest) -> Result<GratisOpResult> {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    use outbe_protocol::codec;
    use outbe_zk_canonical::pledgenote;
    if req.amount.is_zero() || req.account.is_zero() {
        return Ok(reject("amount and account must be nonzero"));
    }
    let owner_op = matches!(req.op, GratisOp::Mint | GratisOp::Burn | GratisOp::Pledge);
    let mut r = base_result();
    if owner_op {
        let modify_key = derive_modify_key(state_key, req.account)?;
        if !verify_modify_auth(
            &modify_key,
            req.account,
            req.op,
            req.amount,
            req.modify_auth.op_nonce,
            req.chain_id,
            &req.modify_auth.mac,
        ) {
            return Ok(reject("invalid modify authorization"));
        }
        let Some(nonce) = req.modify_auth.op_nonce.checked_add(1) else {
            return Ok(reject("modify nonce exhausted"));
        };
        r.next_op_nonce = nonce;
        if matches!(req.op, GratisOp::Pledge) {
            let entropy = outbe_tee::protocol::initial_pledge_secret(
                &modify_key,
                req.amount,
                req.modify_auth.op_nonce,
            );
            let secret = codec::field_from_be_bytes(&entropy);
            if secret == pledgenote::Field::from(0) {
                return Ok(reject("zero note secret"));
            }
            let serial = pledgenote::note_sn(req.account, secret)
                .map_err(|_| crate::errors::TeeError::DecryptFailed)?;
            if serial == pledgenote::Field::from(0) {
                return Ok(reject("zero note serial"));
            }
            r.note_serial = codec::field_to_b256(&serial)
                .map_err(|_| crate::errors::TeeError::DecryptFailed)?;
        }
    } else if matches!(
        req.op,
        GratisOp::ConsumePledge | GratisOp::ReleaseCollateral | GratisOp::BurnPledged
    ) && req.account != CREDIS_ADDRESS
    {
        return Ok(reject("collateral operation requires Credis account"));
    }
    if req.fidelity.is_some() && !owner_op {
        return Ok(reject("collateral must not change Fidelity"));
    }
    let view = derive_view_key(state_key, req.account)?;
    let (version, balance) = read_amount(&view, req.account, FIELD_BALANCE, &req.current_balance)?;
    let credit = matches!(
        req.op,
        GratisOp::Mint | GratisOp::Unpledge | GratisOp::ConsumePledge
    );
    let next = if credit {
        balance.checked_add(req.amount)
    } else {
        balance.checked_sub(req.amount)
    };
    let Some(next) = next else {
        return Ok(reject("insufficient balance or balance overflow"));
    };
    r.new_balance = crate::gratis_cipher::write_amount(
        &view,
        crate::gratis_cipher::BalanceTransition {
            account: req.account,
            field: FIELD_BALANCE,
            previous_version: version,
            amount: next,
            input_hash: outbe_tee::protocol::gratis_op_canonical_hash(req),
        },
    )?;
    r.event_amount = req.amount;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::protocol::ModifyAuth;
    const CHAIN: B256 = B256::repeat_byte(0xC1);
    fn state_key() -> [u8; 32] {
        derive_gratis_state_key(b"a-group-threshold-signature-~48-bytes-long!!", CHAIN, 0).unwrap()
    }
    fn alice() -> Address {
        Address::repeat_byte(0x11)
    }
    fn auth(sk: &[u8; 32], acct: Address, op: GratisOp, amount: U256, nonce: u64) -> ModifyAuth {
        let mk = derive_modify_key(sk, acct).unwrap();
        ModifyAuth {
            mac: modify_mac(&mk, acct, op, amount, nonce, CHAIN),
            op_nonce: nonce,
        }
    }
    fn req(op: GratisOp, account: Address, amount: U256, nonce: u64) -> GratisOpRequest {
        GratisOpRequest {
            op,
            chain_id: CHAIN,
            account,
            amount,
            current_balance: Vec::new(),
            modify_auth: ModifyAuth {
                mac: [0; 32],
                op_nonce: nonce,
            },
            fidelity: None,
        }
    }
    #[test]
    fn gratis_known_answer_vectors() {
        const KAT_GROUP_SIG: &[u8] = b"kat-fixed-gratis-group-signature-48b-padding";
        let sk = derive_gratis_state_key(KAT_GROUP_SIG, CHAIN, 0).unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(sk),
            "ee0bcead11e31dbafbf16c5b7fb2aa659045c38a7259db9002aa66bc9d9b08b3"
        );
        let vk = derive_view_key(&sk, alice()).unwrap();
        let blob = crate::gratis_cipher::write_amount(
            &vk,
            crate::gratis_cipher::BalanceTransition {
                account: alice(),
                field: FIELD_BALANCE,
                previous_version: 0,
                amount: U256::from(1000u64),
                input_hash: B256::ZERO,
            },
        )
        .unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(&blob),
            "00000000000000014752413246232be65ee469056f27d81bd908bab2cd61705404526e19acfa103c5f924bfb368f1a38d1b119051b1b5fb22425659291417af423d4475f231b154efb71e9de9e128688bc118a0bf920c64f413581a4"
        );
        assert_eq!(
            outbe_tee::gratis_decrypt::decrypt_gratis_balance(&vk, alice(), &blob).unwrap(),
            U256::from(1000u64),
        );
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let mac = modify_mac(&mk, alice(), GratisOp::Mint, U256::from(1000u64), 0, CHAIN);
        assert_eq!(
            alloy_primitives::hex::encode(mac),
            "688879e6e80acafeb78b7804de6edbd1032d95b2b654a049824c269c4aace152"
        );
    }
    #[test]
    fn mine_is_deterministic_across_calls() {
        let sk = state_key();
        let mut r = req(GratisOp::Mint, alice(), U256::from(1000u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mint, r.amount, 0);
        let a = apply_op(&sk, &r);
        let b = apply_op(&sk, &r);
        assert_eq!(a, b, "same state key + request -> byte-identical result");
        assert!(matches!(a.status, GratisOpStatus::Applied));
        assert!(!a.new_balance.is_empty());
    }
    #[test]
    fn view_key_decrypts_minted_balance() {
        let sk = state_key();
        let mut r = req(GratisOp::Mint, alice(), U256::from(4242u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mint, r.amount, 0);
        let res = apply_op(&sk, &r);
        // A holder with only the view key can decrypt the balance blob.
        let vk = derive_view_key(&sk, alice()).unwrap();
        let (_v, bal) = read_amount(&vk, alice(), FIELD_BALANCE, &res.new_balance).unwrap();
        assert_eq!(bal, U256::from(4242u64));
        assert_eq!(res.next_op_nonce, 1);
    }
    #[test]
    fn mine_rejects_forged_modify_auth() {
        let sk = state_key();
        let mut r = req(GratisOp::Mint, alice(), U256::from(1u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mint, r.amount, 0);
        r.modify_auth.mac[0] ^= 0xff;
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn burn_requires_sufficient_balance() {
        let sk = state_key();
        // mint 100
        let mut m = req(GratisOp::Mint, alice(), U256::from(100u64), 0);
        m.modify_auth = auth(&sk, alice(), GratisOp::Mint, m.amount, 0);
        let minted = apply_op(&sk, &m);
        // burn 200 -> reject
        let mut b = req(GratisOp::Burn, alice(), U256::from(200u64), 1);
        b.current_balance = minted.new_balance.clone();
        b.modify_auth = auth(&sk, alice(), GratisOp::Burn, b.amount, 1);
        assert!(matches!(
            apply_op(&sk, &b).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
}
