//! Private RPC authority revokes synchronously and reopens without losing resident keys.
use super::*;
use outbe_core_common::confidential_tee::{derive_account_keys_message, eip191_hash, Ledger};
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_tee::authority::{TeeAuthorityChannel, TeeAuthorityState as State};
use std::sync::Arc;

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
    let rpc = OutbeDomainRpcHandler::new()
        .with_authority_watch(authority.subscribe())
        .into_rpc();
    let signer = OutbeEvmSigner::from_secret_bytes([0x55; 32]).unwrap();
    let owner = signer.address();
    let eph_secret = x25519_dalek::StaticSecret::from([0x33; 32]);
    let eph = B256::from(x25519_dalek::PublicKey::from(&eph_secret).to_bytes());
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
    let handler = OutbeDomainRpcHandler::new();
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

#[derive(Clone, Copy)]
enum FreshDoor {
    UpgradeKey,
    DeriveKeys,
    Signer,
}

#[derive(Clone, Copy)]
struct ClosedAdmission {
    height: u64,
    jailed: bool,
    valid_until: u64,
    state: State,
}

fn closed_admissions() -> [ClosedAdmission; 2] {
    [
        ClosedAdmission {
            height: 8,
            jailed: true,
            valid_until: 1_800_001_000,
            state: State::SuspendedJailed,
        },
        ClosedAdmission {
            height: 9,
            jailed: false,
            valid_until: 1_800_000_000,
            state: State::SuspendedLeaseExpired,
        },
    ]
}

async fn one_door_closes_from_fresh_armed(door: FreshDoor, closed: ClosedAdmission) {
    use crate::admission::TeeLeaseGuardGateV1;
    use crate::admission_authority::live::{LiveTeeRuntimeV1, LiveTeeSnapshotV1};
    use crate::admission_authority::test_support as support;
    use outbe_consensus::protocol_state::EngineProtocolState;
    use outbe_consensus::stack::epoch::signer::authority_signer_enabled;
    use outbe_radicle::integration::RadicleVotingGate;
    use outbe_rpc::state_reader::OutbeRpcStateReader;
    use outbe_rpc::OutbeApiServer;
    use outbe_tee::authority::{install_tee_authority_watch, tee_authority_test_guard};
    use std::sync::Mutex;

    let context = support::context(9);
    let initial = support::provider(&context, 7, 1_800_000_000, 1_800_001_000, false);
    let provider = support::SharedProvider(std::sync::Arc::new(Mutex::new(initial.0)));
    let channel = TeeAuthorityChannel::default();
    let owner = std::sync::Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity: initial.2,
                gate: TeeLeaseGuardGateV1::Armed,
                context: std::sync::Arc::clone(&context),
            },
            channel.clone(),
        )
        .unwrap(),
    );
    let protocol: std::sync::Arc<dyn EngineProtocolState> =
        std::sync::Arc::new(crate::OutbeRuntime::new());
    owner.bind_sync_refresh(
        provider.clone(),
        std::sync::Arc::clone(&protocol),
        initial.1,
    );
    owner
        .refresh(&provider, protocol.as_ref(), &initial.1)
        .unwrap();
    channel.note_observed_finalized_height(7);
    let _port = tee_authority_test_guard();
    install_tee_authority_watch(channel.subscribe()).unwrap();
    let resident = context.network_encryption_public().unwrap();
    let _runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
        std::sync::Arc::clone(&context),
    ));
    let admission = std::sync::Arc::new(provider.clone());
    let reader: std::sync::Arc<dyn OutbeRpcStateReader> =
        std::sync::Arc::new(crate::OutbeRuntime::new());
    let upgrade = outbe_rpc::OutbeApiHandler::new(std::sync::Arc::clone(&admission), reader);
    let domain = OutbeDomainRpcHandler::new().with_authority_watch(channel.subscribe());
    let signer = OutbeEvmSigner::from_secret_bytes([0x55; 32]).unwrap();
    let account = signer.address();
    let eph = B256::from(
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([0x33; 32])).to_bytes(),
    );
    let nod_signature = Bytes::copy_from_slice(
        &signer
            .sign_hash(&eip191_hash(&derive_account_keys_message(
                Ledger::Nod,
                account,
                eph,
            )))
            .unwrap(),
    );
    let proof = outbe_tee::upgrade_transfer::UpgradeKeyProofV1 {
        anchor_outcome: Bytes::new(),
        committee_transitions: Vec::new(),
        admission: Bytes::new(),
    };
    let watch = channel.subscribe();
    match door {
        FreshDoor::UpgradeKey => {
            let open_error = upgrade
                .upgrade_key_v1(Bytes::new(), proof.clone(), false)
                .await
                .unwrap_err();
            assert_ne!(open_error.message(), "PrivateExecutionUnavailable");
        }
        FreshDoor::DeriveKeys => {
            let sealed = domain
                .derive_keys_checked(Ledger::Nod, account, eph, nod_signature.clone())
                .await
                .unwrap();
            assert_eq!(sealed.nonce.len(), 12);
        }
        FreshDoor::Signer => {
            assert!(authority_signer_enabled(
                Some(&watch),
                true,
                true,
                RadicleVotingGate::SignerAllowed,
            )
            .unwrap());
        }
    }

    *provider.0.lock().expect("provider lock") = support::provider(
        &context,
        closed.height,
        1_800_000_000,
        closed.valid_until,
        closed.jailed,
    )
    .0;
    channel.note_observed_finalized_height(closed.height);
    // Cached verdict is still the armed publication. This door is the first refresh after the shift.
    assert!(channel.snapshot().is_authority_open());
    match door {
        FreshDoor::UpgradeKey => {
            let closed_error = upgrade
                .upgrade_key_v1(Bytes::new(), proof, false)
                .await
                .unwrap_err();
            assert_eq!(closed_error.message(), "PrivateExecutionUnavailable");
        }
        FreshDoor::DeriveKeys => {
            let error = domain
                .derive_keys_checked(Ledger::Nod, account, eph, nod_signature)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("PrivateExecutionUnavailable"));
        }
        FreshDoor::Signer => {
            assert!(!authority_signer_enabled(
                Some(&watch),
                true,
                true,
                RadicleVotingGate::SignerAllowed,
            )
            .unwrap());
        }
    }
    let verdict = channel.snapshot();
    assert_eq!(verdict, closed.state);
    assert!(verdict.has_verified_resident_key());
    assert!(!verdict.is_authority_open());
    assert_eq!(context.network_encryption_public().unwrap(), resident);
    assert!(context.offer_key_ready().unwrap());
}

#[tokio::test]
async fn each_door_closes_from_its_own_fresh_armed_fixture() {
    for closed in closed_admissions() {
        for door in [
            FreshDoor::UpgradeKey,
            FreshDoor::DeriveKeys,
            FreshDoor::Signer,
        ] {
            one_door_closes_from_fresh_armed(door, closed).await;
        }
    }
}
