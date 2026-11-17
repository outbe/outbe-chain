//! Deterministic confidential ledger keys, slot ciphertexts, and write authorizations.
use crate::errors::{Result, TeeError};
use crate::protocol::{GratisOp, Ledger, PromisOp};
use alloy_primitives::{Address, B256, U256};
use outbe_tee::resident::{self, AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::tee_decrypt::V2EnvelopeDomain;
use zeroize::Zeroizing;

/// Every ledger uses field zero for its primary balance.
pub const FIELD_BALANCE: u8 = 0;

/// Immutable labels separate each ledger from the other confidential ledgers.
pub struct Domain {
    ledger: Ledger,
    state_info: &'static [u8],
    pub account_keys: AccountKeyDerivation,
    pub authorization: ModifyDomain,
}

pub struct AccountKeyDerivation {
    view_info: &'static [u8],
    modify_info: &'static [u8],
}
pub struct SlotCipherDomain {
    pub nonce_info: &'static [u8],
}
pub struct ModifyDomain {
    modify_tag: &'static [u8],
}

/// The fields that a modify authorization authenticates, in canonical order.
pub struct ModifyAuthorization {
    pub account: Address,
    pub op_tag: u8,
    pub amount: U256,
    pub op_nonce: u64,
    pub chain_id: B256,
}

/// A ledger operation whose discriminant a modify authorization binds.
pub trait OperationTag: Copy {
    fn tag(self) -> u8;
}

impl OperationTag for GratisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

impl OperationTag for PromisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

/// The account operation covered by a modify authorization.
pub struct ModifyOperation<Op> {
    pub account: Address,
    pub op: Op,
    pub amount: U256,
    pub op_nonce: u64,
    pub chain_id: B256,
}

impl<Op: OperationTag> ModifyOperation<Op> {
    pub(crate) fn authorization(&self) -> ModifyAuthorization {
        ModifyAuthorization {
            account: self.account,
            op_tag: self.op.tag(),
            amount: self.amount,
            op_nonce: self.op_nonce,
            chain_id: self.chain_id,
        }
    }
}

/// A borrowed slot context keeps the key outside persistent enclave state.
pub struct SlotCipher<'a> {
    domain: &'a SlotCipherDomain,
    view_key: &'a StateKeyHandle,
    account: Address,
    field: u8,
}

pub const GRATIS: Domain = Domain {
    ledger: Ledger::Gratis,
    state_info: b"outbe/gratis/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/gratis/view-key/v1",
        modify_info: b"outbe/gratis/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/gratis/modify/v1",
    },
};

pub const PROMIS: Domain = Domain {
    ledger: Ledger::Promis,
    state_info: b"outbe/promis/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/promis/view-key/v1",
        modify_info: b"outbe/promis/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/promis/modify/v1",
    },
};

pub const FIDELITY: Domain = Domain {
    ledger: Ledger::Fidelity,
    state_info: b"outbe/fidelity/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/fidelity/view-key/v1",
        modify_info: b"outbe/fidelity/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/fidelity/modify/v1",
    },
};

pub const TRIBUTE: Domain = Domain {
    ledger: Ledger::Tribute,
    state_info: b"outbe/tribute/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/tribute/view-key/v1",
        modify_info: b"outbe/tribute/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/tribute/modify/v1",
    },
};

pub const NOD: Domain = Domain {
    ledger: Ledger::Nod,
    state_info: b"outbe/nod/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/nod/view-key/v1",
        modify_info: b"outbe/nod/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/nod/modify/v1",
    },
};

pub fn domain_for(ledger: Ledger) -> &'static Domain {
    match ledger {
        Ledger::Gratis => &GRATIS,
        Ledger::Promis => &PROMIS,
        Ledger::Fidelity => &FIDELITY,
        Ledger::Tribute => &TRIBUTE,
        Ledger::Nod => &NOD,
    }
}

