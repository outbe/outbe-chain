//! Codec: how typed values cross between the protocol's field/curve world and
//! bytes.
//!
//! Two layers live here:
//!
//! 1. **Field encoding** ([`FieldElement`] / [`FieldEncode`]) — how a typed
//!    entity member folds into native field elements. The protocol hashes over
//!    [`Fr`], but an entity is a struct with named, typed members (an
//!    address, a `uint256`, a stored field id, …):
//!    - [`FieldElement`] — a value occupying **exactly one** field element (an
//!      address, a `bytes32` that is itself a field value, a small integer).
//!      Usable as an entity's id seed.
//!    - [`FieldEncode`] — a value expanding into **zero or more** field elements
//!      in canonical order (a `uint256` → `[120, 120, 16]`-bit limbs, a `T[]` → its
//!      elements). Everything in an entity body is `FieldEncode`.
//!
//!    Encoding is **fallible**: there is no safe total map from arbitrary
//!    256-bit bytes onto a ~254-bit field. A `bytes32` whose value is `>=` the
//!    modulus has no canonical element, and silently reducing it (`mod_order`)
//!    would alias distinct byte strings onto the same element — breaking the
//!    on-chain ↔ in-circuit binding. So a checked decode returns
//!    [`Error::NonCanonical`]. Use [`field_from_be_bytes_canonical`] for a value
//!    that *is* a field element; the plain [`field_from_be_bytes`] reducer is
//!    only for inputs already smaller than the modulus (e.g. a 20-byte address).
//!
//! 2. **Byte (de)serialization** — field elements and Grumpkin keys cross
//!    byte boundaries through concrete free functions. Compressed key outputs
//!    are 32-byte arrays; field words are big-endian.

#[cfg(feature = "alloy")]
use alloy_primitives::{B256, U256};
use ark_ec::CurveGroup;
use ark_ff::{BigInteger, PrimeField};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};

use crate::primitive::curve::{mul_generator, Affine, Scalar};
use crate::{Error, Fr};

// ============================ field encoding ============================

/// A value that occupies exactly one field element.
///
/// This is the stricter half of [`FieldEncode`]: a `FieldElement` is a
/// single element, so it can seed an entity id (`Entity::id_seed`) where
/// a multi-element value cannot.
pub trait FieldElement {
    /// The single field element this value encodes to, or
    /// [`Error::NonCanonical`] if it has no canonical element.
    fn to_field(&self) -> Result<Fr, Error>;

    /// Recover this type from a field's canonical value, rejecting values
    /// outside the target's range with [`Error::NonCanonical`].
    fn from_field(value: &Fr) -> Result<Self, Error>
    where
        Self: Sized;
}

/// A value that encodes into zero or more field elements, in canonical
/// order, appended to `out`.
pub trait FieldEncode {
    /// Append this value's field elements to `out`, or fail with
    /// [`Error::NonCanonical`] if some member has no canonical element.
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error>;
}

/// Decode big-endian bytes as one field element, **reducing mod the field
/// order**. Only safe for inputs guaranteed smaller than the modulus
/// (e.g. a ≤20-byte address); for a full-width value that must be a field
/// element use [`field_from_be_bytes_canonical`].
pub fn field_from_be_bytes(bytes: &[u8]) -> Fr {
    Fr::from_be_bytes_mod_order(bytes)
}

/// Decode big-endian bytes as one field element, **rejecting** a value
/// that is not already canonical (`>=` the field modulus). This is the
/// safe decode for a `bytes32` that carries a field element (an id, a
/// `derivedOwner`): a non-canonical input would otherwise be silently
/// reduced onto a different element.
pub fn field_from_be_bytes_canonical(bytes: &[u8], what: &'static str) -> Result<Fr, Error> {
    // Parse the big-endian bytes into the field's big-integer repr, then let
    // `from_bigint` do the canonical check: it compares `repr < modulus` and
    // returns `None` for anything `>= p` (no silent reduction, no byte
    // comparison).
    let bits: Vec<bool> = bytes
        .iter()
        .flat_map(|byte| (0..8).rev().map(move |i| (byte >> i) & 1 == 1))
        .collect();
    // The block rejects nonzero overflow bits while allowing extra leading zeros
    let excess = bits
        .len()
        .saturating_sub(<Fr as PrimeField>::BigInt::NUM_LIMBS * 64);
    if bits[..excess].iter().any(|bit| *bit) {
        return Err(Error::NonCanonical(what));
    }
    let repr = <<Fr as PrimeField>::BigInt as BigInteger>::from_bits_be(&bits);
    Fr::from_bigint(repr).ok_or(Error::NonCanonical(what))
}

