//! Deterministic confidential ledger keys, slot ciphertexts, and write authorizations.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_tee::resident::{self, AccountKeysHandle, ResidentOfferMaterial, StateKeyHandle};
use outbe_tee::tee_decrypt::V2EnvelopeDomain;
use zeroize::Zeroizing;

use super::errors::TeeError;

/// Every ledger uses field zero for its primary balance.
pub const FIELD_BALANCE: u8 = 0;

/// Confidential ledger named by key-derivation and modify labels.
///
/// Declaration order is the frozen protocol order. It is not the witness
/// response tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Ledger {
    Gratis,
    Promis,
    Nod,
}

/// Immutable labels that separate one ledger from the other confidential ledgers.
pub struct Domain {
    ledger: Ledger,
    state_info: &'static [u8],
    /// View and modify subkey labels for this ledger.
    pub account_keys: AccountKeyDerivation,
    /// Modify-authorization domain for this ledger.
    pub authorization: ModifyDomain,
}

/// HKDF labels for an account view key and modify key.
pub struct AccountKeyDerivation {
    view_info: &'static [u8],
    modify_info: &'static [u8],
}

/// Nonce label for one slot family inside a ledger.
pub struct SlotCipherDomain {
    /// HKDF info that separates this slot family's nonce from other fields.
    pub nonce_info: &'static [u8],
}

/// Domain separator for a modify MAC.
pub struct ModifyDomain {
    modify_tag: &'static [u8],
}

/// The fields that a modify authorization authenticates, in canonical order.
pub struct ModifyAuthorization {
    /// Account bound by the MAC.
    pub account: Address,
    /// Operation discriminant bound by the MAC.
    pub op_tag: u8,
    /// Amount bound by the MAC.
    pub amount: U256,
    /// Account operation nonce bound by the MAC.
    pub op_nonce: u64,
    /// Chain id bound by the MAC.
    pub chain_id: B256,
}

/// A ledger operation whose discriminant a modify authorization binds.
pub trait OperationTag: Copy {
    /// Frozen one-byte discriminant of this operation.
    fn tag(self) -> u8;
}

/// The account operation covered by a modify authorization.
pub struct ModifyOperation<Op> {
    /// Account the operation changes.
    pub account: Address,
    /// Ledger operation.
    pub op: Op,
    /// Amount the operation applies.
    pub amount: U256,
    /// Account operation nonce.
    pub op_nonce: u64,
    /// Chain id the operation is bound to.
    pub chain_id: B256,
}

impl<Op: OperationTag> ModifyOperation<Op> {
    /// Canonical fields authenticated by [`ModifyDomain::modify_mac`].
    #[must_use]
    pub fn authorization(&self) -> ModifyAuthorization {
        ModifyAuthorization {
            account: self.account,
            op_tag: self.op.tag(),
            amount: self.amount,
            op_nonce: self.op_nonce,
            chain_id: self.chain_id,
        }
    }
}

/// A borrowed slot context. The key stays outside persistent enclave state.
pub struct SlotCipher<'a> {
    domain: &'a SlotCipherDomain,
    view_key: &'a StateKeyHandle,
    account: Address,
    field: u8,
}

/// Gratis ledger labels.
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

/// Promis ledger labels.
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

/// NOD ledger labels.
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

/// Rollback-safe Gratis balance envelope (`GRA2`).
pub const GRATIS_BALANCE_ENVELOPE: EnvelopeCipherSpec = EnvelopeCipherSpec {
    marker: b"GRA2",
    binding_tag: b"outbe/gratis/transition-binding/v2",
    key_info: b"outbe/gratis/amount-key/v2",
    nonce_info: b"outbe/gratis/amount-nonce/v2",
};

/// Rollback-safe Promis balance envelope (`PRO2`).
pub const PROMIS_BALANCE_ENVELOPE: EnvelopeCipherSpec = EnvelopeCipherSpec {
    marker: b"PRO2",
    binding_tag: b"outbe/promis/transition-binding/v2",
    key_info: b"outbe/promis/amount-key/v2",
    nonce_info: b"outbe/promis/amount-nonce/v2",
};

/// Ledger labels for `ledger`.
#[must_use]
pub fn domain_for(ledger: Ledger) -> &'static Domain {
    match ledger {
        Ledger::Gratis => &GRATIS,
        Ledger::Promis => &PROMIS,
        Ledger::Nod => &NOD,
    }
}

