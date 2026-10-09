//! Public ceremony transcripts preserve request order and founding participation.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use alloy_primitives::B256;
use outbe_tee::{
    errors::TransportError,
    protocol::{EnclaveRequest, EnclaveResponse},
    tee_dkg::{
        run_tee_dkg_ceremony, Ack, CeremonyCoordinator, CeremonyError, CeremonyOutcome,
        DealerBundle, DkgGossip, DkgWireMessage, EnclaveChannel, FinalizedLog,
        FoundingCeremonyParameters,
    },
};

const CEREMONY: B256 = B256::repeat_byte(7);
const CHAIN: B256 = B256::repeat_byte(8);
const ME: u8 = 2;
const PEER: u8 = 1;

enum Step {
    Request(Box<EnclaveRequest>, Box<EnclaveResponse>),
    Send(Vec<u8>, DkgWireMessage),
    Broadcast(DkgWireMessage),
    Receive(DkgWireMessage),
    Closed,
}

fn request(req: EnclaveRequest, response: EnclaveResponse) -> Step {
    Step::Request(Box::new(req), Box::new(response))
}

#[derive(Default)]
struct Transcript {
    steps: VecDeque<Step>,
    mismatch: bool,
}

#[derive(Clone)]
struct Script(Rc<RefCell<Transcript>>);

impl Script {
    fn take(&self) -> Option<Step> {
        self.0.borrow_mut().steps.pop_front()
    }

    fn reject(&self) -> CeremonyError {
        self.0.borrow_mut().mismatch = true;
        CeremonyError::Delivery("unexpected transcript operation".into())
    }

    async fn run(steps: Vec<Step>, participants: usize) -> Result<CeremonyOutcome, CeremonyError> {
        let script = Self(Rc::new(RefCell::new(Transcript {
            steps: steps.into(),
            mismatch: false,
        })));
        let mut enclave = script.clone();
        let mut gossip = script.clone();
        let coordinator = CeremonyCoordinator::new(CEREMONY, 9, vec![ME], Vec::new());
        let result = run_tee_dkg_ceremony(
            &coordinator,
            &mut enclave,
            &mut gossip,
            FoundingCeremonyParameters {
                participant_count: participants,
                chain_id: CHAIN,
                tribute_offer_epoch: 17,
            },
        )
        .await;
        let trace = script.0.borrow();
        assert!(
            !trace.mismatch,
            "operation type differs from the transcript"
        );
        assert!(
            trace.steps.is_empty(),
            "ceremony omitted transcript operations"
        );
        result
    }
}

impl EnclaveChannel for Script {
    fn request(&mut self, actual: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        if let Some(Step::Request(expected, response)) = self.take() {
            assert_eq!(actual, expected.as_ref());
            Ok(*response)
        } else {
            self.0.borrow_mut().mismatch = true;
            Err(TransportError::UnexpectedResponse)
        }
    }
}

impl DkgGossip for Script {
    async fn send(&mut self, to: &[u8], actual: DkgWireMessage) -> Result<(), CeremonyError> {
        if let Some(Step::Send(expected_to, expected)) = self.take() {
            assert_eq!(to, expected_to);
            assert_eq!(actual, expected);
            Ok(())
        } else {
            Err(self.reject())
        }
    }

    async fn broadcast(&mut self, actual: DkgWireMessage) -> Result<(), CeremonyError> {
        if let Some(Step::Broadcast(expected)) = self.take() {
            assert_eq!(actual, expected);
            Ok(())
        } else {
            Err(self.reject())
        }
    }

    async fn recv(&mut self) -> Option<(Vec<u8>, DkgWireMessage)> {
        match self.take() {
            Some(Step::Receive(message)) => Some((vec![0xff], message)),
            Some(Step::Closed) => None,
            _ => {
                self.0.borrow_mut().mismatch = true;
                None
            }
        }
    }
}