fn field_to_be_array<const N: usize>(value: &Fr, what: &'static str) -> Result<[u8; N], Error> {
    let value = value.into_bigint();
    if value.num_bits() as usize > N * 8 {
        return Err(Error::NonCanonical(what));
    }
    let bytes = value.to_bytes_be();
    let len = bytes.len().min(N);
    let mut out = [0u8; N];
    out[N - len..].copy_from_slice(&bytes[bytes.len() - len..]);
    Ok(out)
}

/// Split a big-endian `uint256` into the canonical noir-bignum representation:
/// three little-endian limbs with radix `2^120`.
pub fn u256_limbs_be(be32: &[u8; 32]) -> [u128; 3] {
    let hi = u128::from_be_bytes(be32[..16].try_into().expect("16-byte slice"));
    let lo = u128::from_be_bytes(be32[16..].try_into().expect("16-byte slice"));
    let mask = (1u128 << 120) - 1;
    let mid = (lo >> 120) | ((hi & ((1u128 << 112) - 1)) << 8);
    [lo & mask, mid, hi >> 112]
}

/// Recombine canonical noir-bignum limbs into a big-endian `uint256`.
pub fn u256_from_limbs_be(limbs: [u128; 3]) -> Option<[u8; 32]> {
    let [l0, l1, l2] = limbs;
    if l0 >= 1u128 << 120 || l1 >= 1u128 << 120 || l2 >= 1u128 << 16 {
        return None;
    }
    let lo = l0 | ((l1 & 0xff) << 120);
    let hi = (l2 << 112) | (l1 >> 8);
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&hi.to_be_bytes());
    bytes[16..].copy_from_slice(&lo.to_be_bytes());
    Some(bytes)
}

// ---- primitive integers / bool: one element each ----

macro_rules! scalar_field_element {
    ($($t:ty),*) => {$(
        impl FieldElement for $t {
            fn to_field(&self) -> Result<Fr, Error> { Ok(Fr::from(*self)) }
            fn from_field(value: &Fr) -> Result<Self, Error> {
                Ok(Self::from_be_bytes(field_to_be_array(value, stringify!($t))?))
            }
        }
        impl FieldEncode for $t {
            fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
                out.push(Fr::from(*self));
                Ok(())
            }
        }
    )*};
}
scalar_field_element!(u8, u16, u32, u64, u128);

impl FieldElement for bool {
    fn to_field(&self) -> Result<Fr, Error> {
        Ok(Fr::from(*self))
    }

    fn from_field(value: &Fr) -> Result<Self, Error> {
        match field_to_be_array::<1>(value, "bool")?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::NonCanonical("bool")),
        }
    }
}

impl FieldEncode for bool {
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        out.push(self.to_field()?);
        Ok(())
    }
}

/// An entity vector field viewed as a canonical **set**.
///
/// A bare `[T]`/`Vec<T>` [`FieldEncode`] just concatenates its elements (see
/// below), which is ambiguous for an entity hash: two adjacent vectors
/// `sr = [a]`, `ar = [b, c]` fold to the *same* preimage as `sr = [a, b]`,
/// `ar = [c]` (a boundary collision), and the same multiset in a different
/// order folds to a different hash.
///
/// Wrapping a slice in `SortedSet` gives it a `FieldEncode` impl that removes
/// both ambiguities — so it composes into the same trait as every other field
/// instead of needing a bespoke call. `#[derive(Entity)]` wraps a `Vec<T>`
/// body field in this automatically; the underlying `[T]` impl is left as the
/// plain concatenation for any non-entity use. It is a zero-cost view (a
/// single `&[T]`).
pub struct SortedSet<'a, T>(pub &'a [T]);

impl<T: FieldElement> FieldEncode for SortedSet<'_, T> {
    /// Emit `[ len, e₀, e₁, … ]` with the elements strictly ascending by field
    /// value. The length prefix makes the boundary between two adjacent vectors
    /// unambiguous; the strict order makes the encoding canonical
    /// (origin-order–independent) *and* duplicate-free in one check.
    ///
    /// It does **not** sort for the caller: the input must already be sorted
    /// and de-duplicated, and a violation is rejected with
    /// [`Error::UnsortedSet`]. Requiring sorted input (rather than sorting
    /// here) keeps this in lock-step with the in-circuit fold, which asserts
    /// the same `eᵢ < eᵢ₊₁`, and forces every producer (chain, mobile, tests)
    /// to commit the same canonical order on-chain.
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        out.push(Fr::from(self.0.len() as u64));
        // Field elements are stored in Montgomery form, whose limb order is not
        // the value order, so the strict-ascending check must compare canonical
        // big-integer reprs (this is exactly what `ark`'s own `Ord for Fp` does
        // internally). Convert each element to its `BigInt` once and keep it as
        // `prev`, avoiding a second conversion per comparison.
        let mut prev: Option<<Fr as PrimeField>::BigInt> = None;
        for item in self.0 {
            let f = item.to_field()?;
            let cur = f.into_bigint();
            if let Some(p) = prev {
                // Reject anything not *strictly* greater — catches both
                // unsorted input and duplicate elements in one check.
                if cur <= p {
                    return Err(Error::UnsortedSet(
                        "entity vector field must be strictly ascending by field value (sorted + de-duplicated)".to_string(),
                    ));
                }
            }
            out.push(f);
            prev = Some(cur);
        }
        Ok(())
    }
}

