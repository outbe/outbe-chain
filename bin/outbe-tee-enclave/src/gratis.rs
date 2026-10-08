//! Stateless encrypted Gratis transitions over an account's liquid and pledged balances.
use crate::confidential::{FIELD_BALANCE, GRATIS};
use crate::errors::Result;
use alloy_primitives::{Address, B256, U256};
use outbe_tee::protocol::{GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus};

/// Pledged-blob field tag; differs from `FIELD_BALANCE` so the two blobs never share a nonce.
const FIELD_PLEDGED: u8 = 1;
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

/// Per-account modify key: authorizes writes (via HMAC). It never decrypts state.
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

/// Decrypt a `version || ct` amount blob. An empty blob is a fresh slot (`0`).
fn read_amount(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    GRATIS.read_amount(view_key, account, field, blob)
}

/// Encrypt `amount` into a fresh `version+1 || ct` blob.
fn write_amount(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    prev_version: u64,
    amount: U256,
) -> Result<Vec<u8>> {
    GRATIS.write_amount(view_key, account, field, prev_version, amount)
}

/// Client-side helper: decrypt an account's balance blob with its view key (the
/// key that `DeriveAccountKeys` delivers). It uses the same primitive as the
/// enclave, so a client reproduces the plaintext without ever touching the state key.
pub fn decrypt_balance(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    read_amount(view_key, account, FIELD_BALANCE, blob).map(|(_, v)| v)
}

/// Client-side helper: decrypt an account's pledged blob with its view key.
pub fn decrypt_pledged(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    read_amount(view_key, account, FIELD_PLEDGED, blob).map(|(_, v)| v)
}

