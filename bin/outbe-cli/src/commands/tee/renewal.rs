use super::args::{RenewArgs, StatusArgs};
use super::ensure_signer_matches_node_id;
use super::load_secp256k1_key_file;
use super::sign_node_hash;
use super::CliFinalityRpc;

use crate::errors::TeeRenewalError;
use crate::rpc::Rpc;

use alloy_primitives::U256;

use eyre::Result;

use outbe_operator::errors::RenewalError;
use outbe_operator::tee::read_renewal_status_v1;
use outbe_operator::tee::run_renewal_once_v1;

use outbe_operator::tee::NodeBindingSelectorV1;

use outbe_operator::tee::RenewalOutcomeV1;
use outbe_operator::tee::RenewalServiceConfigV1;
use outbe_operator::tee::RenewalServicesV1;

use outbe_operator::tx::RelaySignerV1;

use outbe_tee::load_committed_enclave_manifest_v1;

use outbe_tee::NodeHostIdentityV1;

use std::time::Duration;
use std::time::Instant;

const MANUAL_RENEWAL_FINALITY_TIMEOUT: Duration = Duration::from_secs(300);

const MANUAL_RENEWAL_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

pub(super) async fn renew(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    args: &RenewArgs,
) -> Result<()> {
    let RenewArgs {
        enclave_socket,
        node_data_dir,
        reth_p2p_secret_key,
    } = args;
    let reth_p2p_secret_key = reth_p2p_secret_key.as_deref();
    let private_key = private_key.ok_or(TeeRenewalError::PrivateKeyRequired)?;
    let evm_signer = RelaySignerV1::new(private_key)?;
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(TeeRenewalError::LoadCommittedManifest)?;
    let rpc_chain_id = client.eth_chain_id().await?;
    if manifest.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(TeeRenewalError::ManifestChainIdMismatch.into());
    }
    let path = reth_p2p_secret_key.ok_or(TeeRenewalError::MissingRethP2pSecretKey)?;
    let node_signing_key = load_secp256k1_key_file(path)?;
    ensure_signer_matches_node_id(&node_signing_key, &manifest.node_id)?;
    let mut enclave = outbe_tee::connect_or_initialize_node_host_enclave(
        enclave_socket,
        node_data_dir,
        NodeHostIdentityV1 {
            network_binding: manifest.network_binding(),
            reth_p2p_public: manifest.node_id.reth_p2p_public,
        },
        |hash| sign_node_hash(&node_signing_key, hash),
    )
    .map_err(TeeRenewalError::ConnectNodeHostEnclave)?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let config = RenewalServiceConfigV1 {
        node_data_dir: node_data_dir.to_path_buf(),
        selector,
        manifest,
    };
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| TeeRenewalError::NodeAuthoritySigningFailed(error).into())
    };
    let outcome = reconcile_renewal(
        async || {
            run_renewal_once_v1(
                RenewalServicesV1 {
                    rpc: &CliFinalityRpc(client),
                    evm_signer: &evm_signer,
                    enclave: &mut enclave,
                    node_signer: &signer,
                },
                &config,
            )
            .await
        },
        MANUAL_RENEWAL_FINALITY_TIMEOUT,
        MANUAL_RENEWAL_RECONCILE_INTERVAL,
    )
    .await?;
    println!("{outcome:#?}");
    Ok(())
}

async fn reconcile_renewal(
    mut run_once: impl AsyncFnMut() -> Result<RenewalOutcomeV1>,
    timeout: Duration,
    interval: Duration,
) -> Result<RenewalOutcomeV1> {
    let started = Instant::now();
    loop {
        let pending = match run_once().await {
            Ok(
                outcome @ (RenewalOutcomeV1::Finalized { .. } | RenewalOutcomeV1::NotDue { .. }),
            ) => {
                return Ok(outcome);
            }
            Ok(outcome) => format!("{outcome:#?}"),
            Err(error)
                if matches!(
                    error.downcast_ref::<RenewalError>(),
                    Some(RenewalError::AwaitingExactReceipt { .. })
                ) =>
            {
                format!("{error:#}")
            }
            Err(error) => return Err(error),
        };
        if started.elapsed() >= timeout {
            return Err(TeeRenewalError::RenewalTimeout(pending).into());
        }
        tokio::time::sleep(interval).await;
    }
}

