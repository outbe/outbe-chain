//! Amount-private NOD mining; both ledger outcomes commit together on the host.
use crate::errors::{NodMineRejectReason, Result, TeeError};
use alloy_primitives::{B256, U256};
use outbe_tee::{
    nod_mine::{MineEncryptedNodRequestV1, MineEncryptedNodResultV1},
    protocol::{FidelityCohortOp, GratisOp, GratisOpRequest, GratisOpStatus},
};
pub fn apply(
    nod_state_key: &[u8; 32],
    gratis_key: &[u8; 32],
    fidelity_key: &[u8; 32],
    request: &MineEncryptedNodRequestV1,
) -> Result<MineEncryptedNodResultV1> {
    if request.fidelity.op != FidelityCohortOp::In {
        return Err(TeeError::DecryptFailed);
    }
    let amount = crate::nod_encryption::decrypt_nod(nod_state_key, &request.nod)?;
    let inputs = outbe_tee::nod_mine::inputs_hash(request).map_err(|_| TeeError::EncryptFailed)?;
    let operation = GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: B256::from(U256::from(request.nod.terms.chain_id)),
        account: request.nod.terms.owner,
        amount,
        current_balance: request.current_balance.clone(),
        current_pledged: Vec::new(),
        modify_auth: request.modify_auth.clone(),
        fidelity: Some(request.fidelity.clone()),
    };
    let result = crate::gratis::apply_op(gratis_key, &operation);
    if let GratisOpStatus::Rejected { reason } = result.status {
        return Err(TeeError::TributeOfferReject(reason));
    }
    let fidelity = crate::fidelity::apply_cohort_section(
        fidelity_key,
        operation.account,
        amount,
        &request.fidelity,
    )
    .map_err(NodMineRejectReason::FidelitySectionFailed)?;
    Ok(MineEncryptedNodResultV1 {
        new_balance: result.new_balance,
        next_op_nonce: result.next_op_nonce,
        fidelity,
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    })
}
