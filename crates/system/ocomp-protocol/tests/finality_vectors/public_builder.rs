use super::*;
use std::cell::RefCell;

use commonware_codec::Encode as _;
use outbe_node::ocomp::finality::{
    PublicAccountProofV1, PublicBlockViewV1, PublicExactBlockProofSourceV1,
    PublicFinalizationBytesV1, PublicFinalizedIntentProofBuildError,
    PublicFinalizedIntentProofBuilderV1, PublicStorageProofV1,
};

#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    FailAt(usize),
    Finalization,
    HeaderHash,
    HeaderHeight,
    HeaderRoot,
    ProofAddress(usize),
    ProofSlots(usize),
    AccountNode(usize),
    StorageValue(usize),
    RecordSize,
    RecordBytes,
    DifferentIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReadCall {
    Finalization(u64),
    Block(B256),
    Account(Address, Vec<B256>, B256),
    Record(B256, B256),
}

struct PublicFixtureSource<'a> {
    fixture: &'a Fixture,
    fault: Fault,
    reads: RefCell<Vec<ReadCall>>,
}

impl PublicFixtureSource<'_> {
    fn record(&self, call: ReadCall) -> Result<(), std::io::Error> {
        let mut reads = self.reads.borrow_mut();
        reads.push(call);
        if matches!(self.fault, Fault::FailAt(index) if index == reads.len()) {
            return Err(std::io::Error::other("injected public read failure"));
        }
        Ok(())
    }

    fn account_ordinal(&self) -> usize {
        self.reads
            .borrow()
            .iter()
            .filter(|call| matches!(call, ReadCall::Account(..)))
            .count()
    }

    fn alter_account(&self, proof: &mut PublicAccountProofV1) {
        let ordinal = self.account_ordinal();
        match self.fault {
            Fault::ProofAddress(index) if index == ordinal => proof.address = Address::ZERO,
            Fault::ProofSlots(index) if index == ordinal => {
                proof.storage_proofs.pop();
            }
            Fault::AccountNode(index) if index == ordinal => {
                proof.account_nodes[0] = Bytes::from_static(&[0])
            }
            Fault::StorageValue(index) if index == ordinal => {
                proof.storage_proofs[0].value ^= U256::from(1)
            }
            _ => {}
        }
    }
}

impl PublicExactBlockProofSourceV1 for PublicFixtureSource<'_> {
    type Error = std::io::Error;

    fn finalization(&self, height: u64) -> Result<PublicFinalizationBytesV1, Self::Error> {
        self.record(ReadCall::Finalization(height))?;
        let mut bytes = PublicFinalizationBytesV1 {
            finalization_bytes: self.fixture.finalization_record.encoded_proof.to_vec(),
            block_bytes: self.fixture.block.encode().to_vec(),
        };
        if matches!(self.fault, Fault::Finalization) {
            bytes.finalization_bytes.clear();
        }
        Ok(bytes)
    }

    fn block_by_hash(&self, hash: B256) -> Result<PublicBlockViewV1, Self::Error> {
        self.record(ReadCall::Block(hash))?;
        let mut view = PublicBlockViewV1 {
            hash: self.fixture.header_hash,
            state_root: self.fixture.state_root,
            number: FINALIZED_BLOCK_NUMBER,
        };
        match self.fault {
            Fault::HeaderHash => view.hash = B256::ZERO,
            Fault::HeaderHeight => view.number += 1,
            Fault::HeaderRoot => view.state_root = B256::ZERO,
            _ => {}
        }
        Ok(view)
    }

    fn job_record(&self, intent: B256, hash: B256) -> Result<Vec<u8>, Self::Error> {
        self.record(ReadCall::Record(intent, hash))?;
        match self.fault {
            Fault::RecordSize => return Ok(vec![0; LIMITS.max_bounded_bytes + 1]),
            Fault::RecordBytes => return Ok(vec![0]),
            _ => {}
        }
        let mut record = OcompJobRecordV1 {
            intent: self.fixture.intent.clone(),
            intent_height: self.fixture.intent.logical_evaluation_height,
            status: OcompJobStatus::AwaitingFinality,
            finalized: None,
            terminal: None,
        };
        if matches!(self.fault, Fault::DifferentIntent) {
            record.intent.chain_id ^= 1;
        }
        record
            .encode_canonical(&LIMITS)
            .map_err(std::io::Error::other)
    }

    fn account_proof(
        &self,
        address: Address,
        slots: &[B256],
        hash: B256,
    ) -> Result<PublicAccountProofV1, Self::Error> {
        self.record(ReadCall::Account(address, slots.to_vec(), hash))?;
        let proof = self
            .fixture
            .provider
            .state
            .proof(TrieInput::default(), address, slots)
            .map_err(std::io::Error::other)?;
        let account = proof.info.expect("independent fixture has this account");
        let mut public = PublicAccountProofV1 {
            address: proof.address,
            nonce: account.nonce,
            balance: account.balance,
            storage_root: proof.storage_root,
            code_hash: account.get_bytecode_hash(),
            account_nodes: proof.proof,
            storage_proofs: proof
                .storage_proofs
                .into_iter()
                .map(|proof| PublicStorageProofV1 {
                    key: proof.key,
                    value: proof.value,
                    nodes: proof.proof,
                })
                .collect(),
        };
        self.alter_account(&mut public);
        Ok(public)
    }
}

