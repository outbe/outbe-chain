use super::*;

#[derive(Clone, Copy)]
enum AdmissionAttemptScriptV1 {
    Success,
    ConnectionFault,
    LostFinishResponse,
    DeterministicFailure,
}

struct ScriptedAdmissionRecoveryIoV1 {
    expected: [u8; 32],
    attempts: VecDeque<AdmissionAttemptScriptV1>,
    probes: VecDeque<JoinOfferKeyState>,
    upload_count: usize,
    reconnect_count: usize,
    probe_count: usize,
}

impl ScriptedAdmissionRecoveryIoV1 {
    fn new(
        attempts: impl IntoIterator<Item = AdmissionAttemptScriptV1>,
        probes: impl IntoIterator<Item = JoinOfferKeyState>,
    ) -> Self {
        Self {
            expected: [0x91; 32],
            attempts: attempts.into_iter().collect(),
            probes: probes.into_iter().collect(),
            upload_count: 0,
            reconnect_count: 0,
            probe_count: 0,
        }
    }
}

impl FinalizedAdmissionRecoveryIoV1 for ScriptedAdmissionRecoveryIoV1 {
    fn upload_once(
        &mut self,
    ) -> impl std::future::Future<Output = std::result::Result<[u8; 32], FinalizedAdmissionAttemptErrorV1>>
    {
        self.upload_count += 1;
        let expected = self.expected;
        let event = self.attempts.pop_front().expect("scripted upload attempt");
        async move {
            match event {
                AdmissionAttemptScriptV1::Success => Ok(expected),
                AdmissionAttemptScriptV1::ConnectionFault
                | AdmissionAttemptScriptV1::LostFinishResponse => {
                    Err(TransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "scripted connection fault",
                    ))
                    .into())
                }
                AdmissionAttemptScriptV1::DeterministicFailure => {
                    Err(TransportError::EnclaveError("scripted rejection".into()).into())
                }
            }
        }
    }

    fn reconnect_exact(&mut self) -> std::result::Result<(), TransportError> {
        self.reconnect_count += 1;
        Ok(())
    }

    fn probe_offer_key(&mut self) -> std::result::Result<JoinOfferKeyState, TransportError> {
        self.probe_count += 1;
        Ok(self.probes.pop_front().expect("scripted recovery probe"))
    }

    fn expected_offer_key(&self) -> [u8; 32] {
        self.expected
    }
}

#[test]
fn finalized_join_anchor_binds_the_exact_finalized_view_and_registry_binding() {
    let view = FinalizedRegistryViewV1 {
        chain_id: U256::from(676_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x11),
        block_number: 91,
        block_hash: B256::repeat_byte(0x12),
        state_root: B256::repeat_byte(0x13),
        consensus_timestamp: 19_000,
    };
    let binding = outbe_operator::test_support::RenewalBindingFixtureV1::new(0x21)
        .versions(2, 3)
        .nonces(0, 0)
        .lease(18_000, 20_000, 20_000)
        .keys(0x31)
        .measurements(0x34)
        .claims(1, 2, 1)
        .authorization(B256::repeat_byte(0x37))
        .build();

    assert_eq!(
        serde_json::to_string(&binding).unwrap(),
        r#"{"nodeIdHash":"0x2121212121212121212121212121212121212121212121212121212121212121","enclaveId":"0x2222222222222222222222222222222222222222222222222222222222222222","bindingId":"0x2323232323232323232323232323232323232323232323232323232323232323","intentHash":"0x2424242424242424242424242424242424242424242424242424242424242424","evidenceHash":"0x2525252525252525252525252525252525252525252525252525252525252525","policyHash":"0x2626262626262626262626262626262626262626262626262626262626262626","bindingVersion":2,"registrationVersion":3,"renewalNonce":0,"transitionNonce":0,"leaseStartedAt":18000,"validUntil":20000,"collateralValidUntil":20000,"recipientX25519":"0x3131313131313131313131313131313131313131313131313131313131313131","attestationEd25519":"0x3232323232323232323232323232323232323232323232323232323232323232","noiseResponderX25519":"0x3333333333333333333333333333333333333333333333333333333333333333","mrenclave":"0x3434343434343434343434343434343434343434343434343434343434343434","mrsigner":"0x3535353535353535353535353535353535353535353535353535353535353535","isvProdId":1,"isvSvn":2,"platformTcbStatus":1,"verdictHash":"0x3636363636363636363636363636363636363636363636363636363636363636","nodeHostAuthorizationHash":"0x3737373737373737373737373737373737373737373737373737373737373737"}"#
    );

    assert_eq!(
        finalized_join_admission_anchor_v1(&view, &binding),
        FinalizedJoinAdmissionAnchorV1 {
            chain_id: view.chain_id,
            genesis_hash: view.genesis_hash,
            node_id_hash: binding.node_id_hash,
            enclave_id: binding.enclave_id,
            intent_hash: binding.intent_hash,
            finalized_height: view.block_number,
            finalized_hash: view.block_hash,
            finalized_state_root: view.state_root,
            finalized_consensus_timestamp: view.consensus_timestamp,
        }
    );
}

