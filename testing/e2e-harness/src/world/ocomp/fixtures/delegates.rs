use crate::world::ocomp::*;

impl OcompTopology {
    /// Ask the node for only the inner OCOMP attestation, then build and sign
    /// the exact public transaction with this domain's dedicated OCOMP EVM key.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_held_vote_transaction(
        &self,
        validator_index: u8,
        mut vote: ResultVoteV1,
        nonce: u64,
        max_fee_per_gas: u128,
        gas_limit: u64,
    ) -> Result<PreparedVoteTransactionV1> {
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is unavailable"))?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let bundle = ProtocolBundleV1::decode_canonical(
            &fs::read(
                self.domain_root(validator_index)?
                    .join("protocol-bundle-v1.ocb1"),
            )?,
            &limits,
        )?;
        let key = fs::read_to_string(self.domain_root(validator_index)?.join("ocomp-key-v1.hex"))?;
        let signing_key = SigningKey::from_slice(&hex::decode(key.trim())?)?;
        let public_key = signing_key.verifying_key().to_encoded_point(true);
        vote.ocomp_key_hash = keccak256(public_key.as_bytes());
        vote.signature_rs = [0; 64];
        let subject = ResultVoteSigningSubjectV1 {
            chain_id: identity.chain_id,
            genesis_hash: identity.genesis_hash,
            fork_id: bundle.fork_id,
            protocol_bundle_hash: vote.protocol_bundle_hash,
            job_id: vote.job_id,
            attempt: vote.attempt,
            result_validator_set_epoch: vote.result_validator_set_epoch,
            result_committee_set_hash: vote.result_committee_set_hash,
            result_ocomp_binding_hash: vote.result_ocomp_binding_hash,
            ocomp_key_hash: vote.ocomp_key_hash,
            key_epoch: vote.key_epoch,
            purpose: SignOncePurpose::ResultSignature as u8,
            result_digest: vote.result_digest(&limits)?,
        };
        let signature: Signature =
            signing_key.sign_prehash(subject.signing_digest()?.as_slice())?;
        vote.signature_rs = signature
            .normalize_s()
            .unwrap_or(signature)
            .to_bytes()
            .into();
        let canonical_vote = vote.encode_canonical(&limits)?;
        let calldata =
            outbe_ocomp_protocol::abi::encode_submit_lysis_result_calldata(&vote, &limits)?;
        let signer = OutbeEvmSigner::from_file(
            self.domain_root(validator_index)?.join("ocomp-evm-key.hex"),
        )?;
        let signed = signer.sign_eip1559(TxEip1559 {
            chain_id: identity.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input: Bytes::from(calldata),
            access_list: Default::default(),
        })?;
        let transaction_hash = *signed.hash();
        let mut raw_transaction = Vec::with_capacity(signed.encode_2718_len());
        signed.encode_2718(&mut raw_transaction);
        Ok(PreparedVoteTransactionV1 {
            canonical_vote: BoundedBytes(canonical_vote),
            raw_transaction: BoundedBytes(raw_transaction),
            transaction_hash,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn install_ocomp_delegate_bindings(&self) -> Result<()> {
        const OCOMP_ROLE: u8 = 2;
        let validator_indices = self.validator_indices()?;
        for validator_index in validator_indices.iter().copied() {
            let index = usize::from(validator_index);
            let validator_key =
                crate::internal::proc::read_evm_key(&self.cfg.validator_dir(index))?;
            let delegate = self.ocomp_delegate_address(validator_index)?;
            let url = self.cfg.rpc_url(index);
            let tx_hash = crate::internal::eth::send_call(
                &url,
                VALIDATOR_SET_ADDRESS,
                &validator_key,
                &crate::internal::eth::IValidatorSet::setDelegateCall {
                    role: OCOMP_ROLE,
                    delegate,
                },
                None,
            )?;
            eyre::ensure!(
                crate::internal::eth::receipt_success(&url, &tx_hash) == Some(true),
                "validator-{validator_index} OCOMP delegation transaction failed"
            );
            if crate::internal::eth::balance(&url, delegate) == Some(U256::ZERO) {
                let funding_tx = crate::internal::eth::send_value(
                    &url,
                    delegate,
                    &validator_key,
                    crate::internal::eth::coen(1),
                )?;
                eyre::ensure!(
                    crate::internal::eth::receipt_success(&url, &funding_tx) == Some(true),
                    "validator-{validator_index} OCOMP delegate funding transaction failed"
                );
            }
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let complete = validator_indices.iter().copied().all(|validator_index| {
                let delegate = self
                    .ocomp_delegate_address(validator_index)
                    .unwrap_or(Address::ZERO);
                let expected_validator = crate::internal::proc::read_evm_key(
                    &self.cfg.validator_dir(usize::from(validator_index)),
                )
                .ok()
                .and_then(|key| crate::internal::eth::address_of(&key))
                .unwrap_or(Address::ZERO);
                (0..self.domains.len()).all(|rpc_index| {
                    let rpc_url = self.cfg.rpc_url(rpc_index);
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: OCOMP_ROLE,
                            signer: delegate,
                        },
                    ) == Some(expected_validator)
                        && crate::internal::eth::balance(&rpc_url, delegate)
                            .is_some_and(|balance| !balance.is_zero())
                })
            });
            if complete {
                return Ok(());
            }
            eyre::ensure!(
                Instant::now() < deadline,
                "OCOMP delegate bindings did not converge on every validator"
            );
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_delegate_address(&self, validator_index: u8) -> Result<Address> {
        self.domain(validator_index)?;
        crate::internal::eth::address_of(&ocomp_evm_private_key(validator_index))
            .ok_or_else(|| eyre::eyre!("invalid deterministic OCOMP EVM key"))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_delegate_private_key(&self, validator_index: u8) -> Result<String> {
        self.domain(validator_index)?;
        Ok(format!("0x{}", ocomp_evm_private_key(validator_index)))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_delegate_private_key_for_vote(&self, vote: &ResultVoteV1) -> Result<String> {
        for validator_index in self.validator_indices()? {
            let encoded =
                fs::read_to_string(self.domain_root(validator_index)?.join("ocomp-key-v1.hex"))?;
            let signing_key = SigningKey::from_slice(&hex::decode(encoded.trim())?)?;
            let public_key = signing_key.verifying_key().to_encoded_point(true);
            if keccak256(public_key.as_bytes()) == vote.ocomp_key_hash {
                return self.ocomp_delegate_private_key(validator_index);
            }
        }
        Err(eyre::eyre!(
            "ResultVoteV1 OCOMP key is not owned by this pinned test topology"
        ))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn verify_ocomp_delegate_bindings(&self) -> Result<()> {
        const ORACLE_ROLE: u8 = 1;
        const OCOMP_ROLE: u8 = 2;

        let mut observed_delegates = Vec::with_capacity(self.domains.len());
        for validator_index in self.validator_indices()? {
            let index = usize::from(validator_index);
            let validator_key =
                crate::internal::proc::read_evm_key(&self.cfg.validator_dir(index))?;
            let validator = crate::internal::eth::address_of(&validator_key)
                .ok_or_else(|| eyre::eyre!("invalid validator-{validator_index} EVM key"))?;
            let delegate = self.ocomp_delegate_address(validator_index)?;
            eyre::ensure!(
                delegate != validator,
                "validator-{validator_index} reused its validator EVM key for OCOMP"
            );
            eyre::ensure!(
                !observed_delegates.contains(&delegate),
                "two validator domains share the same OCOMP delegate"
            );
            observed_delegates.push(delegate);

            for rpc_index in 0..self.domains.len() {
                let rpc_url = self.cfg.rpc_url(rpc_index);
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::getDelegateCall {
                            validator,
                            role: OCOMP_ROLE,
                        },
                    ) == Some(delegate),
                    "validator-{validator_index} OCOMP delegate is inconsistent on RPC {rpc_index}"
                );
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: OCOMP_ROLE,
                            signer: delegate,
                        },
                    ) == Some(validator),
                    "validator-{validator_index} OCOMP delegate does not resolve on RPC {rpc_index}"
                );
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: ORACLE_ROLE,
                            signer: delegate,
                        },
                    ) == Some(Address::ZERO),
                    "validator-{validator_index} OCOMP delegate also has the ORACLE role on RPC {rpc_index}"
                );
            }
        }
        Ok(())
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn measurement_founder_registrations(
    validators_path: &Path,
    chain_id: u64,
    genesis_hash: B256,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<Vec<OcompKeyRegistrationV1>> {
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(validators_path)?)?;
    let validators = manifest
        .as_array()
        .ok_or_else(|| eyre::eyre!("validators manifest must be a JSON array"))?;
    let max_validators = usize::try_from(outbe_consensus::bls::MAX_VALIDATORS)?;
    eyre::ensure!(
        !validators.is_empty(),
        "validators manifest must not be empty"
    );
    eyre::ensure!(
        validators.len() <= max_validators,
        "validators manifest exceeds consensus bound {max_validators}"
    );

    validators
        .iter()
        .enumerate()
        .map(|(index, validator)| {
            let address = validator
                .get("address")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("validator-{index} has no address"))
                .and_then(|value| Address::from_str(value).map_err(Into::into))?;
            let consensus_key = validator
                .get("public_key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("validator-{index} has no public_key"))?;
            let consensus_key =
                hex::decode(consensus_key.strip_prefix("0x").unwrap_or(consensus_key))?;
            let consensus_key: [u8; 48] = consensus_key.try_into().map_err(|value: Vec<u8>| {
                eyre::eyre!(
                    "validator-{index} BLS MinPk must be 48 bytes, got {}",
                    value.len()
                )
            })?;
            let signing_key = measurement_signing_key(u8::try_from(index)?);
            let public_key: [u8; 33] = signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| eyre::eyre!("measurement OCOMP public key is not SEC1-33"))?;
            let mut registration = OcompKeyRegistrationV1 {
                core: OcompKeyRegistrationCoreV1 {
                    chain_id,
                    genesis_hash,
                    validator_identity_hash: validator_identity_hash_v1(address, &consensus_key)?,
                    ocomp_public_key_sec1: public_key,
                    key_epoch: POC_KEY_EPOCH,
                    allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
                },
                proof_of_possession: [0; 64],
            };
            let proof: Signature = signing_key
                .sign_prehash(registration.proof_of_possession_digest(limits)?.as_slice())
                .map_err(|_| eyre::eyre!("cannot sign validator-{index} OCOMP PoP"))?;
            registration.proof_of_possession =
                proof.normalize_s().unwrap_or(proof).to_bytes().into();
            registration.validate_proof_of_possession(limits)?;
            Ok(registration)
        })
        .collect()
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn measurement_signing_key(validator_index: u8) -> SigningKey {
    SigningKey::from_bytes((&[validator_index.saturating_add(1); 32]).into())
        .expect("indices 0..3 produce valid deterministic measurement scalars")
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn ocomp_evm_private_key(validator_index: u8) -> String {
    hex::encode([validator_index.saturating_add(0x71); 32])
}