impl Domain {
    pub fn derive_state_key(
        &self,
        material: &ResidentOfferMaterial,
        chain_id: B256,
        epoch: u64,
    ) -> Result<StateKeyHandle> {
        Ok(material.derive_state_key_handle(self.state_info, chain_id, epoch)?)
    }

    /// Derive the owner proof internally; callers cannot substitute an unrelated hash.
    pub fn export_sealed_account_keys(
        &self,
        keys: &AccountKeysHandle,
        owner_signature: &[u8; 65],
        requester_ephemeral_x25519: &[u8; 32],
    ) -> Result<resident::SealedAccountKeysExport> {
        let message = crate::protocol::derive_account_keys_message(
            self.ledger,
            keys.account(),
            B256::from(*requester_ephemeral_x25519),
        );
        Ok(resident::export_sealed_account_keys(
            keys,
            crate::protocol::eip191_hash(&message),
            owner_signature,
            requester_ephemeral_x25519,
            b"outbe/tee/dkg-share/v1",
        )?)
    }
}

impl AccountKeyDerivation {
    pub fn derive_view_key(
        &self,
        state_key: &StateKeyHandle,
        account: Address,
    ) -> Result<StateKeyHandle> {
        Ok(state_key.derive_subkey(account, self.view_info)?)
    }

    pub fn derive_account_keys(
        &self,
        state_key: &StateKeyHandle,
        account: Address,
    ) -> Result<AccountKeysHandle> {
        Ok(resident::derive_account_keys_handle(
            state_key,
            account,
            self.view_info,
            self.modify_info,
        )?)
    }
}

impl SlotCipherDomain {
    pub fn slot<'a>(
        &'a self,
        view_key: &'a StateKeyHandle,
        account: Address,
        field: u8,
    ) -> SlotCipher<'a> {
        SlotCipher {
            domain: self,
            view_key,
            account,
            field,
        }
    }
}

impl SlotCipher<'_> {
    pub fn read_blob(&self, blob: &[u8]) -> Result<(u64, Vec<u8>)> {
        let (version, plaintext) =
            self.view_key
                .slot_decrypt(self.domain.nonce_info, self.account, self.field, blob)?;
        Ok((version, plaintext.to_vec()))
    }

    pub fn write_blob(&self, prev_version: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
        Ok(self.view_key.slot_encrypt(
            self.domain.nonce_info,
            self.account,
            self.field,
            prev_version,
            plaintext,
        )?)
    }

    pub fn read_u256(&self, version: Option<u64>, blob: &[u8]) -> Result<U256> {
        if version.is_none() {
            return Err(TeeError::DecryptFailed);
        }
        let (_, bytes) = self.read_blob(blob)?;
        let bytes = Zeroizing::new(bytes);
        let amount: &[u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| TeeError::DecryptFailed)?;
        Ok(U256::from_be_bytes(*amount))
    }

    pub fn write_u256(&self, prev_version: u64, amount: U256) -> Result<Vec<u8>> {
        let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
        self.write_blob(prev_version, plaintext.as_ref())
    }
}

impl ModifyDomain {
    fn modify_payload(&self, request: &ModifyAuthorization) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(request.account.as_slice());
        b.push(request.op_tag);
        b.extend_from_slice(&request.amount.to_be_bytes::<32>());
        b.extend_from_slice(&request.op_nonce.to_be_bytes());
        b.extend_from_slice(request.chain_id.as_slice());
        b
    }

    pub fn modify_mac(&self, keys: &AccountKeysHandle, request: &ModifyAuthorization) -> [u8; 32] {
        resident::compute_modify_mac(keys, self.modify_tag, &self.modify_payload(request))
            .expect("fixed protocol modify domain")
    }

    pub fn verify_modify_auth(
        &self,
        keys: &AccountKeysHandle,
        request: &ModifyAuthorization,
        mac: &[u8; 32],
    ) -> bool {
        keys.account() == request.account
            && resident::verify_modify_mac(
                keys,
                self.modify_tag,
                &self.modify_payload(request),
                mac,
            )
    }
}

