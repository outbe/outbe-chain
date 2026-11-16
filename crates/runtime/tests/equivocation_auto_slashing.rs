//! Component conformance for the execution-side consensus evidence consumer.

#[path = "equivocation_auto_slashing/mod.rs"]
mod support;

use std::{sync::Arc, time::Duration};

use alloy_consensus::Transaction as _;
use alloy_primitives::U256;
use outbe_primitives::{
    equivocation_evidence::{EquivocationEvidenceKind as Kind, EquivocationEvidenceMailbox},
    signer::OutbeEvmSigner,
};
use outbe_runtime::equivocation_slasher::spawn;
use reth_transaction_pool::TransactionPool as _;
use support::{evidence, launch, seed, ACCUSED, STAKE};
use tokio_util::sync::CancellationToken;

const INTERVAL: Duration = Duration::from_millis(20);

#[tokio::test]
async fn every_vote_conflict_reaches_the_real_pool_from_a_zero_balance_sender() -> eyre::Result<()>
{
    outbe_primitives::proof::init_consensus_chain_id(1)?;
    for kind in [
        Kind::ConflictingNotarize,
        Kind::ConflictingFinalize,
        Kind::NullifyFinalize,
    ] {
        let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
        let seeded = seed(signer.address());
        let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
        let mailbox = EquivocationEvidenceMailbox::new();
        mailbox.push(evidence(kind));
        let cancel = CancellationToken::new();
        let worker = spawn(
            node.clone(),
            signer.clone(),
            mailbox.clone(),
            INTERVAL,
            cancel.clone(),
        );
        let transaction = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let transactions = node.pool.get_transactions_by_sender(signer.address());
                if let Some(tx) = transactions.first() {
                    break tx.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert!(mailbox.is_empty());
        assert_eq!(transaction.nonce(), 0);
        assert_eq!(transaction.to_consensus().value(), U256::ZERO);
        assert_eq!(transaction.max_priority_fee_per_gas(), Some(0));
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker).await??;
        support::execute(
            &seeded,
            transaction.to_consensus().into_inner(),
            signer.address(),
        );
        println!(
            "{kind:?}: felony_count=1 JailRetained stake={} sender_balance=0 nonce=1",
            STAKE * 92 / 100
        );
        assert_ne!(signer.address(), ACCUSED);
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_an_idle_mailbox_wait() -> eyre::Result<()> {
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let (node, _runtime, _temp) = launch(&seed(signer.address()), signer.clone()).await?;
    let cancel = CancellationToken::new();
    let worker = spawn(
        node,
        signer,
        EquivocationEvidenceMailbox::new(),
        INTERVAL,
        cancel.clone(),
    );
    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    Ok(())
}

#[path = "equivocation_auto_slashing/retention.rs"]
mod retention;

#[path = "equivocation_auto_slashing/pool.rs"]
mod pool;

#[tokio::test]
async fn reporter_ranking_excludes_the_accused_before_assigning_the_first_slot() -> eyre::Result<()>
{
    use alloy_primitives::keccak256;
    let mut prefix = 1u64.to_be_bytes().to_vec();
    prefix.extend_from_slice(ACCUSED.as_slice());
    let rank_hash = |address: alloy_primitives::Address| {
        keccak256([prefix.as_slice(), address.as_slice()].concat())
    };
    let signer = (1..=255)
        .map(|byte| OutbeEvmSigner::from_secret_bytes([byte; 32]).unwrap())
        .find(|signer| rank_hash(ACCUSED) < rank_hash(signer.address()))
        .unwrap();
    let signer = Arc::new(signer);
    let seeded = seed(signer.address());
    let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::ConflictingFinalize));
    let cancel = CancellationToken::new();
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox,
        Duration::from_secs(3600),
        cancel.clone(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while node
            .pool
            .get_transactions_by_sender(signer.address())
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    assert_eq!(
        node.pool.get_transactions_by_sender(signer.address()).len(),
        1
    );
    Ok(())
}
