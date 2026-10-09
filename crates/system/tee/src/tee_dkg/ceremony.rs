//! Host-side phases of the founding DKG ceremony.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::B256;

use super::{
    Ack, CeremonyCoordinator, CeremonyError, CeremonyOutcome, DealerBundle, DkgGossip,
    DkgWireMessage, EnclaveChannel, FinalizedLog, Result,
};

/// The participant count and chain binding of a founding ceremony.
#[derive(Debug, Clone, Copy)]
pub struct FoundingCeremonyParameters {
    pub participant_count: usize,
    pub chain_id: B256,
    pub tribute_offer_epoch: u64,
}

/// Drive the founding ceremony and keep every secret operation in the enclave.
/// All participants must supply acknowledgements, dealer logs and offer partials.
/// A cryptographic recovery quorum does not replace this founding policy.
pub async fn run_tee_dkg_ceremony<C: EnclaveChannel, G: DkgGossip>(
    coord: &CeremonyCoordinator,
    enclave: &mut C,
    gossip: &mut G,
    parameters: FoundingCeremonyParameters,
) -> Result<CeremonyOutcome> {
    coord.open(enclave)?;
    let acknowledged = distribute_dealings(coord, enclave, gossip).await?;
    let collection = DealerCollection::new(coord, parameters.participant_count, acknowledged);
    let (logs, offer_partials) = collection.collect(enclave, gossip).await?;
    let mut outcome = coord.finalize_player(enclave, &logs)?;
    let (public, group_public) = offer_partials
        .complete(coord, enclave, gossip, parameters)
        .await?;
    outcome.tribute_offer_public = public;
    outcome.tribute_offer_group_public_key = group_public;
    Ok(outcome)
}

async fn distribute_dealings<C: EnclaveChannel, G: DkgGossip>(
    coord: &CeremonyCoordinator,
    enclave: &mut C,
    gossip: &mut G,
) -> Result<BTreeSet<Vec<u8>>> {
    let mut self_ack = None;
    for bundle in coord.deal(enclave)? {
        if bundle.to == coord.my_bls() {
            self_ack = coord.ingest(enclave, &bundle.msg)?;
        } else {
            gossip
                .send(&bundle.to, DkgWireMessage::DealerBundle(bundle.msg))
                .await?;
        }
    }
    let mut acknowledged = BTreeSet::new();
    if let Some(ack) = self_ack {
        coord.receive_ack(enclave, &ack.msg)?;
        acknowledged.insert(ack.msg.player_bls.clone());
    }
    Ok(acknowledged)
}

struct DealerCollection<'a> {
    coord: &'a CeremonyCoordinator,
    required: usize,
    acknowledged: BTreeSet<Vec<u8>>,
    finalized: bool,
    logs: BTreeMap<Vec<u8>, FinalizedLog>,
    ingested: BTreeSet<Vec<u8>>,
    offer_partials: OfferPartials,
}

impl<'a> DealerCollection<'a> {
    fn new(
        coord: &'a CeremonyCoordinator,
        required: usize,
        acknowledged: BTreeSet<Vec<u8>>,
    ) -> Self {
        Self {
            coord,
            required,
            acknowledged,
            finalized: false,
            logs: BTreeMap::new(),
            ingested: BTreeSet::new(),
            offer_partials: OfferPartials {
                recipient: coord.my_bls().to_vec(),
                sealed: BTreeMap::new(),
            },
        }
    }