/// Normalise `items` into the canonical set a [`SortedSet`] requires: sorted
/// ascending by field value, with duplicates removed.
///
/// This is the producer-side counterpart to [`SortedSet`]: the encoder
/// *asserts* the order (and never sorts, to stay in lock-step with the
/// in-circuit fold), so every producer of an entity (wallet, attester, tests)
/// must normalise its vector fields first. Calling this is how they do it — the
/// ordering is defined here, in one place, so a producer can never drift from
/// the order the precompile / circuit recompute against.
///
/// The order is by canonical field *value* (`to_field().into_bigint()`), which
/// — for the canonical, `< modulus` ids these sets hold — matches a plain
/// big-endian byte / `uint256` comparison, so it agrees with a contract-side
/// `require(ids[i] > ids[i-1])`. A set is unique by definition, so repeated
/// elements are collapsed to one. A non-canonical element is rejected by
/// [`FieldElement::to_field`]. (Deduping here is a convenience for the producer;
/// the on-chain `require` + precompile still *reject* duplicates on any path
/// that bypasses this helper, so a genuine double-reference is caught.)
pub fn sort_set<T: FieldElement + Clone>(items: &[T]) -> Result<Vec<T>, Error> {
    let mut keyed: Vec<(<Fr as PrimeField>::BigInt, &T)> = items
        .iter()
        .map(|t| Ok((t.to_field()?.into_bigint(), t)))
        .collect::<Result<_, Error>>()?;
    keyed.sort_unstable_by_key(|(k, _)| *k);
    // After sorting, equal field values are adjacent — collapse each run to its
    // first element so the result is a true set.
    keyed.dedup_by(|a, b| a.0 == b.0);
    Ok(keyed.into_iter().map(|(_, t)| t.clone()).collect())
}

// ---- containers: encode each element in order ----

impl<T: FieldEncode + ?Sized> FieldEncode for &T {
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        (*self).encode(out)
    }
}
impl<T: FieldEncode> FieldEncode for [T] {
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        for x in self {
            x.encode(out)?;
        }
        Ok(())
    }
}
impl<T: FieldEncode> FieldEncode for Vec<T> {
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        self.as_slice().encode(out)
    }
}
impl<T: FieldEncode, const N: usize> FieldEncode for [T; N] {
    fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        self.as_slice().encode(out)
    }
}

// ---- alloy ABI scalar types (behind the `alloy` feature) ----
//
// These impls live here, not in a consumer crate, because the orphan rule
// forbids `impl FieldEncode for alloy::Address` anywhere but the trait's
// own crate. They mirror the Solidity → field convention exactly:
// `address`/`bytes32` are single elements; `uint256` uses the canonical
// `[120, 120, 16]`-bit limb representation shared with Noir.
#[cfg(feature = "alloy")]
mod alloy_impls {
    use super::{
        field_from_be_bytes, field_from_be_bytes_canonical, field_to_be_array, u256_limbs_be,
        Error, FieldElement, FieldEncode, Fr,
    };
    use alloy_primitives::{Address, FixedBytes, U16, U256, U32, U64};
    use ark_ff::PrimeField;

    // `address` — 160 bits, always < the field modulus, so total.
    impl FieldElement for Address {
        fn to_field(&self) -> Result<Fr, Error> {
            Ok(field_from_be_bytes(self.as_slice()))
        }