/// Domain-tagged message a caller personal-signs before `DeriveAccountKeys`.
#[must_use]
pub fn derive_account_keys_message(
    ledger: Ledger,
    account: Address,
    ephemeral_pubkey: B256,
) -> Vec<u8> {
    let tag: &[u8] = match ledger {
        Ledger::Gratis => b"outbe/gratis/derive-keys/v1",
        Ledger::Promis => b"outbe/promis/derive-keys/v1",
        Ledger::Nod => b"outbe/nod/derive-keys/v1",
    };
    let mut message = Vec::with_capacity(tag.len() + 20 + 32);
    message.extend_from_slice(tag);
    message.extend_from_slice(account.as_slice());
    message.extend_from_slice(ephemeral_pubkey.as_slice());
    message
}

/// EIP-191 hash of `message`. Byte-identical to `outbe_tee::protocol::eip191_hash`.
#[must_use]
pub fn eip191_hash(message: &[u8]) -> B256 {
    outbe_tee::protocol::eip191_hash(message)
}

/// Domain-separated `input_hash` for an event-only amount envelope.
#[must_use]
pub fn event_amount_input_hash(canonical_hash: B256) -> B256 {
    let mut preimage = b"outbe/tee/event-amount/v1".to_vec();
    preimage.extend_from_slice(canonical_hash.as_slice());
    keccak256(preimage)
}

impl Domain {
    /// HKDF label used to derive this ledger's state key.
    #[must_use]
    pub const fn state_info(&self) -> &'static [u8] {
        self.state_info
    }

    /// Ledger these labels belong to.
    #[must_use]
    pub const fn ledger(&self) -> Ledger {
        self.ledger
    }

    /// Derive the ledger state key from resident offer material.
    pub fn derive_state_key(
        &self,
        material: &ResidentOfferMaterial,
        chain_id: B256,
        epoch: u64,
    ) -> Result<StateKeyHandle, TeeError> {
        Ok(material.derive_state_key_handle(self.state_info, chain_id, epoch)?)
    }

    /// Seal account keys for the owner proof of this ledger, account, and recipient.
    pub fn export_sealed_account_keys(
        &self,
        keys: &AccountKeysHandle,
        owner_signature: &[u8; 65],
        requester_ephemeral_x25519: &[u8; 32],
    ) -> Result<resident::SealedAccountKeysExport, TeeError> {
        let message = derive_account_keys_message(
            self.ledger,
            keys.account(),
            B256::from(*requester_ephemeral_x25519),
        );
        Ok(resident::export_sealed_account_keys(
            keys,
            eip191_hash(&message),
            owner_signature,
            requester_ephemeral_x25519,
            b"outbe/tee/dkg-share/v1",
        )?)
    }
}

impl AccountKeyDerivation {
    /// View-key HKDF label.
    #[must_use]
    pub const fn view_info(&self) -> &'static [u8] {
        self.view_info
    }

    /// Modify-key HKDF label.
    #[must_use]
    pub const fn modify_info(&self) -> &'static [u8] {
        self.modify_info
    }

    /// Derive the account view key.
    pub fn derive_view_key(
        &self,
        state_key: &StateKeyHandle,
        account: Address,
    ) -> Result<StateKeyHandle, TeeError> {
        Ok(state_key.derive_subkey(account, self.view_info)?)
    }

    /// Derive the account view and modify keys.
    pub fn derive_account_keys(
        &self,
        state_key: &StateKeyHandle,
        account: Address,
    ) -> Result<AccountKeysHandle, TeeError> {
        Ok(resident::derive_account_keys_handle(
            state_key,
            account,
            self.view_info,
            self.modify_info,
        )?)
    }
}

impl SlotCipherDomain {
    /// Borrow a slot cipher for `account` and `field`.
    #[must_use]
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
    /// Open a versioned slot blob.
    pub fn read_blob(&self, blob: &[u8]) -> Result<(u64, Vec<u8>), TeeError> {
        let (version, plaintext) =
            self.view_key
                .slot_decrypt(self.domain.nonce_info, self.account, self.field, blob)?;
        Ok((version, plaintext.to_vec()))
    }

    /// Seal a versioned slot blob.
    pub fn write_blob(&self, prev_version: u64, plaintext: &[u8]) -> Result<Vec<u8>, TeeError> {
        Ok(self.view_key.slot_encrypt(
            self.domain.nonce_info,
            self.account,
            self.field,
            prev_version,
            plaintext,
        )?)
    }