fn bundle(dealer: u8) -> DealerBundle {
    DealerBundle {
        dealer_bls: vec![dealer],
        pub_msg: vec![10],
        sealed_share: vec![11],
    }
}

fn ingest(dealer: u8, ack: Option<Vec<u8>>) -> Step {
    request(
        EnclaveRequest::DkgPlayerIngest {
            ceremony_id: CEREMONY,
            dealer_bls: vec![dealer],
            pub_msg: vec![10],
            sealed_share: vec![11],
        },
        EnclaveResponse::DkgPlayerAck { ack },
    )
}

fn record_ack(player: u8, ack: u8) -> Step {
    request(
        EnclaveRequest::DkgDealerReceiveAck {
            ceremony_id: CEREMONY,
            player_bls: vec![player],
            ack: vec![ack],
        },
        EnclaveResponse::Ack,
    )
}

fn log(dealer: u8, byte: u8) -> DkgWireMessage {
    DkgWireMessage::FinalizedLog(FinalizedLog {
        dealer_bls: vec![dealer],
        signed_log: vec![byte],
    })
}

fn partial(signer: u8, recipient: u8, byte: u8) -> DkgWireMessage {
    DkgWireMessage::TributeOfferPartial {
        signer_bls: vec![signer],
        recipient_bls: vec![recipient],
        partial: vec![byte],
    }
}

fn opening(two_participants: bool) -> Vec<Step> {
    let mut shares = vec![(vec![ME], vec![11])];
    if two_participants {
        shares.push((vec![PEER], vec![12]));
    }
    let mut steps = vec![
        request(
            EnclaveRequest::DkgOpen {
                ceremony_id: CEREMONY,
                round: 9,
                participants: Vec::new(),
            },
            EnclaveResponse::Ack,
        ),
        request(
            EnclaveRequest::DkgStartDealer {
                ceremony_id: CEREMONY,
            },
            EnclaveResponse::DkgDealt {
                pub_msg: vec![10],
                sealed_shares: shares,
            },
        ),
        ingest(ME, Some(vec![13])),
    ];
    if two_participants {
        let mut remote = bundle(ME);
        remote.sealed_share = vec![12];
        steps.push(Step::Send(vec![PEER], DkgWireMessage::DealerBundle(remote)));
    }
    steps.push(record_ack(ME, 13));
    steps
}

fn own_log() -> [Step; 2] {
    [
        request(
            EnclaveRequest::DkgDealerFinalize {
                ceremony_id: CEREMONY,
            },
            EnclaveResponse::DkgSignedLog {
                signed_log: vec![31],
            },
        ),
        Step::Broadcast(log(ME, 31)),
    ]
}

fn recover_share(two_participants: bool) -> [Step; 2] {
    let logs = if two_participants {
        vec![vec![51], vec![31]]
    } else {
        vec![vec![31]]
    };
    let sealed = if two_participants {
        vec![(vec![PEER], vec![62]), (vec![ME], vec![61])]
    } else {
        vec![(vec![ME], vec![61])]
    };
    [
        request(
            EnclaveRequest::DkgPlayerFinalize {
                ceremony_id: CEREMONY,
                signed_logs: logs,
            },
            EnclaveResponse::DkgPlayerFinalized {
                group_public: vec![70],
                share_commitment: B256::repeat_byte(71),
            },
        ),
        request(
            EnclaveRequest::DkgTributeOfferPartial {
                ceremony_id: CEREMONY,
            },
            EnclaveResponse::DkgTributeOfferPartial { sealed },
        ),
    ]
}

fn install_offer(partials: Vec<Vec<u8>>) -> Step {
    request(
        EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id: CEREMONY,
            sealed_partials: partials,
            chain_id: CHAIN,
            tribute_offer_epoch: 17,
        },
        EnclaveResponse::DkgTributeOfferKey {
            tribute_offer_public: [72; 32],
            group_public_key: vec![73],
        },
    )
}