    async fn collect<C: EnclaveChannel, G: DkgGossip>(
        mut self,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<(Vec<FinalizedLog>, OfferPartials)> {
        self.finalize_dealing(enclave, gossip).await?;
        while self.logs.len() < self.required {
            let Some((_from, message)) = gossip.recv().await else {
                return Err(CeremonyError::UnexpectedResponse(
                    "gossip closed before ceremony completed",
                ));
            };
            self.receive(message, enclave, gossip).await?;
        }
        Ok((self.logs.into_values().collect(), self.offer_partials))
    }

    async fn receive<C: EnclaveChannel, G: DkgGossip>(
        &mut self,
        message: DkgWireMessage,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<()> {
        match message {
            DkgWireMessage::DealerBundle(bundle) => {
                self.accept_dealing(bundle, enclave, gossip).await?
            }
            DkgWireMessage::Ack(ack) => self.accept_ack(ack, enclave, gossip).await?,
            DkgWireMessage::FinalizedLog(log) => {
                self.logs.insert(log.dealer_bls.clone(), log);
            }
            partial @ DkgWireMessage::TributeOfferPartial { .. } => {
                self.offer_partials.record(partial);
            }
        }
        Ok(())
    }

    async fn accept_dealing<C: EnclaveChannel, G: DkgGossip>(
        &mut self,
        bundle: DealerBundle,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<()> {
        if self.ingested.contains(&bundle.dealer_bls) {
            return Ok(());
        }
        let Some(ack) = self.coord.ingest(enclave, &bundle)? else {
            return Ok(());
        };
        self.ingested.insert(bundle.dealer_bls.clone());
        gossip.send(&ack.to, DkgWireMessage::Ack(ack.msg)).await
    }

    async fn accept_ack<C: EnclaveChannel, G: DkgGossip>(
        &mut self,
        ack: Ack,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<()> {
        if !self.acknowledged.contains(&ack.player_bls) {
            self.coord.receive_ack(enclave, &ack)?;
            self.acknowledged.insert(ack.player_bls.clone());
        }
        self.finalize_dealing(enclave, gossip).await
    }

    async fn finalize_dealing<C: EnclaveChannel, G: DkgGossip>(
        &mut self,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<()> {
        if self.finalized || self.acknowledged.len() < self.required {
            return Ok(());
        }
        self.finalized = true;
        let log = self.coord.finalize_dealer(enclave)?;
        self.logs.insert(log.dealer_bls.clone(), log.clone());
        gossip.broadcast(DkgWireMessage::FinalizedLog(log)).await
    }
}

/// Partials addressed to this enclave, including those received before recovery.
struct OfferPartials {
    recipient: Vec<u8>,
    sealed: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl OfferPartials {
    fn record(&mut self, message: DkgWireMessage) {
        if let DkgWireMessage::TributeOfferPartial {
            signer_bls,
            recipient_bls,
            partial,
        } = message
        {
            if recipient_bls == self.recipient {
                self.sealed.insert(signer_bls, partial);
            }
        }
    }

    async fn publish_local<C: EnclaveChannel, G: DkgGossip>(
        &mut self,
        coord: &CeremonyCoordinator,
        enclave: &mut C,
        gossip: &mut G,
    ) -> Result<()> {
        for (recipient_bls, partial) in coord.tribute_offer_partials_sealed(enclave)? {
            if recipient_bls == self.recipient {
                self.sealed.insert(self.recipient.clone(), partial);
            } else {
                gossip
                    .broadcast(DkgWireMessage::TributeOfferPartial {
                        signer_bls: self.recipient.clone(),
                        recipient_bls,
                        partial,
                    })
                    .await?;
            }
        }
        Ok(())
    }

    async fn complete<C: EnclaveChannel, G: DkgGossip>(
        mut self,
        coord: &CeremonyCoordinator,
        enclave: &mut C,
        gossip: &mut G,
        parameters: FoundingCeremonyParameters,
    ) -> Result<([u8; 32], Vec<u8>)> {
        self.publish_local(coord, enclave, gossip).await?;
        while self.sealed.len() < parameters.participant_count {
            let Some((_from, message)) = gossip.recv().await else {
                return Err(CeremonyError::UnexpectedResponse(
                    "gossip closed before founding offer-key finalization completed",
                ));
            };
            self.record(message);
        }
        let partials = self.sealed.into_values().collect::<Vec<_>>();
        coord.finalize_tribute_offer(
            enclave,
            &partials,
            parameters.chain_id,
            parameters.tribute_offer_epoch,
        )
    }
}