    /// Open a 32-byte unsigned integer slot.
    pub fn read_u256(&self, version: Option<u64>, blob: &[u8]) -> Result<U256, TeeError> {
        if version.is_none() {
            return Err(TeeError::DecryptFailed(None));
        }
        let (_, bytes) = self.read_blob(blob)?;
        u256_from_plaintext(&bytes)
    }

    /// Seal a 32-byte unsigned integer slot.
    pub fn write_u256(&self, prev_version: u64, amount: U256) -> Result<Vec<u8>, TeeError> {
        let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
        self.write_blob(prev_version, plaintext.as_ref())
    }
}

fn u256_from_plaintext(bytes: &[u8]) -> Result<U256, TeeError> {
    let amount: &[u8; 32] = bytes
        .try_into()
        .map_err(|_| TeeError::DecryptFailed(None))?;
    Ok(U256::from_be_bytes(*amount))
}

impl ModifyDomain {
    fn modify_payload(&self, request: &ModifyAuthorization) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(request.account.as_slice());
        payload.push(request.op_tag);
        payload.extend_from_slice(&request.amount.to_be_bytes::<32>());
        payload.extend_from_slice(&request.op_nonce.to_be_bytes());
        payload.extend_from_slice(request.chain_id.as_slice());
        payload
    }

    /// MAC over the canonical modify payload.
    #[must_use]
    pub fn modify_mac(&self, keys: &AccountKeysHandle, request: &ModifyAuthorization) -> [u8; 32] {
        resident::compute_modify_mac(keys, self.modify_tag, &self.modify_payload(request))
            .expect("fixed protocol modify domain")
    }

    /// True when `mac` authenticates `request` for `keys`.
    #[must_use]
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

/// Domain constants for a rollback-safe V2 bound envelope.
pub struct EnvelopeCipherSpec {
    /// Four-byte envelope marker (`GRA2`, `PRO2`).
    pub marker: &'static [u8; 4],
    /// Binding tag mixed into the transition MAC.
    pub binding_tag: &'static [u8],
    /// HKDF info for the envelope key.
    pub key_info: &'static [u8],
    /// HKDF info for the envelope nonce.
    pub nonce_info: &'static [u8],
}

/// Inputs for a 92-byte V2 balance or event transition.
pub struct BalanceTransition {
    /// Account the balance belongs to.
    pub account: Address,
    /// Slot field. Primary balances use [`FIELD_BALANCE`].
    pub field: u8,
    /// Version already stored, or zero for a first write.
    pub previous_version: u64,
    /// Plaintext amount.
    pub amount: U256,
    /// Commitment mixed into the transition binding.
    pub input_hash: B256,
}

impl EnvelopeCipherSpec {
    /// Open a 32-byte amount. An empty blob is version 0 and amount 0.
    pub fn read_amount(
        &self,
        key: &StateKeyHandle,
        account: Address,
        field: u8,
        blob: &[u8],
    ) -> Result<(u64, U256), TeeError> {
        if blob.is_empty() {
            return Ok((0, U256::ZERO));
        }
        if blob.len() != BALANCE_BLOB_LEN {
            return Err(TeeError::DecryptFailed(None));
        }
        let (version, bytes) = self.read_envelope(key, account, &[field], blob)?;
        let bytes = Zeroizing::new(bytes);
        Ok((version, u256_from_plaintext(&bytes)?))
    }

    /// Seal a 32-byte amount. `previous_version == u64::MAX` fails closed.
    pub fn write_amount(
        &self,
        key: &StateKeyHandle,
        transition: BalanceTransition,
    ) -> Result<Vec<u8>, TeeError> {
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

    /// Open an arbitrary V2 envelope under `scope`.
    pub fn read_envelope(
        &self,
        key: &StateKeyHandle,
        account: Address,
        scope: &[u8],
        blob: &[u8],
    ) -> Result<(u64, Vec<u8>), TeeError> {
        let (version, plaintext) =
            key.decrypt_v2_envelope(account, self.envelope_domain(scope), blob)?;
        Ok((version, plaintext.to_vec()))
    }

    /// Seal an arbitrary V2 envelope. The next version must fit in `u64`.
    pub fn write_envelope(
        &self,
        key: &StateKeyHandle,
        (account, scope): (Address, &[u8]),
        transition: (u64, &[u8]),
        plaintext: &[u8],
    ) -> Result<Vec<u8>, TeeError> {
        transition
            .0
            .checked_add(1)
            .ok_or(TeeError::EncryptFailed(None))?;
        Ok(key.encrypt_v2_envelope(
            account,
            self.envelope_domain(scope),
            self.binding_tag,
            transition,
            plaintext,
        )?)
    }
}
