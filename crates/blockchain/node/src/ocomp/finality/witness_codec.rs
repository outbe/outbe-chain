use super::*;

pub(super) fn encode_account_witness(
    proof: &AccountProof,
    expected_address: Address,
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    if proof.address != expected_address {
        return Err(FinalizedIntentProofBuildError::Trie(
            "account proof address mismatch".to_owned(),
        ));
    }
    let account = proof.info.as_ref().ok_or_else(|| {
        FinalizedIntentProofBuildError::Trie("account proof opened no account".to_owned())
    })?;
    let code_hash = account.get_bytecode_hash();
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&ACCOUNT_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(&account.nonce.to_be_bytes());
    encoded.extend_from_slice(&account.balance.to_be_bytes::<32>());
    encoded.extend_from_slice(proof.storage_root.as_slice());
    encoded.extend_from_slice(code_hash.as_slice());
    encode_nodes(&mut encoded, &proof.proof)?;
    Ok(encoded)
}

pub(super) fn encode_storage_witness(
    proof: &AccountProof,
    expected_slots: &[(U256, U256)],
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&STORAGE_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(expected_slots.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("storage proof count exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for (slot, value) in expected_slots {
        let key = B256::new(slot.to_be_bytes::<32>());
        let matches = proof
            .storage_proofs
            .iter()
            .filter(|candidate| candidate.key == key)
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].value != *value {
            return Err(FinalizedIntentProofBuildError::Trie(
                "account proof does not contain one exact requested storage value".to_owned(),
            ));
        }
        encode_nodes(&mut encoded, &matches[0].proof)?;
    }
    Ok(encoded)
}

fn encode_nodes(
    encoded: &mut Vec<u8>,
    nodes: &[Bytes],
) -> Result<(), FinalizedIntentProofBuildError> {
    encoded.extend_from_slice(
        &u32::try_from(nodes.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("proof node count exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for node in nodes {
        encoded.extend_from_slice(
            &u32::try_from(node.len())
                .map_err(|_| {
                    FinalizedIntentProofBuildError::Trie("proof node length exceeds u32".to_owned())
                })?
                .to_be_bytes(),
        );
        encoded.extend_from_slice(node);
    }
    Ok(())
}

pub(super) fn encode_historical_committee_witness(
    snapshot: &CommitteeSnapshot,
    proof: &AccountProof,
    expected_slots: &[(U256, U256)],
) -> Result<Vec<u8>, FinalizedIntentProofBuildError> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&HISTORICAL_COMMITTEE_PROOF_MAGIC);
    encoded.extend_from_slice(&WITNESS_VERSION.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.committee.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("committee length exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    for entry in &snapshot.committee {
        encoded.extend_from_slice(entry.address.as_slice());
        encoded.extend_from_slice(&entry.consensus_pubkey);
    }
    encoded.extend_from_slice(&snapshot.vrf_material_version.to_be_bytes());
    encoded.extend_from_slice(
        &u32::try_from(snapshot.vrf_group_public_key_bytes.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie("VRF group key length exceeds u32".to_owned())
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&snapshot.vrf_group_public_key_bytes);
    encoded.extend_from_slice(snapshot.vrf_public_polynomial_hash.as_slice());

    let account = encode_account_witness(proof, VALIDATOR_SET_ADDRESS)?;
    encoded.extend_from_slice(
        &u32::try_from(account.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie(
                    "committee account witness length exceeds u32".to_owned(),
                )
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&account);
    let storage = encode_storage_witness(proof, expected_slots)?;
    encoded.extend_from_slice(
        &u32::try_from(storage.len())
            .map_err(|_| {
                FinalizedIntentProofBuildError::Trie(
                    "committee storage witness length exceeds u32".to_owned(),
                )
            })?
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&storage);
    Ok(encoded)
}

pub(super) fn ensure_witness_cap(
    kind: &'static str,
    actual: usize,
    limit: usize,
) -> Result<(), FinalizedIntentVerifierError> {
    if actual > limit {
        return Err(FinalizedIntentVerifierError::WitnessTooLarge { kind });
    }
    Ok(())
}

pub(super) fn verify_account_witness(
    account: AccountWitness,
    address: Address,
    state_root: B256,
) -> Result<B256, FinalizedIntentVerifierError> {
    let account_info = Account {
        nonce: account.nonce,
        balance: account.balance,
        bytecode_hash: Some(account.code_hash),
    };
    AccountProof {
        address,
        info: Some(account_info),
        proof: account.nodes,
        storage_root: account.storage_root,
        storage_proofs: Vec::new(),
    }
    .verify(state_root)
    .map_err(|error| FinalizedIntentVerifierError::AccountProof(error.to_string()))?;

    Ok(account.storage_root)
}

pub(super) struct AccountWitness {
    pub(super) nonce: u64,
    pub(super) balance: U256,
    pub(super) storage_root: B256,
    pub(super) code_hash: B256,
    pub(super) nodes: Vec<Bytes>,
}

impl AccountWitness {
    pub(super) fn decode(
        encoded: &[u8],
        limits: &SchemaLimits,
    ) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "account");
        input.expect_magic(ACCOUNT_PROOF_MAGIC)?;
        input.expect_version()?;
        let nonce = input.read_u64()?;
        let balance = U256::from_be_bytes(input.read_array::<32>()?);
        let storage_root = B256::new(input.read_array::<32>()?);
        let code_hash = B256::new(input.read_array::<32>()?);
        let nodes = input.read_nodes(limits)?;
        input.finish()?;
        Ok(Self {
            nonce,
            balance,
            storage_root,
            code_hash,
            nodes,
        })
    }
}

pub(super) struct StorageWitness {
    pub(super) proofs: Vec<Vec<Bytes>>,
}

impl StorageWitness {
    pub(super) fn decode(
        encoded: &[u8],
        limits: &SchemaLimits,
        expected_proofs: usize,
    ) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "storage");
        input.expect_magic(STORAGE_PROOF_MAGIC)?;
        input.expect_version()?;
        let proof_count = input.read_count(limits.max_collection_items)?;
        if proof_count != expected_proofs {
            return Err(FinalizedIntentVerifierError::StorageProofCount {
                expected: expected_proofs,
                actual: proof_count,
            });
        }
        let mut proofs = Vec::with_capacity(proof_count);
        for _ in 0..proof_count {
            proofs.push(input.read_nodes(limits)?);
        }
        input.finish()?;
        Ok(Self { proofs })
    }
}