pub(super) async fn renewal_status(client: &(impl Rpc + Sync), args: &StatusArgs) -> Result<()> {
    let StatusArgs {
        node_data_dir,
        warning_blocks,
        critical_blocks,
    } = args;
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(TeeRenewalError::LoadCommittedManifest)?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let status = read_renewal_status_v1(
        &CliFinalityRpc(client),
        node_data_dir,
        &selector,
        *warning_blocks,
        *critical_blocks,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use eyre::WrapErr as _;
    use std::collections::VecDeque;

    #[tokio::test]
    async fn renewal_waits_through_known_transaction_and_receipt_until_finality() {
        let transaction_hash = B256::repeat_byte(0x42);
        let finalized = RenewalOutcomeV1::Finalized {
            finalized_height: 123,
            valid_until: 2_000_000,
        };
        let awaiting = Err::<RenewalOutcomeV1, _>(eyre::eyre!("already known"))
            .wrap_err(RenewalError::AwaitingExactReceipt { transaction_hash });
        let mut observations = VecDeque::from([
            Ok(RenewalOutcomeV1::Submitted {
                transaction_hash,
                replayed: false,
            }),
            awaiting,
            Ok(RenewalOutcomeV1::Submitted {
                transaction_hash,
                replayed: true,
            }),
            Ok(finalized.clone()),
        ]);
        let outcome = reconcile_renewal(
            async || observations.pop_front().expect("unexpected reconciliation"),
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert_eq!(outcome, finalized);
        assert!(observations.is_empty());
    }

    #[tokio::test]
    async fn renewal_pending_observations_do_not_reset_the_deadline() {
        let transaction_hash = B256::repeat_byte(0x42);
        let mut observations = 0;
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            reconcile_renewal(
                async || {
                    observations += 1;
                    if observations % 2 == 0 {
                        Ok(RenewalOutcomeV1::Submitted {
                            transaction_hash,
                            replayed: true,
                        })
                    } else {
                        Err(RenewalError::AwaitingExactReceipt { transaction_hash }.into())
                    }
                },
                Duration::from_millis(20),
                Duration::from_millis(1),
            ),
        )
        .await
        .expect("pending observations must not extend the reconciliation budget")
        .expect_err("an unresolved renewal must time out");

        assert!(matches!(
            error.downcast_ref::<TeeRenewalError>(),
            Some(TeeRenewalError::RenewalTimeout(_))
        ));
        assert!(error
            .to_string()
            .contains(&format!("{transaction_hash:#x}")));
    }

    #[tokio::test]
    async fn renewal_unrelated_errors_remain_fatal() {
        for error in [
            eyre::eyre!("already known without the exact transaction receipt"),
            eyre::eyre!("renewal nonce was consumed without the exact transaction receipt"),
            RenewalError::MissingReceiptTransactionHash.into(),
            RenewalError::ReceiptTransactionHashMismatch.into(),
            RenewalError::MissingReceiptStatus.into(),
            RenewalError::NonSuccessReceiptStatus("0x0".to_owned()).into(),
        ] {
            let expected = format!("{error:#}");
            let mut observation = Some(error);
            let error = reconcile_renewal(
                async || {
                    Err(observation
                        .take()
                        .expect("a fatal error must not be retried"))
                },
                Duration::ZERO,
                Duration::ZERO,
            )
            .await
            .expect_err("an unrelated error must stop reconciliation");

            assert_eq!(format!("{error:#}"), expected);
        }
    }

    #[tokio::test]
    async fn renewal_not_due_finishes_without_waiting() {
        let not_due = RenewalOutcomeV1::NotDue {
            finalized_height: 123,
            opens_at_timestamp: 1_000_000,
        };
        let mut observation = Some(not_due.clone());
        let outcome = reconcile_renewal(
            async || {
                Ok(observation
                    .take()
                    .expect("NotDue must finish reconciliation"))
            },
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert_eq!(outcome, not_due);
    }
}