fn base_result() -> GratisOpResult {
    GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: Vec::new(),
        new_pledged: Vec::new(),
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
/// `state_key` + `req`. Sets `inputs_canonical_hash`. The caller (dispatch) signs
/// and fills `attestation_tag`. This function returns business rejections as
/// `GratisOpStatus::Rejected` (-> precompile revert), never as a panic.
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
    if req.amount.is_zero() || req.account.is_zero() {
        return Ok(reject("amount and account must be nonzero"));
    }
    let owner_op = matches!(req.op, GratisOp::Mint | GratisOp::Burn | GratisOp::Pledge);
    let mut r = base_result();
    if owner_op {
        let modify_key = derive_modify_key(state_key, req.account)?;
        if !GRATIS.verify_modify_auth(
            &modify_key,
            req.account,
            req.op as u8,
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
    } else if req.fidelity.is_some() {
        return Ok(reject("collateral must not change Fidelity"));
    }
    let view = derive_view_key(state_key, req.account)?;
    // `Some(true)` credits the blob, `Some(false)` debits it, `None` leaves it unread.
    let (balance, pledged) = match req.op {
        GratisOp::Mint => (Some(true), None),
        GratisOp::Burn => (Some(false), None),
        GratisOp::Pledge => (Some(false), Some(true)),
        GratisOp::ReleasePledged => (Some(true), Some(false)),
        GratisOp::BurnPledged => (None, Some(false)),
    };
    let moves = [
        (balance, FIELD_BALANCE, &req.current_balance),
        (pledged, FIELD_PLEDGED, &req.current_pledged),
    ];
    let mut written = [Vec::new(), Vec::new()];
    for ((credit, field, blob), out) in moves.into_iter().zip(&mut written) {
        let Some(credit) = credit else { continue };
        let (version, current) = read_amount(&view, req.account, field, blob)?;
        let next = if credit {
            current.checked_add(req.amount)
        } else {
            current.checked_sub(req.amount)
        };
        let Some(next) = next else {
            return Ok(reject("insufficient balance or balance overflow"));
        };
        *out = write_amount(&view, req.account, field, version, next)?;
    }
    [r.new_balance, r.new_pledged] = written;
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
            current_pledged: Vec::new(),
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
        let blob = write_amount(&vk, alice(), FIELD_BALANCE, 0, U256::from(1000u64)).unwrap();
        assert_eq!(
            alloy_primitives::hex::encode(&blob),
            "0000000000000001186436dfe4774b400beaa3115d0ab9abae57d6defa6f80943ec703ede5ea855d8823cdeb05b2de5491aaf6829c5b213b"
        );
        let pledged = write_amount(&vk, alice(), FIELD_PLEDGED, 0, U256::from(1000u64)).unwrap();
        assert_eq!(alloy_primitives::hex::encode(&pledged), "0000000000000001067486674e123e00e26faa10058874b27c1b6e83cd4790eac7e13fccc9e0844f0f48b392582560d6f9e42ddc89d3746f");
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
    fn pledged(sk: &[u8; 32], amount: u64) -> Vec<u8> {
        let vk = derive_view_key(sk, alice()).unwrap();
        write_amount(&vk, alice(), FIELD_PLEDGED, 0, U256::from(amount)).unwrap()
    }
    #[test]
    fn release_pledged_moves_collateral_to_the_balance() {
        let sk = state_key();
        let vk = derive_view_key(&sk, alice()).unwrap();
        let mut r = req(GratisOp::ReleasePledged, alice(), U256::from(40u64), 0);
        r.current_pledged = pledged(&sk, 100);
        let res = apply_op(&sk, &r);
        assert_eq!(res.status, GratisOpStatus::Applied);
        assert_eq!(
            decrypt_pledged(&vk, alice(), &res.new_pledged).unwrap(),
            U256::from(60u64)
        );
        assert_eq!(
            decrypt_balance(&vk, alice(), &res.new_balance).unwrap(),
            U256::from(40u64)
        );
        assert_eq!(res.next_op_nonce, 0);
    }
    #[test]
    fn release_pledged_rejects_more_than_pledged() {
        let sk = state_key();
        let mut r = req(GratisOp::ReleasePledged, alice(), U256::from(101u64), 0);
        r.current_pledged = pledged(&sk, 100);
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn pledged_and_balance_blobs_are_not_interchangeable() {
        let sk = state_key();
        let vk = derive_view_key(&sk, alice()).unwrap();
        let blob = pledged(&sk, 100);
        assert!(decrypt_balance(&vk, alice(), &blob).is_err());
        let mut r = req(GratisOp::ReleasePledged, alice(), U256::from(1u64), 0);
        r.current_balance = blob.clone();
        r.current_pledged = blob;
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
        let balance = write_amount(&vk, alice(), FIELD_BALANCE, 0, U256::from(100u64)).unwrap();
        assert!(decrypt_pledged(&vk, alice(), &balance).is_err());
        let mut r = req(GratisOp::BurnPledged, alice(), U256::from(1u64), 0);
        r.current_pledged = balance;
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn pledge_moves_the_authorized_amount_into_the_pledged_balance() {
        let sk = state_key();
        let vk = derive_view_key(&sk, alice()).unwrap();
        let mut m = req(GratisOp::Mint, alice(), U256::from(100u64), 0);
        m.modify_auth = auth(&sk, alice(), GratisOp::Mint, m.amount, 0);
        let minted = apply_op(&sk, &m);
        let mut p = req(GratisOp::Pledge, alice(), U256::from(30u64), 1);
        p.current_balance = minted.new_balance;
        p.modify_auth = auth(&sk, alice(), GratisOp::Pledge, p.amount, 1);
        let res = apply_op(&sk, &p);
        assert_eq!(res.status, GratisOpStatus::Applied);
        assert_eq!(
            decrypt_balance(&vk, alice(), &res.new_balance).unwrap(),
            U256::from(70u64)
        );
        assert_eq!(
            decrypt_pledged(&vk, alice(), &res.new_pledged).unwrap(),
            U256::from(30u64)
        );
        assert_eq!(res.next_op_nonce, 2);
        p.modify_auth = auth(&sk, alice(), GratisOp::Mint, p.amount, 1);
        assert!(matches!(
            apply_op(&sk, &p).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn pledge_requires_sufficient_liquid_balance() {
        let sk = state_key();
        let mut p = req(GratisOp::Pledge, alice(), U256::from(1u64), 0);
        p.current_pledged = pledged(&sk, 100);
        p.modify_auth = auth(&sk, alice(), GratisOp::Pledge, p.amount, 0);
        assert!(matches!(
            apply_op(&sk, &p).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn burn_pledged_debits_only_the_pledged_balance() {
        let sk = state_key();
        let vk = derive_view_key(&sk, alice()).unwrap();
        let mut r = req(GratisOp::BurnPledged, alice(), U256::from(40u64), 0);
        r.current_pledged = pledged(&sk, 100);
        let res = apply_op(&sk, &r);
        assert_eq!(res.status, GratisOpStatus::Applied);
        assert!(res.new_balance.is_empty());
        assert_eq!(
            decrypt_pledged(&vk, alice(), &res.new_pledged).unwrap(),
            U256::from(60u64)
        );
        r.current_balance = vec![0xAB; 56];
        assert_eq!(apply_op(&sk, &r).status, GratisOpStatus::Applied);
        r.amount = U256::from(101u64);
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
    }
    #[test]
    fn pledged_blobs_cross_the_wire_and_bind_the_inputs_hash() {
        use outbe_tee::codec::{decode_request, decode_response, encode_request, encode_response};
        use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
        let sk = state_key();
        let mut r = req(GratisOp::ReleasePledged, alice(), U256::from(1u64), 0);
        r.current_pledged = pledged(&sk, 100);
        let result = apply_op(&sk, &r);
        let mut other = r.clone();
        other.current_pledged = pledged(&sk, 101);
        assert_ne!(
            result.inputs_canonical_hash,
            outbe_tee::protocol::gratis_op_canonical_hash(&other)
        );
        let request = EnclaveRequest::ApplyGratisOp {
            request: Box::new(r.clone()),
        };
        let EnclaveRequest::ApplyGratisOp { request } =
            decode_request(&encode_request(&request).unwrap()).unwrap()
        else {
            panic!("request variant changed");
        };
        assert_eq!(*request, r);
        let response = EnclaveResponse::GratisOpApplied {
            result: Box::new(result.clone()),
        };
        let EnclaveResponse::GratisOpApplied { result: decoded } =
            decode_response(&encode_response(&response).unwrap()).unwrap()
        else {
            panic!("response variant changed");
        };
        assert_eq!(*decoded, result);
    }
    #[test]
    fn collateral_ops_reject_a_fidelity_section() {
        let sk = state_key();
        for op in [GratisOp::ReleasePledged, GratisOp::BurnPledged] {
            let mut r = req(op, alice(), U256::from(1u64), 0);
            r.current_pledged = pledged(&sk, 100);
            r.fidelity = Some(outbe_tee::protocol::FidelityOpSection {
                op: outbe_tee::protocol::FidelityCohortOp::Out,
                timestamp: 1,
                first_qualified_start: 0,
                current_blob: Vec::new(),
            });
            assert!(matches!(
                apply_op(&sk, &r).status,
                GratisOpStatus::Rejected { .. }
            ));
        }
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
