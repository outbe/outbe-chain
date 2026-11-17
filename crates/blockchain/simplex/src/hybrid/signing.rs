//! Synchronous local signing permission; certificate verification is never gated.
use super::*;
#[derive(Clone)]
pub(super) struct SigningGate(Arc<dyn Fn() -> bool + Send + Sync>);
impl std::fmt::Debug for SigningGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningGate(..)")
    }
}
impl<V: Variant> HybridScheme<V> {
    /// Narrow this instance's signing permission without changing its verification authority.
    pub fn with_signing_gate(mut self, gate: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.signing_gate = Some(SigningGate(Arc::new(gate)));
        self
    }
    fn signing_allowed(&self) -> bool {
        self.signing_gate.as_ref().is_none_or(|gate| (gate.0)())
    }
}

impl<V: Variant> certificate::Scheme for HybridScheme<V> {
    type Signature = HybridSignature<V>;

    fn me(&self) -> Option<Participant> {
        // WAL replay identifies historical local votes even while live signing is closed.
        match &self.role {
            Role::Signer { index, .. } => Some(*index),
            Role::Verifier { .. } => None,
        }
    }

    fn participants(&self) -> &Set<Self::PublicKey> {
        self.participants_ref()
    }

    fn sign<D: Digest>(&self, subject: Subject<'_, D>) -> Option<Attestation<Self>> {
        if !self.signing_allowed() {
            return None;
        }
        let (individual_key, index, vrf_materials, vrf_material_version, namespace) =
            match &self.role {
                Role::Signer {
                    fields,
                    individual_key,
                    index,
                } => (
                    individual_key,
                    *index,
                    &fields.vrf_materials,
                    fields.expected_vrf_material_version,
                    &fields.namespace,
                ),
                Role::Verifier { .. } => return None,
            };

        // BLS individual vote signature (MinPk)
        let vote_namespace = subject.namespace(namespace);
        let message = subject.message();
        let bls_individual_vote = individual_key.sign(vote_namespace, &message);

        // BLS threshold seed partial (MinSig)
        let seed_message = seed_message_from_subject(&subject);
        #[cfg(any(test, feature = "test-utils"))]
        let seed_message = if self.invalid_seed_partial_for_test() {
            b"outbe-test/invalid-seed-partial".to_vec()
        } else {
            seed_message.to_vec()
        };
        let bls_seed_partial =
            vrf_materials.sign_seed(vrf_material_version, &namespace.seed, &seed_message)?;

        // MinPk identity attestation binding the partial to this validator's
        // identity key over (round, version, partial). Makes the partial
        // non-repudiably attributable for slashing (see proof::seed_partial).
        let round = round_from_subject(&subject);
        let seed_partial_identity_sig = {
            let partial_bytes = bls_seed_partial.encode();
            let attest_message = crate::proof::seed_partial_attest_message(
                round.epoch().get(),
                round.view().get(),
                vrf_material_version,
                partial_bytes.as_ref(),
            );
            individual_key.sign(&crate::proof::seed_attest_namespace(), &attest_message)
        };

        let signature = HybridSignature {
            bls_individual_vote,
            vrf_material_version,
            bls_seed_partial,
            seed_partial_identity_sig,
        };

        Some(Attestation {
            signer: index,
            signature: signature.into(),
        })
    }

    fn verify_attestation<R, D>(
        &self,
        _rng: &mut R,
        subject: Subject<'_, D>,
        attestation: &Attestation<Self>,
        _strategy: &impl Strategy,
    ) -> bool
    where
        R: CryptoRng,
        D: Digest,
    {
        let participants = self.participants_ref();
        let namespace = self.namespace_ref();

        let Some(public_key) = participants.key(attestation.signer) else {
            return false;
        };
        let Some(signature) = attestation.signature.get() else {
            return false;
        };

        // Verify BLS individual vote (MinPk)
        let vote_namespace = subject.namespace(namespace);
        let message = subject.message();
        public_key.verify(vote_namespace, &message, &signature.bls_individual_vote)
    }