pub(super) struct HistoricalCommitteeWitness {
    pub(super) snapshot: CommitteeSnapshot,
    pub(super) account: AccountWitness,
    pub(super) storage: StorageWitness,
}

impl HistoricalCommitteeWitness {
    pub(super) fn decode(
        encoded: &[u8],
        limits: &SchemaLimits,
    ) -> Result<Self, FinalizedIntentVerifierError> {
        let mut input = WitnessReader::new(encoded, "historical committee");
        input.expect_magic(HISTORICAL_COMMITTEE_PROOF_MAGIC)?;
        input.expect_version()?;
        let committee_len = input.read_count(limits.max_collection_items)?;
        let mut committee = Vec::with_capacity(committee_len);
        for _ in 0..committee_len {
            committee.push(CommitteeEntry {
                address: Address::from(input.read_array::<20>()?),
                consensus_pubkey: input.read_array::<48>()?,
            });
        }
        let vrf_material_version = input.read_u64()?;
        let vrf_key_len = input.read_count(limits.max_proof_bytes)?;
        let vrf_group_public_key_bytes = input.take(vrf_key_len)?.to_vec();
        let vrf_public_polynomial_hash = B256::new(input.read_array::<32>()?);

        let account_len = input.read_count(limits.max_proof_bytes)?;
        let account = AccountWitness::decode(input.take(account_len)?, limits)?;
        let snapshot = CommitteeSnapshot {
            committee,
            vrf_material_version,
            vrf_group_public_key_bytes,
            vrf_public_polynomial_hash,
        };
        let expected_storage = historical_committee_storage_slots(0, B256::ZERO, &snapshot).len();
        let storage_len = input.read_count(limits.max_proof_bytes)?;
        let storage = StorageWitness::decode(input.take(storage_len)?, limits, expected_storage)?;
        input.finish()?;
        Ok(Self {
            snapshot,
            account,
            storage,
        })
    }
}

struct WitnessReader<'a> {
    encoded: &'a [u8],
    offset: usize,
    kind: &'static str,
}

impl<'a> WitnessReader<'a> {
    const fn new(encoded: &'a [u8], kind: &'static str) -> Self {
        Self {
            encoded,
            offset: 0,
            kind,
        }
    }

    fn expect_magic(&mut self, expected: [u8; 4]) -> Result<(), FinalizedIntentVerifierError> {
        if self.read_array::<4>()? != expected {
            return self.malformed("magic");
        }
        Ok(())
    }

    fn expect_version(&mut self) -> Result<(), FinalizedIntentVerifierError> {
        if self.read_u16()? != WITNESS_VERSION {
            return self.malformed("version");
        }
        Ok(())
    }

    fn read_nodes(
        &mut self,
        limits: &SchemaLimits,
    ) -> Result<Vec<Bytes>, FinalizedIntentVerifierError> {
        let count = self.read_count(limits.max_collection_items)?;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            let len = self.read_count(limits.max_proof_bytes)?;
            nodes.push(Bytes::copy_from_slice(self.take(len)?));
        }
        Ok(nodes)
    }

    fn read_count(&mut self, limit: usize) -> Result<usize, FinalizedIntentVerifierError> {
        let value =
            usize::try_from(self.read_u32()?).map_err(|_| self.error("count conversion"))?;
        if value > limit {
            return self.malformed("count cap");
        }
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, FinalizedIntentVerifierError> {
        Ok(u16::from_be_bytes(self.read_array::<2>()?))
    }

    fn read_u32(&mut self) -> Result<u32, FinalizedIntentVerifierError> {
        Ok(u32::from_be_bytes(self.read_array::<4>()?))
    }

    fn read_u64(&mut self) -> Result<u64, FinalizedIntentVerifierError> {
        Ok(u64::from_be_bytes(self.read_array::<8>()?))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], FinalizedIntentVerifierError> {
        self.take(N)?
            .try_into()
            .map_err(|_| self.error("truncated fixed field"))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], FinalizedIntentVerifierError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| self.error("offset overflow"))?;
        if end > self.encoded.len() {
            return self.malformed("truncated field");
        }
        let bytes = &self.encoded[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn finish(&self) -> Result<(), FinalizedIntentVerifierError> {
        if self.offset != self.encoded.len() {
            return self.malformed("trailing bytes");
        }
        Ok(())
    }

    fn malformed<T>(&self, reason: &'static str) -> Result<T, FinalizedIntentVerifierError> {
        Err(self.error(reason))
    }

    const fn error(&self, reason: &'static str) -> FinalizedIntentVerifierError {
        FinalizedIntentVerifierError::WitnessMalformed {
            kind: self.kind,
            reason,
        }
    }
}
