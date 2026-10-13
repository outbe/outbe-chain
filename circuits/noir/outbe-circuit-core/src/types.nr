//! Primitive protocol types.
//!
//! `EthAddress` mirrors Aztec's, which is `Field`-backed rather than `[u8; 20]`.
//! Upstream (the file this is a trimmed copy of):
//!   AztecProtocol/aztec-packages, tag v5.1.0,
//!   noir-projects/noir-protocol-circuits/crates/types/src/address/eth_address.nr
//! (on the `next` branch that path moved to noir-projects/fnd/noir-protocol-circuits/...)
//!
//! A 20-byte address is 160 bits and fits one BN254 field element, so carrying
//! it across the ABI as bytes costs 20 leaves -- and 20 public inputs -- to move
//! one value the circuit immediately folds back into a single `Field`.
//!
//! TODO: if we need more of these (AztecAddress, Point, Serialize/Deserialize/
//! Packable, to_be_bytes), take the real dependency instead of growing a
//! parallel copy:
//!   protocol_types = { git = "https://github.com/AztecProtocol/aztec-packages",
//!                      tag = "v5.1.0",
//!                      directory = "noir-projects/noir-protocol-circuits/crates/types" }
//! v5.1.0 is the tag whose pinned noir submodule is exactly 1.0.0-beta.22, our
//! `mise.toml` pin. It also needs a third git origin allowed in `deny.toml`.

/// Upstream calls this `MAX_ETH_ADDRESS_BIT_SIZE` (types/src/constants.nr).
pub global MAX_ETH_ADDRESS_BIT_SIZE: u32 = 160;

pub struct EthAddress {
    inner: Field,
}

impl EthAddress {
    /// Range-checked constructor -- identical to upstream's `from_field`. A
    /// `[u8; 20]` ABI param enforced 160 bits for free; keep that guarantee now
    /// that the ABI leaf is a bare `Field`.
    ///
    /// The fold is only injective below 2^160 and `note_sn` hashes this value,
    /// so the check is what stops two distinct field witnesses from aliasing
    /// onto one address. Upstream defers it to the tail circuit because
    /// `from_field` is on their hot path; here there is exactly one
    /// construction, so we check on construction.
    pub fn from_field(field: Field) -> Self {
        field.assert_max_bit_size::<MAX_ETH_ADDRESS_BIT_SIZE>();
        Self { inner: field }
    }

    /// Re-impose the 160-bit bound on a value that did NOT come through
    /// `from_field` -- notably one deserialized straight from the ABI, where no
    /// constructor runs. Upstream has this for the same reason. Forgetting it
    /// silently drops the bound, so every ABI-supplied address must call it.
    pub fn validate(self) {
        self.inner.assert_max_bit_size::<MAX_ETH_ADDRESS_BIT_SIZE>();
    }

    pub fn to_field(self) -> Field {
        self.inner
    }
}
