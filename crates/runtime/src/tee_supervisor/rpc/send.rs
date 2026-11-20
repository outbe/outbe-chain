use super::*;
use alloy_consensus::{transaction::SignerRecoverable, Transaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;

pub(super) enum RejectedNonce {
    Consumed,
    Occupied(u64),
}
impl RpcLifecyclePort {
    // -32000 covers unrelated errors. Classify from chain facts, never error prose.
    pub(super) async fn rejected_nonce(&self, raw: &[u8]) -> Option<RejectedNonce> {
        let mut input = raw;
        let transaction = TxEnvelope::decode_2718(&mut input).ok()?;
        if !input.is_empty() {
            return None;
        }
        let sender = transaction.recover_signer().ok()?;
        let nonce = transaction.nonce();
        let latest = self
            .account_nonce(sender, LifecycleNonceTagV1::Latest)
            .await
            .ok()?;
        if latest > nonce {
            return Some(RejectedNonce::Consumed);
        }
        let pending = self
            .account_nonce(sender, LifecycleNonceTagV1::Pending)
            .await
            .ok()?;
        (pending > nonce).then_some(RejectedNonce::Occupied(nonce))
    }
}
