use super::*;
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::B256;
use outbe_execution::OutbeFullNode;
use reth_primitives_traits::Recovered;
use reth_transaction_pool::{error::PoolErrorKind, EthPooledTransaction, TransactionOrigin};

async fn transaction_at(
    node: &OutbeFullNode,
    sender: alloy_primitives::Address,
    fee: u128,
) -> Arc<reth_transaction_pool::ValidPoolTransaction<EthPooledTransaction>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(tx) = node
                .pool
                .get_transactions_by_sender(sender)
                .into_iter()
                .find(|tx| tx.nonce() == 0 && tx.max_fee_per_gas() == fee)
            {
                break tx;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

fn resign(
    tx: &reth_transaction_pool::ValidPoolTransaction<EthPooledTransaction>,
    signer: &OutbeEvmSigner,
    nonce: u64,
    fee: u128,
) -> EthPooledTransaction {
    let consensus = tx.to_consensus();
    let mut unsigned = consensus.as_eip1559().unwrap().tx().clone();
    unsigned.nonce = nonce;
    unsigned.max_fee_per_gas = fee;
    let signed = signer.sign_eip1559(unsigned).unwrap();
    let size = signed.encode_2718_len();
    EthPooledTransaction::new(Recovered::new_unchecked(signed, signer.address()), size)
}

#[tokio::test]
async fn own_basefee_replacement_retries_underpriced_and_ignores_queued_siblings(
) -> eyre::Result<()> {
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let seeded = seed(signer.address());
    let (node, _runtime, _temp) =
        support::launch_configured(&seeded, signer.clone(), 100, 200).await?;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::ConflictingNotarize));
    let cancel = CancellationToken::new();
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox,
        INTERVAL,
        cancel.clone(),
    );
    let own = transaction_at(&node, signer.address(), 150).await;
    let first_hash = *own.hash();
    assert_eq!(own.origin, TransactionOrigin::Local);
    support::advance_fee(&node, 151)?;
    let queued = node
        .pool
        .add_transaction(TransactionOrigin::Local, resign(&own, &signer, 1, 1000))
        .await?;
    assert!(node.pool.pool_size().basefee > 0);
    assert!(node.pool.pool_size().queued > 0);
    let failures = retention::counter("submit_failed", "submit");
    retention::wait_for_counter("submit_failed", "submit", failures + 1).await;
    assert_eq!(
        *transaction_at(&node, signer.address(), 150).await.hash(),
        first_hash
    );
    let error = node
        .pool
        .add_transaction(TransactionOrigin::Local, resign(&own, &signer, 0, 302))
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PoolErrorKind::ReplacementUnderpriced));
    assert!(node.pool.get(&queued.hash).is_some());
    support::advance_fee(&node, 230)?;
    let replacement = transaction_at(&node, signer.address(), 460).await;
    assert_ne!(*replacement.hash(), first_hash);
    assert_eq!(replacement.nonce(), 0);
    assert_ne!(*replacement.hash(), B256::ZERO);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    support::execute(
        &seeded,
        replacement.to_consensus().into_inner(),
        signer.address(),
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_key_foreign_transaction_defers_evidence_until_the_committed_nonce(
) -> eyre::Result<()> {
    use revm::{
        database::{states::StateChangeset, Database as _},
        state::AccountInfo,
    };
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let seeded = seed(signer.address());
    let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
    let initial_account = AccountInfo {
        balance: U256::from(1_000_000),
        ..Default::default()
    };
    support::commit_changes(
        &node,
        StateChangeset {
            accounts: vec![(signer.address(), Some(initial_account.clone()))],
            ..Default::default()
        },
    )?;
    support::advance_fee(&node, 1)?;
    let foreign = signer.sign_eip1559(alloy_consensus::TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE.into(),
        max_priority_fee_per_gas: 0,
        to: alloy_primitives::TxKind::Call(signer.address()),
        ..Default::default()
    })?;
    let size = foreign.encode_2718_len();
    let foreign_hash = node
        .pool
        .add_transaction(
            TransactionOrigin::Local,
            EthPooledTransaction::new(
                Recovered::new_unchecked(foreign.clone(), signer.address()),
                size,
            ),
        )
        .await?
        .hash;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::ConflictingFinalize));
    let cancel = CancellationToken::new();
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox,
        INTERVAL,
        cancel.clone(),
    );
    retention::wait_for_counter("skipped", "sender_busy", 1).await;
    let occupied = node.pool.get_transactions_by_sender(signer.address());
    assert_eq!(occupied.len(), 1);
    assert_eq!(*occupied[0].hash(), foreign_hash);
    assert_eq!(occupied[0].nonce(), 0);
    let mut state =
        support::execute_envelope(&seeded, foreign.clone(), signer.address(), initial_account);
    let committed = state.basic(signer.address())?.unwrap();
    assert_eq!(committed.nonce, 1);
    support::commit_changes(
        &node,
        StateChangeset {
            accounts: vec![(signer.address(), Some(committed.clone()))],
            ..Default::default()
        },
    )?;
    use reth_transaction_pool::TransactionPoolExt as _;
    node.pool
        .update_accounts(vec![reth_provider::ChangedAccount {
            address: signer.address(),
            nonce: committed.nonce,
            balance: committed.balance,
        }]);
    node.pool.remove_transactions(vec![foreign_hash]);
    // Publish the executed foreign envelope and its committed sender nonce together.
    support::advance_block(&node, 1, vec![foreign])?;
    let tx = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(tx) = node
                .pool
                .get_transactions_by_sender(signer.address())
                .first()
            {
                break tx.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(tx.nonce(), 1);
    assert_eq!(node.pool.best_transactions().next().unwrap().nonce(), 1);
    assert_ne!(*tx.hash(), foreign_hash);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    Ok(())
}