    fn verify_attestations<R, D, I>(
        &self,
        rng: &mut R,
        subject: Subject<'_, D>,
        attestations: I,
        strategy: &impl Strategy,
    ) -> Verification<Self>
    where
        R: CryptoRng,
        D: Digest,
        I: IntoIterator<Item = Attestation<Self>>,
        I::IntoIter: Send,
    {
        let mut verified = Vec::new();
        let mut invalid = Vec::new();

        for attestation in attestations {
            if !self.verify_attestation(rng, subject, &attestation, strategy) {
                invalid.push(attestation.signer);
                continue;
            }
            let verdict = self.classify_seed_partial(rng, subject, &attestation, strategy);
            if verdict == SeedPartialVerdict::Valid {
                verified.push(attestation);
            } else {
                self.record_seed_partial_drop(subject, &attestation, verdict);
            }
        }

        Verification::new(verified, invalid)
    }

    fn assemble<I>(
        &self,
        attestations: NonEmpty<I>,
        strategy: &impl Strategy,
    ) -> Result<Self::Certificate, certificate::AssemblyError>
    where
        I: Iterator<Item = Attestation<Self>> + Send,
    {
        let participants = self.participants_ref();

        // Collect and validate attestations
        let mut entries = Vec::new();
        for Attestation { signer, signature } in attestations {
            if usize::from(signer) >= participants.len() {
                return Err(certificate::AssemblyError::UnknownSigner(signer));
            }
            let sig = signature
                .get()
                .cloned()
                .ok_or(certificate::AssemblyError::MalformedSignature(signer))?;
            entries.push((signer, sig));
        }

        if entries.len() < participants.quorum::<N3f1>() as usize {
            return Err(certificate::AssemblyError::InsufficientAttestations(
                participants.quorum::<N3f1>(),
                entries.len() as u32,
            ));
        }

        // Sort by signer index
        entries.sort_by_key(|(signer, _)| *signer);
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(certificate::AssemblyError::DuplicateSigner(pair[0].0));
        }

        let signers_vec: Vec<Participant> = entries.iter().map(|(s, _)| *s).collect();

        // Aggregate individual BLS MinPk vote signatures
        let vote_sigs: Vec<&<MinPk as Variant>::Signature> = entries
            .iter()
            .map(|(_, sig)| sig.bls_individual_vote.as_ref())
            .collect();
        let bls_aggregated_vote = aggregate::combine_signatures::<MinPk, _>(
            NonEmpty::try_new(vote_sigs.into_iter()).expect("verified quorum is non-empty"),
        );

        // Every attestation reaching assembly is either locally produced by
        // this pinned scheme or was admitted by `verify_attestations` for the
        // exact subject. Keep the version check fail-closed so a future caller
        // cannot assemble a certificate from a heterogeneous set.
        let expected_vrf_version = self.expected_vrf_material_version();
        let seed_partials: Vec<PartialSignature<V>> = entries
            .iter()
            .filter_map(|(signer, sig)| {
                (sig.vrf_material_version == expected_vrf_version).then_some(PartialSignature {
                    index: *signer,
                    value: sig.bls_seed_partial,
                })
            })
            .collect();
        let quorum = participants.quorum::<N3f1>() as usize;
        if seed_partials.len() < quorum {
            crate::metrics::record_vrf_recover_failed_under_quorum();
            tracing::warn!(
                target: "outbe::hybrid",
                expected_vrf_version,
                quorum,
                seed_partials = seed_partials.len(),
                "refusing to assemble a certificate without a VRF quorum"
            );
            return Err(certificate::AssemblyError::RecoveryFailed);
        }
        let Some(vrf_proof) =
            self.vrf_materials()
                .recover_proof(expected_vrf_version, &seed_partials, strategy)
        else {
            crate::metrics::record_vrf_recover_failed_under_quorum();
            tracing::warn!(
                target: "outbe::hybrid",
                expected_vrf_version,
                quorum,
                seed_partials = seed_partials.len(),
                "refusing to assemble a certificate after VRF recovery failed"
            );
            return Err(certificate::AssemblyError::RecoveryFailed);
        };

        let signers = Signers::new(participants.len() as u32, signers_vec)?;

        Ok(HybridCertificate {
            signers,
            bls_aggregated_vote,
            vrf_proof,
        })
    }

    fn is_attributable() -> bool {
        true
    }
}