#[tokio::test]
async fn lost_finish_response_is_reconciled_as_ready_without_replay() {
    let mut io = ScriptedAdmissionRecoveryIoV1::new(
        [AdmissionAttemptScriptV1::LostFinishResponse],
        [JoinOfferKeyState::ReadyExact],
    );

    assert_eq!(
        run_finalized_admission_recovery_v1(&mut io).await.unwrap(),
        io.expected
    );
    assert_eq!(io.upload_count, 1, "committed Finish must not be replayed");
    assert_eq!(io.reconnect_count, 1);
    assert_eq!(io.probe_count, 1);
}

#[tokio::test]
async fn keyless_reconnect_replays_the_whole_upload_once() {
    let mut io = ScriptedAdmissionRecoveryIoV1::new(
        [
            AdmissionAttemptScriptV1::ConnectionFault,
            AdmissionAttemptScriptV1::Success,
        ],
        [JoinOfferKeyState::Keyless],
    );

    assert_eq!(
        run_finalized_admission_recovery_v1(&mut io).await.unwrap(),
        io.expected
    );
    assert_eq!(io.upload_count, 2);
    assert_eq!(io.reconnect_count, 1);
    assert_eq!(io.probe_count, 1);
}

#[tokio::test]
async fn second_connection_fault_is_probed_and_keyless_state_fails_closed() {
    let mut io = ScriptedAdmissionRecoveryIoV1::new(
        [
            AdmissionAttemptScriptV1::ConnectionFault,
            AdmissionAttemptScriptV1::ConnectionFault,
        ],
        [JoinOfferKeyState::Keyless, JoinOfferKeyState::Keyless],
    );

    let error = run_finalized_admission_recovery_v1(&mut io)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("replay exhausted"));
    assert_eq!(io.upload_count, 2, "whole upload is replayed at most once");
    assert_eq!(io.reconnect_count, 2);
    assert_eq!(io.probe_count, 2, "second fault must be reconciled");
}

#[tokio::test]
async fn lost_finish_response_on_the_only_replay_is_reconciled_as_ready() {
    let mut io = ScriptedAdmissionRecoveryIoV1::new(
        [
            AdmissionAttemptScriptV1::ConnectionFault,
            AdmissionAttemptScriptV1::LostFinishResponse,
        ],
        [JoinOfferKeyState::Keyless, JoinOfferKeyState::ReadyExact],
    );

    assert_eq!(
        run_finalized_admission_recovery_v1(&mut io).await.unwrap(),
        io.expected
    );
    assert_eq!(io.upload_count, 2);
    assert_eq!(io.reconnect_count, 2);
    assert_eq!(io.probe_count, 2);
}

#[tokio::test]
async fn ready_mismatch_after_reconnect_is_terminal() {
    let mut io = ScriptedAdmissionRecoveryIoV1::new(
        [AdmissionAttemptScriptV1::ConnectionFault],
        [JoinOfferKeyState::ReadyMismatch],
    );

    let error = run_finalized_admission_recovery_v1(&mut io)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match finalized TeeRegistry"));
    assert_eq!(io.upload_count, 1);
    assert_eq!(io.reconnect_count, 1);
    assert_eq!(io.probe_count, 1);
}

#[tokio::test]
async fn deterministic_enclave_rejection_is_never_reconnected_or_replayed() {
    let mut io =
        ScriptedAdmissionRecoveryIoV1::new([AdmissionAttemptScriptV1::DeterministicFailure], []);

    let error = run_finalized_admission_recovery_v1(&mut io)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("scripted rejection"));
    assert_eq!(io.upload_count, 1);
    assert_eq!(io.reconnect_count, 0);
    assert_eq!(io.probe_count, 0);
}
