use super::*;
use outbe_validatorset::{read_committee_snapshot_for_epoch, state::write_committee_snapshot};

fn recorder() -> &'static reth_node_metrics::recorder::PrometheusRecorder {
    reth_node_metrics::recorder::install_prometheus_recorder()
}

async fn wait_for_skip(reason: &str) {
    wait_for_counter("skipped", reason, 1).await;
}

pub fn counter(metric: &str, reason: &str) -> u64 {
    recorder()
        .handle()
        .render()
        .lines()
        .find_map(|line| {
            (line.starts_with(&format!(
                "reth_outbe_slashing_equivocation_{metric}_total{{"
            )) && line.contains(&format!("reason=\"{reason}\"")))
            .then(|| line.split_whitespace().last()?.parse::<u64>().ok())
            .flatten()
        })
        .unwrap_or_default()
}

pub async fn wait_for_counter(metric: &str, reason: &str, minimum: u64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while counter(metric, reason) < minimum {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn cancellation_interrupts_a_non_slashable_retry_deadline() -> eyre::Result<()> {
    let _ = recorder();
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let seeded = support::seed_target(signer.address(), false);
    let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::ConflictingNotarize));
    let cancel = CancellationToken::new();
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox.clone(),
        Duration::from_secs(3600),
        cancel.clone(),
    );
    wait_for_skip("not_slashable").await;
    assert!(mailbox.is_empty());
    assert!(node
        .pool
        .get_transactions_by_sender(signer.address())
        .is_empty());
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    Ok(())
}

async fn an_evicted_epoch_expires_even_a_non_slashable_validator(
    inactive: bool,
) -> eyre::Result<()> {
    let _ = recorder();
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let mut seeded = if inactive {
        support::seed_inactive(signer.address())
    } else {
        support::seed_target(signer.address(), false)
    };
    let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::ConflictingFinalize));
    let cancel = CancellationToken::new();
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox.clone(),
        INTERVAL,
        cancel.clone(),
    );
    let before = counter("skipped", "not_slashable");
    wait_for_counter("skipped", "not_slashable", before + 1).await;
    seeded.enter(|storage| {
        let snapshot = read_committee_snapshot_for_epoch(storage.clone(), 1)
            .unwrap()
            .unwrap();
        write_committee_snapshot(storage.clone(), 9, &snapshot).unwrap();
        assert!(read_committee_snapshot_for_epoch(storage, 1)
            .unwrap()
            .is_none());
    });
    let expired_before = counter("skipped", "expired_epoch");
    support::publish(&node, &seeded)?;
    support::advance_fee(&node, 1)?;
    wait_for_counter("skipped", "expired_epoch", expired_before + 1).await;
    tokio::time::sleep(INTERVAL * 3).await;
    assert_eq!(counter("skipped", "expired_epoch"), expired_before + 1);
    assert!(mailbox.is_empty());
    assert!(node
        .pool
        .get_transactions_by_sender(signer.address())
        .is_empty());
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    Ok(())
}

#[tokio::test]
async fn retained_evidence_retry_expiration_and_deadline_cancellation() -> eyre::Result<()> {
    cancellation_interrupts_a_non_slashable_retry_deadline().await?;
    for inactive in [false, true] {
        an_evicted_epoch_expires_even_a_non_slashable_validator(inactive).await?;
    }
    let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32])?);
    let mut seeded = support::seed_unjailed(signer.address());
    let (node, _runtime, _temp) = launch(&seeded, signer.clone()).await?;
    let mailbox = EquivocationEvidenceMailbox::new();
    mailbox.push(evidence(Kind::NullifyFinalize));
    let cancel = CancellationToken::new();
    let before = counter("skipped", "not_slashable");
    let worker = spawn(
        node.clone(),
        signer.clone(),
        mailbox.clone(),
        INTERVAL,
        cancel.clone(),
    );
    wait_for_counter("skipped", "not_slashable", before + 1).await;
    while !mailbox.is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(node
        .pool
        .get_transactions_by_sender(signer.address())
        .is_empty());
    seeded.enter(|storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        vs.admit_validator_for_boundary_for_test(ACCUSED).unwrap();
        assert!(matches!(
            vs.validator_lifecycle(ACCUSED).unwrap(),
            outbe_validatorset::ValidatorLifecycle::Joining(_)
        ));
    });
    let before_joining = counter("skipped", "not_slashable");
    support::publish(&node, &seeded)?;
    support::advance_fee(&node, 1)?;
    wait_for_counter("skipped", "not_slashable", before_joining + 1).await;
    assert!(node
        .pool
        .get_transactions_by_sender(signer.address())
        .is_empty());
    seeded.enter(|storage| {
        outbe_validatorset::contract::ValidatorSet::new(storage)
            .activate_validator_via_boundary_for_test(ACCUSED)
            .unwrap();
    });
    support::publish(&node, &seeded)?;
    support::advance_fee(&node, 1)?;
    let transaction = tokio::time::timeout(Duration::from_secs(10), async {
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
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker).await??;
    support::execute(
        &seeded,
        transaction.to_consensus().into_inner(),
        signer.address(),
    );
    Ok(())
}
