//! Stack-wide bounded individual-vote verification and execution bridge handoff.
mod admission;

use crate::{digest::Digest, hybrid::HybridScheme, metrics::EquivocationKind};
use admission::{Admission, Queued};
use alloy_primitives::{keccak256, B256};
use commonware_codec::{Encode, ReadExt};
use commonware_consensus::{
    simplex::types::{Attributable, Finalize, Notarize, Nullify, Proposal},
    types::{Epoch, Round},
    Epochable,
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::{Attestation, Scheme},
    Verifier,
};
use commonware_utils::{ordered::Quorum as _, Participant};
use outbe_primitives::equivocation_evidence::{
    canonical_equivocation_evidence_hash, pack_equivocation_evidence_block,
    EquivocationEvidenceMailbox, EquivocationPushOutcome, VerifiedEquivocationEvidence,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

#[derive(Clone)]
pub(crate) struct RawEquivocationEvidence {
    kind: EquivocationKind,
    epoch: Epoch,
    signer: Participant,
    bytes: Vec<u8>,
    scheme: Arc<HybridScheme<MinSig>>,
}

impl RawEquivocationEvidence {
    pub(crate) fn new(
        kind: EquivocationKind,
        conflict: &(impl Attributable + Epochable + Encode),
        scheme: Arc<HybridScheme<MinSig>>,
    ) -> Self {
        Self {
            kind,
            epoch: conflict.epoch(),
            signer: conflict.signer(),
            bytes: conflict.encode().to_vec(),
            scheme,
        }
    }

    fn key(&self) -> (u64, u32) {
        (self.epoch.get(), self.signer.get())
    }
}

#[derive(Clone)]
pub struct EquivocationVerifyMailbox {
    admission: Arc<Mutex<Admission>>,
    wake: mpsc::Sender<()>,
}

impl EquivocationVerifyMailbox {
    pub(crate) fn enqueue(&self, raw: RawEquivocationEvidence) {
        if self.wake.is_closed() {
            crate::metrics::record_equivocation_verify_admission("closed");
            return;
        }
        let mut id_bytes = raw.kind.label().as_bytes().to_vec();
        id_bytes.extend_from_slice(&raw.bytes);
        let queued = Queued {
            raw_id: keccak256(id_bytes),
            raw,
        };
        if self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(queued)
        {
            let _ = self.wake.try_send(());
        }
    }
}

pub struct EquivocationVerifyActor {
    admission: Arc<Mutex<Admission>>,
    wake: mpsc::Receiver<()>,
    bridge: EquivocationEvidenceMailbox,
    verified: VecDeque<B256>,
}

impl EquivocationVerifyActor {
    pub fn new(bridge: EquivocationEvidenceMailbox) -> (Self, EquivocationVerifyMailbox) {
        let (wake, rx) = mpsc::channel(1);
        let admission = Arc::new(Mutex::new(Admission::default()));
        let mailbox = EquivocationVerifyMailbox {
            admission: admission.clone(),
            wake,
        };
        let actor = Self {
            admission,
            wake: rx,
            bridge,
            verified: VecDeque::new(),
        };
        (actor, mailbox)
    }

    pub async fn run(mut self) {
        loop {
            let next = self
                .admission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop();
            if let Some(queued) = next {
                self.process(queued.raw);
                tokio::task::yield_now().await;
            } else if self.wake.recv().await.is_none() {
                break;
            }
        }
    }

    fn process(&mut self, raw: RawEquivocationEvidence) {
        use crate::metrics::record_equivocation_verify as record;
        let evidence = match verify(&raw) {
            Ok(evidence) => evidence,
            Err(VerifyError::InvalidSignature) => {
                record(raw.kind, "invalid_signature");
                return;
            }
            Err(_) => {
                record(raw.kind, "malformed_vote");
                return;
            }
        };
        let hash = evidence.evidence_hash;
        if self.verified.contains(&hash) {
            record(raw.kind, "duplicate_verified");
            return;
        }
        let outcome = self.bridge.push(evidence);
        record(
            raw.kind,
            match outcome {
                EquivocationPushOutcome::Enqueued => "verified",
                EquivocationPushOutcome::Duplicate => "bridge_duplicate",
                EquivocationPushOutcome::Full => "bridge_full",
            },
        );
        if outcome != EquivocationPushOutcome::Full {
            if self.verified.len() == 512 {
                self.verified.pop_front();
            }
            self.verified.push_back(hash);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum VerifyError {
    #[error("malformed consensus vote")]
    Decode(
        #[from]
        #[source]
        commonware_codec::Error,
    ),
    #[error("invalid individual vote signature")]
    InvalidSignature,
    #[error("inconsistent equivocation evidence")]
    InconsistentEvidence,
}

struct Vote {
    round: Round,
    attestation: Attestation<HybridScheme<MinSig>>,
    payload: Vec<u8>,
}

fn read_vote(
    bytes: &mut commonware_codec::Copying<'_>,
    kind: EquivocationKind,
    first: bool,
) -> Result<Vote, VerifyError> {
    let (round, attestation, payload) = match kind {
        EquivocationKind::ConflictingNotarize => {
            let v = Notarize::<HybridScheme<MinSig>, Digest>::read(bytes)?;
            (
                v.proposal.round,
                v.attestation,
                v.proposal.encode().to_vec(),
            )
        }
        EquivocationKind::NullifyFinalize if first => {
            let v = Nullify::<HybridScheme<MinSig>>::read(bytes)?;
            (v.round, v.attestation, v.round.encode().to_vec())
        }
        EquivocationKind::ConflictingFinalize | EquivocationKind::NullifyFinalize => {
            let v = Finalize::<HybridScheme<MinSig>, Digest>::read(bytes)?;
            (
                v.proposal.round,
                v.attestation,
                v.proposal.encode().to_vec(),
            )
        }
    };
    Ok(Vote {
        round,
        attestation,
        payload,
    })
}

fn verify(raw: &RawEquivocationEvidence) -> Result<VerifiedEquivocationEvidence, VerifyError> {
    let mut bytes = commonware_codec::Copying(raw.bytes.as_slice());
    let first = read_vote(&mut bytes, raw.kind, true)?;
    let second = read_vote(&mut bytes, raw.kind, false)?;
    let same_context = (
        first.round,
        first.round.epoch(),
        first.attestation.signer,
        second.attestation.signer,
    ) == (second.round, raw.epoch, raw.signer, raw.signer);
    let same_proposal =
        raw.kind != EquivocationKind::NullifyFinalize && first.payload == second.payload;
    if !bytes.0.is_empty() || !same_context || same_proposal {
        return Err(VerifyError::InconsistentEvidence);
    }
    let pk = raw
        .scheme
        .participants()
        .key(raw.signer)
        .ok_or(VerifyError::InconsistentEvidence)?;
    let pubkey = pk
        .encode()
        .as_ref()
        .try_into()
        .map_err(|_| VerifyError::InconsistentEvidence)?;
    let first_block = verify_block(raw, pk, &pubkey, first, true)?;
    let second_block = verify_block(raw, pk, &pubkey, second, false)?;
    let evidence = VerifiedEquivocationEvidence {
        kind: raw.kind,
        epoch: raw.epoch.get(),
        accused_pubkey: pubkey,
        evidence_hash: canonical_equivocation_evidence_hash(&first_block, &second_block),
        first_block,
        second_block,
    };
    unpack_verified_equivocation_evidence(&evidence)?;
    Ok(evidence)
}

fn verify_block(
    raw: &RawEquivocationEvidence,
    pk: &bls12381::PublicKey,
    pubkey: &[u8; 48],
    vote: Vote,
    first: bool,
) -> Result<Vec<u8>, VerifyError> {
    let participants = raw.scheme.participants();
    let namespace = match raw.kind {
        EquivocationKind::ConflictingNotarize => crate::proof::notarize_namespace(participants),
        EquivocationKind::NullifyFinalize if first => crate::proof::nullify_namespace(participants),
        EquivocationKind::ConflictingFinalize | EquivocationKind::NullifyFinalize => {
            crate::proof::finalize_namespace(participants)
        }
    };
    let sig = &vote
        .attestation
        .signature
        .get()
        .ok_or(VerifyError::InvalidSignature)?
        .bls_individual_vote;
    if !pk.verify(&namespace, &vote.payload, sig) {
        return Err(VerifyError::InvalidSignature);
    }
    let sig = sig.encode();
    let sig = sig
        .as_ref()
        .try_into()
        .map_err(|_| VerifyError::InconsistentEvidence)?;
    Ok(pack_equivocation_evidence_block(pubkey, sig, &vote.payload))
}

pub(crate) fn unpack_verified_equivocation_evidence(
    evidence: &VerifiedEquivocationEvidence,
) -> Result<(), VerifyError> {
    let mut rounds = Vec::with_capacity(2);
    for (first, block) in [
        (true, &evidence.first_block),
        (false, &evidence.second_block),
    ] {
        if block.len() <= 144 || block[..48] != evidence.accused_pubkey {
            return Err(VerifyError::InconsistentEvidence);
        }
        let mut bytes = commonware_codec::Copying(&block[144..]);
        let round = if evidence.kind == EquivocationKind::NullifyFinalize && first {
            Round::read(&mut bytes)?
        } else {
            Proposal::<Digest>::read(&mut bytes)?.round
        };
        if !bytes.0.is_empty() || round.epoch().get() != evidence.epoch {
            return Err(VerifyError::InconsistentEvidence);
        }
        rounds.push(round);
    }
    if rounds[0] != rounds[1]
        || (evidence.kind != EquivocationKind::NullifyFinalize
            && evidence.first_block[144..] == evidence.second_block[144..])
        || evidence.evidence_hash
            != canonical_equivocation_evidence_hash(&evidence.first_block, &evidence.second_block)
    {
        return Err(VerifyError::InconsistentEvidence);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
