use super::*;

pub(super) fn evidence_intent(evidence: &AttestationEvidenceV1) -> &RegistrationIntentV1 {
    match evidence {
        AttestationEvidenceV1::Dcap(value) => &value.intent,
        AttestationEvidenceV1::GramineDirectDev(value) => &value.intent,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BootstrapEvidenceKind {
    Dcap,
    GramineDirectDev,
}

pub(super) fn bootstrap_evidence_kind(
    policy: AttestationMode,
    production_session: bool,
) -> eyre::Result<BootstrapEvidenceKind> {
    match (policy, production_session) {
        (AttestationMode::DcapRequired, true) => Ok(BootstrapEvidenceKind::Dcap),
        (AttestationMode::DcapRequired, false) => Err(eyre::eyre!(
            "DcapRequired genesis policy cannot use a development enclave session"
        )),
        (AttestationMode::GramineDirectDev, _) => Ok(BootstrapEvidenceKind::GramineDirectDev),
    }
}

struct SubmissionValidation<'a> {
    submission: &'a TeeBootstrapParticipantSubmissionV2,
    intent: &'a RegistrationIntentV1,
    policy: &'a TeePolicyV1,
    committee: &'a BTreeSet<Address>,
    validator: Address,
    policy_hash: B256,
}

impl SubmissionValidation<'_> {
    fn committee_registration_matches(&self) -> bool {
        self.committee.contains(&self.validator)
            && self.submission.evidence.mode() == self.policy.attestation_mode
            && self.intent.operation == AttestationOperationV1::RegisterEnclave
            && self.intent.attestation_mode == self.policy.attestation_mode
    }

    fn policy_binding_matches(&self) -> bool {
        self.intent.policy_hash == self.policy_hash
            && self.intent.chain_id == self.policy.chain_id
            && self.intent.genesis_hash == self.policy.genesis_hash
    }

    fn initial_versions_match(&self) -> bool {
        self.intent.binding_version == 1
            && self.intent.registration_version == 0
            && self.intent.renewal_nonce == 0
            && self.intent.transition_nonce == 0
    }

    fn intent_signatures_match(&self) -> bool {
        self.intent
            .verify_node_signature(&self.submission.node_signature)
            && self
                .intent
                .verify_enclave_signature(&self.submission.enclave_signature)
    }

    fn node_identity_matches(&self) -> eyre::Result<bool> {
        let binding = &self.submission.validator_binding;
        Ok(binding.chain_id == self.policy.chain_id
            && binding.genesis_hash == self.policy.genesis_hash
            && binding.node_id_hash
                == self
                    .intent
                    .node_id
                    .node_id_hash()
                    .map_err(|error| eyre::eyre!("invalid OST3 NodeHost identity: {error}"))?)
    }

    fn binding_signatures_match(&self) -> bool {
        self.submission
            .validator_binding
            .verify_validator_signature(&self.submission.validator_signature)
            && self
                .submission
                .validator_binding
                .verify_node_signature(&self.submission.node_binding_signature)
    }

    fn canonical_registration_matches(&self) -> eyre::Result<bool> {
        let intent_matches = self.committee_registration_matches()
            && self.policy_binding_matches()
            && self.initial_versions_match();
        Ok(intent_matches
            && self.intent_signatures_match()
            && self.node_identity_matches()?
            && self.binding_signatures_match())
    }
}

pub(super) fn validate_submission(
    submission: &TeeBootstrapParticipantSubmissionV2,
    policy: &TeePolicyV1,
    committee: &BTreeSet<Address>,
) -> eyre::Result<Address> {
    let intent = evidence_intent(&submission.evidence);
    let validator = Address::from(submission.validator_binding.validator);
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("invalid OST3 policy: {error}"))?;
    let validation = SubmissionValidation {
        submission,
        intent,
        policy,
        committee,
        validator,
        policy_hash,
    };
    if !validation.canonical_registration_matches()? {
        return Err(eyre::eyre!(
            "OST3 submission does not prove one canonical committee registration"
        ));
    }
    if let AttestationEvidenceV1::GramineDirectDev(dev) = &submission.evidence {
        if dev.dev_attestation_public != intent.attestation_ed25519
            || dev.dev_signature != submission.enclave_signature
        {
            return Err(eyre::eyre!(
                "OST3 GramineDirectDev evidence does not match the outer enclave proof"
            ));
        }
    }
    Ok(validator)
}