fn assert_outcome(outcome: CeremonyOutcome) {
    assert_eq!(
        outcome,
        CeremonyOutcome {
            group_public: vec![70],
            share_commitment: B256::repeat_byte(71),
            tribute_offer_public: [72; 32],
            tribute_offer_group_public_key: vec![73],
        }
    );
}

#[tokio::test]
async fn one_participant_completes_local_requests_in_order() -> Result<(), CeremonyError> {
    let mut steps = opening(false);
    steps.extend(own_log());
    steps.extend(recover_share(false));
    steps.push(install_offer(vec![vec![61]]));
    assert_outcome(Script::run(steps, 1).await?);
    Ok(())
}

#[tokio::test]
async fn accepted_dealers_are_deduplicated_and_rejected_dealers_can_retry(
) -> Result<(), CeremonyError> {
    let mut steps = opening(true);
    steps.extend([
        Step::Receive(DkgWireMessage::DealerBundle(bundle(PEER))),
        ingest(PEER, Some(vec![23])),
        Step::Send(
            vec![PEER],
            DkgWireMessage::Ack(Ack {
                player_bls: vec![ME],
                ack: vec![23],
            }),
        ),
        Step::Receive(DkgWireMessage::DealerBundle(bundle(PEER))),
        Step::Receive(DkgWireMessage::DealerBundle(bundle(3))),
        ingest(3, None),
        Step::Receive(DkgWireMessage::DealerBundle(bundle(3))),
        ingest(3, Some(vec![24])),
        Step::Send(
            vec![3],
            DkgWireMessage::Ack(Ack {
                player_bls: vec![ME],
                ack: vec![24],
            }),
        ),
        Step::Closed,
    ]);
    assert!(matches!(
        Script::run(steps, 2).await,
        Err(CeremonyError::UnexpectedResponse(
            "gossip closed before ceremony completed"
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn early_partials_overwrite_by_signer_and_recovery_uses_sorted_keys(
) -> Result<(), CeremonyError> {
    let mut steps = opening(true);
    steps.extend([
        Step::Receive(partial(PEER, ME, 42)),
        Step::Receive(partial(PEER, ME, 43)),
        Step::Receive(partial(3, PEER, 99)),
        Step::Receive(log(PEER, 50)),
        Step::Receive(log(PEER, 51)),
        Step::Receive(DkgWireMessage::Ack(Ack {
            player_bls: vec![ME],
            ack: vec![13],
        })),
        Step::Receive(DkgWireMessage::Ack(Ack {
            player_bls: vec![PEER],
            ack: vec![14],
        })),
        record_ack(PEER, 14),
    ]);
    steps.extend(own_log());
    steps.extend(recover_share(true));
    steps.push(Step::Broadcast(partial(ME, PEER, 62)));
    steps.push(install_offer(vec![vec![43], vec![61]]));
    assert_outcome(Script::run(steps, 2).await?);
    Ok(())
}

#[tokio::test]
async fn offer_phase_waits_for_every_participant_and_ignores_late_dkg() -> Result<(), CeremonyError>
{
    let mut steps = opening(true);
    steps.extend([
        Step::Receive(log(PEER, 51)),
        Step::Receive(DkgWireMessage::Ack(Ack {
            player_bls: vec![PEER],
            ack: vec![14],
        })),
        record_ack(PEER, 14),
    ]);
    steps.extend(own_log());
    steps.extend(recover_share(true));
    steps.extend([
        Step::Broadcast(partial(ME, PEER, 62)),
        Step::Receive(log(PEER, 99)),
        Step::Receive(partial(PEER, 3, 99)),
        Step::Closed,
    ]);
    assert!(matches!(
        Script::run(steps, 2).await,
        Err(CeremonyError::UnexpectedResponse(
            "gossip closed before founding offer-key finalization completed"
        ))
    ));
    Ok(())
}
