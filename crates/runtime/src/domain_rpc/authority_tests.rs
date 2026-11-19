//! Private RPC authority revokes synchronously and reopens without losing resident keys.
use super::*;
use outbe_core_common::confidential_tee::{derive_account_keys_message, eip191_hash, Ledger};
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_tee::authority::{TeeAuthorityChannel, TeeAuthorityState as State};
use reth_provider::test_utils::NoopProvider;

fn closed_states() -> [State; 4] {
    [
        State::UnkeyedAwaitingAdmission,
        State::SuspendedJailed,
        State::SuspendedLeaseExpired,
        State::SuspendedPolicyMismatch,
    ]
}

#[tokio::test]
async fn private_rpc_revokes_before_work_and_reopens_real_sealed_exports() {
    let context = crate::tee_canary_probe::test_support::context(8);
    crate::tee_canary_probe::test_support::activate(&context);
    let public_key = context.network_encryption_public().unwrap();
    let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
        Arc::clone(&context),
    ));
    let authority = TeeAuthorityChannel::default();
    let provider: Arc<
        NoopProvider<reth_chainspec::ChainSpec<OutbeHeader>, outbe_primitives::OutbePrimitives>,
    > = Arc::new(NoopProvider::new(Arc::new(
        reth_chainspec::ChainSpec::default(),
    )));
    let rpc = OutbeDomainRpcHandler::new(provider, (777, context.manifest().genesis_hash))
        .with_authority_watch(authority.subscribe())
        .into_rpc();
    let signer = OutbeEvmSigner::from_secret_bytes([0x55; 32]).unwrap();
    let owner = signer.address();
    let eph_secret = x25519_dalek::StaticSecret::from([0x33; 32]);
    let eph = B256::from(x25519_dalek::PublicKey::from(&eph_secret).to_bytes());
    let day = outbe_system_common::time::WorldwideDay::new(20250115);
    let subject = TributeSourceSubject {
        checkpoint: outbe_tribute::confidential::TributeSourceCheckpoint {
            block_number: 1,
            block_hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
        },
        chain_id: 777,
        worldwide_day: day,
        tribute_count: 1,
    };
    let id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(3));
    for state in closed_states() {
        authority.publish(state);
        for method in ["outbe_deriveKeys", "outbe_deriveGratisKeys"] {
            let error = if method == "outbe_deriveKeys" {
                rpc.call::<_, GratisKeysSealed>(method, (Ledger::Gratis, owner, eph, Bytes::new()))
                    .await
                    .unwrap_err()
            } else {
                rpc.call::<_, GratisKeysSealed>(method, (owner, eph, Bytes::new()))
                    .await
                    .unwrap_err()
            };
            assert!(
                error.to_string().contains("PrivateExecutionUnavailable"),
                "{method}: {error}"
            );
        }
        let error = rpc
            .call::<_, TributeStateSource>("outbe_getTributeStateSource", (subject, id))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("PrivateExecutionUnavailable"),
            "{error}"
        );
        authority.publish(State::Armed {
            enclave_id: B256::repeat_byte(7),
            valid_until: 123,
        });
        for ledger in [Ledger::Nod, Ledger::Gratis] {
            let signature = Bytes::copy_from_slice(
                &signer
                    .sign_hash(&eip191_hash(&derive_account_keys_message(
                        ledger, owner, eph,
                    )))
                    .unwrap(),
            );
            let sealed: GratisKeysSealed = if ledger == Ledger::Gratis {
                rpc.call("outbe_deriveGratisKeys", (owner, eph, signature))
                    .await
                    .unwrap()
            } else {
                rpc.call("outbe_deriveKeys", (ledger, owner, eph, signature))
                    .await
                    .unwrap()
            };
            assert_eq!(sealed.nonce.len(), 12);
            assert_eq!(sealed.sealed.len(), 80);
            assert_ne!(sealed.enclave_ephemeral_pubkey, B256::ZERO);
        }
        assert_eq!(context.network_encryption_public().unwrap(), public_key);
    }
}

#[test]
fn process_authority_port_is_read_live_and_missing_or_dropped_publishers_fail_closed() {
    use outbe_tee::authority::{install_tee_authority_watch, tee_authority_test_guard};
    let _guard = tee_authority_test_guard();
    let handler = OutbeDomainRpcHandler::new(Arc::new(()), (777, B256::repeat_byte(2)));
    assert!(require_private_authority(handler.authority_watch.as_ref()).is_err());
    let authority = TeeAuthorityChannel::default();
    install_tee_authority_watch(authority.subscribe()).unwrap();
    authority.publish(State::Armed {
        enclave_id: B256::repeat_byte(7),
        valid_until: 123,
    });
    require_private_authority(handler.authority_watch.as_ref()).unwrap();
    authority.publish(State::SuspendedJailed);
    assert!(require_private_authority(handler.authority_watch.as_ref()).is_err());
    drop(authority);
    assert!(require_private_authority(handler.authority_watch.as_ref()).is_err());
}