#[cfg(test)]
mod validation_regression {
    use super::*;
    use outbe_primitives::tee_test_utils::sign_node_host_hash_for_test;
    fn policy() -> TeePolicyV1 {
        // Canonical golden bytes keep this fixture independent of the submission builder.
        let bytes = hex::decode(concat!(
            "010000000000000001101010101010101010101010101010101010101010101010101010101010101011111111111111",
            "111111111111111111111111111111111111111111111111110000000000000001000000000000000000000000000000",
            "000000000000000000000000000000000002727272727272727272727272727272727272727272727272727272727272",
            "72720003000000000002939a7233f79c4ca9940a0db3957f0607000503020000000102010000000000000e1000000000",
            "00093a800000000000000e107d7e959b073b078b70bfbca6bb532d099684958f711e048649d7a8fd75fb2eca00010181",
            "818181818181818181818181818181818181818181818181818181818181818282828282828282828282828282828282",
            "82828282828282828282828282828200010002000000000000000100000000000003e8"
        )).unwrap();
        TeePolicyV1::decode_canonical(&bytes).unwrap()
    }

    fn signed_intent() -> RegistrationIntentV1 {
        // Canonical golden bytes keep this fixture independent of the submission builder.
        let bytes = hex::decode(concat!(
            "011010101010101010101010101010101010101010101010101010101010101010111111111111111111111111111111",
            "11111111111111111111111111111111110102718f70dac4963b0c8f20c8432d6b020bc04b7145adf70eba36f9aeea4c",
            "ee943b0100000021036930f46dd0b16d866d59d1054aa63298b357499cd1862ef16f3f55f1cafceb8233333333333333",
            "333333333333333333333333333333333333333333333333333434343434343434343434343434343434343434343434",
            "34343434343434343400000000000000010000000000000000000000000000000000000000000000000000000000001c",
            "203535353535353535353535353535353535353535353535353535353535353535c6822637c7d310ec57627be00ba259",
            "d253749f4aaf644470cffbe53a35f7324237373737373737373737373737373737373737373737373737373737373737",
            "373838383838383838383838383838383838383838383838383838383838383838"
        )).unwrap();
        RegistrationIntentV1::decode_canonical(&bytes).unwrap()
    }