        fn from_field(value: &Fr) -> Result<Self, Error> {
            field_to_be_array::<20>(value, "address").map(Self::from)
        }
    }
    impl FieldEncode for Address {
        fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
            out.push(FieldElement::to_field(self)?);
            Ok(())
        }
    }

    // `bytes32` carrying a field element (an id, a `derivedOwner`):
    // decoded canonically, rejecting a value >= the modulus.
    impl FieldElement for FixedBytes<32> {
        fn to_field(&self) -> Result<Fr, Error> {
            field_from_be_bytes_canonical(self.as_slice(), "bytes32")
        }

        fn from_field(value: &Fr) -> Result<Self, Error> {
            field_to_be_array::<32>(value, "bytes32").map(Self::from)
        }
    }
    impl FieldEncode for FixedBytes<32> {
        fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
            out.push(FieldElement::to_field(self)?);
            Ok(())
        }
    }

    // `uintN` for N <= 64 — one 64-bit limb, always < the field modulus,
    // so a single element.
    macro_rules! uint_one_limb {
        ($($t:ty),*) => {$(
            impl FieldElement for $t {
                fn to_field(&self) -> Result<Fr, Error> {
                    Ok(Fr::from(self.as_limbs()[0]))
                }
                fn from_field(value: &Fr) -> Result<Self, Error> {
                    Self::checked_from_limbs_slice(value.into_bigint().as_ref())
                        .ok_or(Error::NonCanonical(stringify!($t)))
                }
            }
            impl FieldEncode for $t {
                fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
                    out.push(FieldElement::to_field(self)?);
                    Ok(())
                }
            }
        )*};
    }
    uint_one_limb!(U16, U32, U64);

    // `uint256` uses the canonical noir-bignum limbs. Intentionally not a
    // `FieldElement`, so it cannot seed an id or be an owner.
    impl FieldEncode for U256 {
        fn encode(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
            out.extend(u256_limbs_be(&self.to_be_bytes::<32>()).map(Fr::from));
            Ok(())
        }
    }
}

// ====================== byte (de)serialization ==========================

/// Compress a Grumpkin key into its canonical 32-byte representation.
fn serialize<T: CanonicalSerialize>(value: &T) -> Result<[u8; 32], Error> {
    let mut bytes = [0u8; 32];
    let mut output = bytes.as_mut_slice();
    value
        .serialize_compressed(&mut output)
        .map_err(|_| Error::Signature("serialize failed"))?;
    if !output.is_empty() {
        return Err(Error::Signature("serialized length != 32"));
    }
    Ok(bytes)
}

/// Field element → owned 32-byte big-endian vector.
pub fn field_to_be_bytes(f: &Fr) -> Vec<u8> {
    f.into_bigint().to_bytes_be()
}

/// 32-byte big-endian → field element (reduced mod order).
pub fn field_from_be32(b: &[u8; 32]) -> Fr {
    Fr::from_be_bytes_mod_order(b)
}

/// Decode a big-endian field word, rejecting values >= the field modulus.
#[cfg(feature = "alloy")]
pub fn field_from_b256(value: &B256) -> Result<Fr, Error> {
    value.to_field()
}

/// Encode a field as a zero-padded big-endian word.
#[cfg(feature = "alloy")]
pub fn field_to_b256(value: &Fr) -> Result<B256, Error> {
    B256::from_field(value)
}

/// Encode a full-width amount as three little-endian `[120, 120, 16]`-bit
/// field limbs, using the same representation as [`FieldEncode`].
#[cfg(feature = "alloy")]
pub fn fields_from_u256(value: &U256) -> Result<[Fr; 3], Error> {
    Ok(u256_limbs_be(&value.to_be_bytes::<32>()).map(Fr::from))
}

/// Recombine exactly three canonical `[120, 120, 16]`-bit field limbs.
#[cfg(feature = "alloy")]
pub fn fields_to_u256(fields: &[Fr]) -> Result<U256, Error> {
    let [lo, mid, hi] = fields else {
        return Err(Error::NonCanonical("uint256 requires three field limbs"));
    };
    let limbs = [
        u128::from_field(lo)?,
        u128::from_field(mid)?,
        u128::from_field(hi)?,
    ];
    u256_from_limbs_be(limbs)
        .map(U256::from_be_bytes)
        .ok_or(Error::NonCanonical("uint256 limbs"))
}

/// Grumpkin secret scalar → its compressed 32-byte representation.
pub fn secret_to_bytes(s: &Scalar) -> Result<[u8; 32], Error> {
    serialize(s)
}

/// Bytes → Grumpkin secret scalar.
pub fn secret_from_bytes(b: &[u8]) -> Result<Scalar, Error> {
    Scalar::deserialize_compressed(b).map_err(|_| Error::Signature("bad scalar"))
}

/// Public key from its secret: `pk = sk·G`.
pub fn public_key_from_secret(sk: &Scalar) -> Affine {
    mul_generator(sk).into_affine()
}

/// Public key → its compressed 32-byte representation.
pub fn public_key_to_bytes(p: &Affine) -> Result<[u8; 32], Error> {
    serialize(p)
}

/// Bytes → Grumpkin public key.
pub fn public_key_from_bytes(b: &[u8]) -> Result<Affine, Error> {
    Affine::deserialize_compressed(b).map_err(|_| Error::Signature("bad public key"))
}
