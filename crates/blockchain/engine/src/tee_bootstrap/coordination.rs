use super::*;

/// Authority, committee membership and local signer for OST3 coordination.
pub struct TeeBootstrapCoordination<'a> {
    pub authority: TeeBootstrapAuthorityV2,
    pub committee: &'a BTreeSet<Address>,
    pub evm_signer: &'a OutbeEvmSigner,
}

fn insert_submission(
    submissions: &mut BTreeMap<Address, TeeBootstrapParticipantSubmissionV2>,
    submission: TeeBootstrapParticipantSubmissionV2,
    policy: &TeePolicyV1,
    committee: &BTreeSet<Address>,
) -> eyre::Result<()> {
    let validator = validate_submission(&submission, policy, committee)?;
    if let Some(existing) = submissions.get(&validator) {
        if existing != &submission {
            return Err(eyre::eyre!(
                "validator {validator} equivocated between two valid OST3 submissions"
            ));
        }
        return Ok(());
    }
    submissions.insert(validator, submission);
    Ok(())
}

/// Coordinate complete canonical OST3 participant evidence and committee
/// signatures over one deterministic body. The enclosing startup timeout is
/// the liveness bound; malformed or unauthenticated gossip is ignored.
pub async fn coordinate_tee_bootstrap_v2<G: BootstrapGossip>(
    local_submission: TeeBootstrapParticipantSubmissionV2,
    coordination: TeeBootstrapCoordination<'_>,
    gossip: &mut G,
) -> eyre::Result<TeeBootstrapV2> {
    let TeeBootstrapCoordination {
        authority,
        committee,
        evm_signer,
    } = coordination;
    if committee.is_empty() || committee.len() > 256 {
        return Err(eyre::eyre!(
            "OST3 committee size is outside protocol bounds"
        ));
    }
    let local_validator = validate_submission(&local_submission, &authority.policy, committee)?;
    if local_validator != evm_signer.address() {
        return Err(eyre::eyre!("OST3 local submission and EVM signer differ"));
    }

    let CollectedSubmissions {
        submissions,
        early_signatures,
    } = collect_submissions(gossip, local_submission, &authority.policy, committee).await?;

    let mut payload =
        TeeBootstrapV2::assemble_unsigned(authority, submissions.into_values().collect())
            .map_err(|error| eyre::eyre!("OST3 unsigned assembly failed: {error}"))?;
    let signing_hash = payload
        .signing_hash()
        .map_err(|error| eyre::eyre!("OST3 signing hash failed: {error}"))?;
    let local_signature = evm_signer
        .sign_hash(&signing_hash)
        .map_err(|error| eyre::eyre!("OST3 local signature failed: {error}"))?;
    let signatures = SignatureCollection {
        signing_hash,
        committee,
        local_validator,
        local_signature,
    }
    .collect(gossip, early_signatures)
    .await?;
    for record in &mut payload.committee_signatures {
        record.signature = *signatures
            .get(&record.validator)
            .ok_or_else(|| eyre::eyre!("OST3 signature set is incomplete"))?;
    }
    payload
        .preflight()
        .map_err(|error| eyre::eyre!("signed OST3 payload failed preflight: {error}"))?;
    Ok(payload)
}

struct CollectedSubmissions {
    submissions: BTreeMap<Address, TeeBootstrapParticipantSubmissionV2>,
    early_signatures: Vec<Ost3WireMessage>,
}

async fn collect_submissions<G: BootstrapGossip>(
    gossip: &mut G,
    local_submission: TeeBootstrapParticipantSubmissionV2,
    policy: &TeePolicyV1,
    committee: &BTreeSet<Address>,
) -> eyre::Result<CollectedSubmissions> {
    gossip
        .broadcast(Ost3WireMessage::Submission(Box::new(local_submission.clone())).encode()?)
        .await
        .map_err(|error| eyre::eyre!("OST3 submission broadcast failed: {error}"))?;
    let mut submissions = BTreeMap::new();
    insert_submission(&mut submissions, local_submission, policy, committee)?;
    let mut early_signatures = Vec::new();
    while submissions.len() < committee.len() {
        let bytes = gossip
            .recv()
            .await
            .ok_or_else(|| eyre::eyre!("OST3 gossip closed before all submissions arrived"))?;
        match Ost3WireMessage::decode(&bytes) {
            Some(Ost3WireMessage::Submission(submission))
                if validate_submission(&submission, policy, committee).is_ok() =>
            {
                insert_submission(&mut submissions, *submission, policy, committee)?;
            }
            Some(signature @ Ost3WireMessage::Signature { .. }) => {
                early_signatures.push(signature);
            }
            _ => {}
        }
    }
    Ok(CollectedSubmissions {
        submissions,
        early_signatures,
    })
}

struct SignatureCollection<'a> {
    signing_hash: B256,
    committee: &'a BTreeSet<Address>,
    local_validator: Address,
    local_signature: [u8; 65],
}

impl SignatureCollection<'_> {
    fn accept(
        &self,
        message: Ost3WireMessage,
        signatures: &mut BTreeMap<Address, [u8; 65]>,
    ) -> eyre::Result<()> {
        let Ost3WireMessage::Signature {
            signing_hash: received_hash,
            validator,
            signature,
        } = message
        else {
            return Ok(());
        };
        if received_hash != self.signing_hash
            || !self.committee.contains(&validator)
            || recover_signer(&self.signing_hash, &signature).ok() != Some(validator)
        {
            return Ok(());
        }
        if let Some(existing) = signatures.get(&validator) {
            if existing != &signature {
                return Err(eyre::eyre!(
                    "validator {validator} equivocated between OST3 signatures"
                ));
            }
            return Ok(());
        }
        signatures.insert(validator, signature);
        Ok(())
    }

    async fn collect<G: BootstrapGossip>(
        &self,
        gossip: &mut G,
        early_signatures: Vec<Ost3WireMessage>,
    ) -> eyre::Result<BTreeMap<Address, [u8; 65]>> {
        gossip
            .broadcast(
                Ost3WireMessage::Signature {
                    signing_hash: self.signing_hash,
                    validator: self.local_validator,
                    signature: self.local_signature,
                }
                .encode()?,
            )
            .await
            .map_err(|error| eyre::eyre!("OST3 signature broadcast failed: {error}"))?;

        let mut signatures = BTreeMap::from([(self.local_validator, self.local_signature)]);
        for message in early_signatures {
            self.accept(message, &mut signatures)?;
        }
        while signatures.len() < self.committee.len() {
            let bytes = gossip
                .recv()
                .await
                .ok_or_else(|| eyre::eyre!("OST3 gossip closed before all signatures arrived"))?;
            if let Some(message) = Ost3WireMessage::decode(&bytes) {
                self.accept(message, &mut signatures)?;
            }
        }
        Ok(signatures)
    }
}