const ENVELOPE_HEADER_LEN: usize = 8 + 4 + 32;
const ENVELOPE_TAG_LEN: usize = 16;
const BALANCE_BLOB_LEN: usize = ENVELOPE_HEADER_LEN + 32 + ENVELOPE_TAG_LEN;

/// Domain constants for a rollback-safe V2 bound envelope (`GRA2`, `PRO2`, `FID2`).
pub struct EnvelopeCipherSpec {
    pub marker: &'static [u8; 4],
    pub binding_tag: &'static [u8],
    pub key_info: &'static [u8],
    pub nonce_info: &'static [u8],
}

/// Inputs for a 92-byte V2 balance/event transition (`GRA2` / `PRO2`).
pub struct BalanceTransition {
    pub account: Address,
    pub field: u8,
    pub previous_version: u64,
    pub amount: U256,
    pub input_hash: B256,
}

/// Domain-separated `input_hash` for sealing event-only `GRA2`/`PRO2` amount
/// envelopes (`previous_version: 0`), ensuring the event envelope never shares
/// a binding, key, or nonce with the version-1 balance transition on first mint.
pub fn event_amount_input_hash(canonical_hash: B256) -> B256 {
    let mut preimage = b"outbe/tee/event-amount/v1".to_vec();
    preimage.extend_from_slice(canonical_hash.as_slice());
    alloy_primitives::keccak256(preimage)
}

impl EnvelopeCipherSpec {
    pub(crate) fn read_amount(
        &self,
        key: &StateKeyHandle,
        account: Address,
        field: u8,
        blob: &[u8],
    ) -> Result<(u64, U256)> {
        if blob.is_empty() {
            return Ok((0, U256::ZERO));
        }
        if blob.len() != BALANCE_BLOB_LEN {
            return Err(TeeError::DecryptFailed);
        }
        let (version, bytes) = self.read_envelope(key, account, &[field], blob)?;
        let bytes = Zeroizing::new(bytes);
        let amount: &[u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| TeeError::DecryptFailed)?;
        Ok((version, U256::from_be_bytes(*amount)))
    }

    pub(crate) fn write_amount(
        &self,
        key: &StateKeyHandle,
        transition: BalanceTransition,
    ) -> Result<Vec<u8>> {
        let BalanceTransition {
            account,
            field,
            previous_version,
            amount,
            input_hash,
        } = transition;
        let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
        let mut binding_payload = Zeroizing::new([0u8; 64]);
        binding_payload[..32].copy_from_slice(input_hash.as_slice());
        binding_payload[32..].copy_from_slice(plaintext.as_ref());
        self.write_envelope(
            key,
            (account, &[field]),
            (previous_version, binding_payload.as_ref()),
            plaintext.as_ref(),
        )
    }

    fn envelope_domain<'a>(&'a self, scope: &'a [u8]) -> V2EnvelopeDomain<'a> {
        V2EnvelopeDomain {
            marker: self.marker,
            scope,
            key_info: self.key_info,
            nonce_info: self.nonce_info,
        }
    }

    pub(crate) fn read_envelope(
        &self,
        key: &StateKeyHandle,
        account: Address,
        scope: &[u8],
        blob: &[u8],
    ) -> Result<(u64, Vec<u8>)> {
        let (version, plaintext) =
            key.decrypt_v2_envelope(account, self.envelope_domain(scope), blob)?;
        Ok((version, plaintext.to_vec()))
    }

    pub(crate) fn write_envelope(
        &self,
        key: &StateKeyHandle,
        (account, scope): (Address, &[u8]),
        transition: (u64, &[u8]),
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        transition.0.checked_add(1).ok_or(TeeError::EncryptFailed)?;
        Ok(key.encrypt_v2_envelope(
            account,
            self.envelope_domain(scope),
            self.binding_tag,
            transition,
            plaintext,
        )?)
    }
}