    fn node_signature(hash: B256) -> [u8; 65] {
        let key = k256::ecdsa::SigningKey::from_bytes((&[0x31; 32]).into()).unwrap();
        sign_node_host_hash_for_test(&key, hash)
    }
    fn fixture() -> (
        TeeBootstrapParticipantSubmissionV2,
        TeePolicyV1,
        BTreeSet<Address>,
    ) {
        let policy = policy();
        let intent = signed_intent();
        // Independent Ed25519 vector over the canonical intent (test seed 0x55).
        assert_eq!(
            hex::encode(intent.intent_hash().unwrap()),
            "79c09305fd18db907eb0d7e09222cb63014c67165aac8e8990331327e5457bb1"
        );
        let enclave_signature: [u8;64] = hex::decode("f3875b1ac94158c84f6a94b1ff3a6e688a37898372de53e9a25cd659d37059c12765bbecdde1abf1704ea86d76b34b3e466016d8516f815e0b5cd711e416dc05").unwrap().try_into().unwrap();
        let signer = OutbeEvmSigner::from_secret_bytes([0x41; 32]).unwrap();
        let binding = ValidatorNodeBindingV1 {
            chain_id: policy.chain_id,
            genesis_hash: policy.genesis_hash,
            validator: signer.address().into_array(),
            node_id_hash: intent.node_id.node_id_hash().unwrap(),
        };
        let submission = TeeBootstrapParticipantSubmissionV2 {
            node_signature: node_signature(intent.intent_hash().unwrap()),
            validator_signature: signer.sign_hash(&binding.binding_hash().unwrap()).unwrap(),
            node_binding_signature: node_signature(binding.binding_hash().unwrap()),
            validator_binding: binding,
            evidence: AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                dev_attestation_public: intent.attestation_ed25519,
                dev_signature: enclave_signature,
                intent,
                transition_key_ready_proof: None,
            }),
            enclave_signature,
        };
        (submission, policy, BTreeSet::from([signer.address()]))
    }

    fn intent_mut(
        submission: &mut TeeBootstrapParticipantSubmissionV2,
    ) -> &mut RegistrationIntentV1 {
        match &mut submission.evidence {
            AttestationEvidenceV1::Dcap(value) => &mut value.intent,
            AttestationEvidenceV1::GramineDirectDev(value) => &mut value.intent,
        }
    }

    type Mutation = fn(&mut TeeBootstrapParticipantSubmissionV2);

    fn registration_mutations() -> [Mutation; 18] {
        [
            |s| s.validator_binding.validator = [0; 20],
            |s| intent_mut(s).operation = AttestationOperationV1::RenewEnclave,
            |s| intent_mut(s).attestation_mode = AttestationMode::DcapRequired,
            |s| intent_mut(s).policy_hash = B256::ZERO,
            |s| intent_mut(s).chain_id = [0; 32],
            |s| intent_mut(s).genesis_hash = B256::ZERO,
            |s| intent_mut(s).binding_version = 2,
            |s| intent_mut(s).registration_version = 1,
            |s| intent_mut(s).renewal_nonce = 1,
            |s| intent_mut(s).transition_nonce = 1,
            |s| intent_mut(s).node_id.reth_p2p_public = [0; 33],
            |s| s.node_signature[0] ^= 1,
            |s| s.enclave_signature[0] ^= 1,
            |s| s.validator_binding.chain_id = [0; 32],
            |s| s.validator_binding.genesis_hash = B256::ZERO,
            |s| s.validator_binding.node_id_hash = B256::ZERO,
            |s| s.validator_signature[0] ^= 1,
            |s| s.node_binding_signature[0] ^= 1,
        ]
    }

    #[test]
    fn canonical_registration_requires_every_binding_and_signature() {
        let (base, policy, committee) = fixture();
        assert_eq!(
            validate_submission(&base, &policy, &committee).unwrap(),
            Address::from(base.validator_binding.validator)
        );
        for mutate in registration_mutations() {
            let mut submission = base.clone();
            mutate(&mut submission);
            assert_eq!(
                validate_submission(&submission, &policy, &committee)
                    .unwrap_err()
                    .to_string(),
                "OST3 submission does not prove one canonical committee registration"
            );
        }
    }

    #[test]
    fn outer_dev_proof_must_match_the_signed_intent() {
        let (base, policy, committee) = fixture();
        for alter_public in [true, false] {
            let mut submission = base.clone();
            let AttestationEvidenceV1::GramineDirectDev(dev) = &mut submission.evidence else {
                unreachable!()
            };
            if alter_public {
                dev.dev_attestation_public = [0; 32];
            } else {
                dev.dev_signature[0] ^= 1;
            }
            assert_eq!(
                validate_submission(&submission, &policy, &committee)
                    .unwrap_err()
                    .to_string(),
                "OST3 GramineDirectDev evidence does not match the outer enclave proof"
            );
        }
    }

    #[test]
    fn invalid_policy_precedes_committee_and_identity_errors() {
        let (mut submission, mut policy, _) = fixture();
        policy.policy_version = 0;
        intent_mut(&mut submission).node_id.reth_p2p_public = [0; 33];
        let error = validate_submission(&submission, &policy, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().starts_with("invalid OST3 policy:"));
    }

    #[test]
    fn wire_codec_rejects_truncation_trailing_bytes_and_oversized_evidence() {
        let (submission, _, _) = fixture();
        let message = Ost3WireMessage::Submission(Box::new(submission));
        let encoded = message.encode().unwrap();
        assert_eq!(Ost3WireMessage::decode(&encoded).unwrap(), message);
        for size in 0..encoded.len() {
            assert!(Ost3WireMessage::decode(&encoded[..size]).is_none());
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(Ost3WireMessage::decode(&trailing).is_none());
        let mut oversized = encoded;
        oversized[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Ost3WireMessage::decode(&oversized).is_none());
    }
}