fn source(fixture: &Fixture, fault: Fault) -> PublicFixtureSource<'_> {
    PublicFixtureSource {
        fixture,
        fault,
        reads: RefCell::new(Vec::new()),
    }
}

#[test]
fn public_builder_matches_independent_canonical_proof_and_exact_reads() {
    let fixture = fixture(&[0, 1, 2]);
    let source = source(&fixture, Fault::None);
    let (built, verified) = PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
        .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, fixture.expected)
        .expect("public source must produce the independent canonical proof");
    assert_eq!(built, fixture.proof);
    assert_eq!(
        built.encode_canonical(&LIMITS).unwrap(),
        fixture.proof.encode_canonical(&LIMITS).unwrap()
    );
    assert_eq!(verified.intent_id, fixture.intent_id);
    let reads = source.reads.borrow();
    assert_eq!(reads.len(), 7);
    assert_eq!(reads[0], ReadCall::Finalization(FINALIZED_BLOCK_NUMBER));
    assert_eq!(reads[1], ReadCall::Block(fixture.header_hash));
    assert_eq!(
        reads[5],
        ReadCall::Record(fixture.intent_id, fixture.header_hash)
    );
    for index in [2, 3, 4, 6] {
        let ReadCall::Account(address, slots, hash) = &reads[index] else {
            panic!("expected exact-block account read")
        };
        assert_eq!(*hash, fixture.header_hash);
        assert_eq!(
            *address,
            if index == 6 {
                METADOSIS_ADDRESS
            } else {
                VALIDATOR_SET_ADDRESS
            }
        );
        assert!(!slots.is_empty());
    }
}

#[test]
fn public_builder_stops_at_each_source_failure() {
    let fixture = fixture(&[0, 1, 2]);
    for index in 1..=7 {
        let source = source(&fixture, Fault::FailAt(index));
        let error = PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
            .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, fixture.expected)
            .unwrap_err();
        assert!(matches!(
            error,
            PublicFinalizedIntentProofBuildError::Source(_)
        ));
        assert_eq!(source.reads.borrow().len(), index);
    }
}

#[test]
fn public_builder_rejects_transport_corruption_before_later_reads() {
    let fixture = fixture(&[0, 1, 2]);
    let cases = [
        (Fault::Finalization, 1),
        (Fault::HeaderHash, 2),
        (Fault::HeaderHeight, 2),
        (Fault::HeaderRoot, 2),
        (Fault::RecordSize, 6),
        (Fault::RecordBytes, 6),
        (Fault::DifferentIntent, 6),
    ];
    for (fault, expected_reads) in cases {
        let source = source(&fixture, fault);
        assert!(
            PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
                .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, fixture.expected)
                .is_err(),
            "{fault:?}"
        );
        assert_eq!(source.reads.borrow().len(), expected_reads, "{fault:?}");
    }
}

#[test]
fn public_builder_authenticates_every_account_and_storage_stage() {
    let fixture = fixture(&[0, 1, 2]);
    for index in 1..=4 {
        for fault in [
            Fault::ProofAddress(index),
            Fault::ProofSlots(index),
            Fault::AccountNode(index),
            Fault::StorageValue(index),
        ] {
            let source = source(&fixture, fault);
            assert!(
                PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
                    .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, fixture.expected)
                    .is_err(),
                "{fault:?}"
            );
            assert_eq!(
                source.reads.borrow().len(),
                if index == 4 { 7 } else { index + 2 },
                "{fault:?}"
            );
        }
    }
}

#[test]
fn public_builder_enforces_caps_before_later_reads_and_checks_expected_binding_last() {
    let fixture = fixture(&[0, 1, 2]);
    let source = source(&fixture, Fault::None);
    let mut limits = LIMITS;
    limits.max_proof_bytes = 1;
    let error = PublicFinalizedIntentProofBuilderV1::new(&source, limits)
        .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, fixture.expected)
        .unwrap_err();
    assert!(matches!(
        error,
        PublicFinalizedIntentProofBuildError::VrfKeyLength
    ));
    assert_eq!(source.reads.borrow().len(), 4);
    source.reads.borrow_mut().clear();
    let mut expected = fixture.expected;
    expected.chain_id ^= 1;
    let error = PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
        .build_and_verify(FINALIZED_BLOCK_NUMBER, fixture.intent_id, expected)
        .unwrap_err();
    assert!(matches!(
        error,
        PublicFinalizedIntentProofBuildError::Verification(_)
    ));
    assert_eq!(source.reads.borrow().len(), 7);
}
