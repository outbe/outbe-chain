//! Generic entity model — **traits only**.
//!
//! An entity is a declarative preimage: a *seed* (folded to the id) and a
//! *body* (folded from the id to the hash). An entity *encodes itself*: it
//! pushes its members — through [`crate::codec::FieldEncode`] — in
//! canonical order, so the named, typed structure of an NFT lives in the
//! type, not in an out-of-band filling convention.
//!
//! This core declares only the [`Entity`] and [`Owned`] traits. The
//! concrete entity types (Solidity-mirroring `SpendingUnit`,
//! `TributeDraft`, …) live in consumer crates (e.g. `outbe-integration`),
//! where `#[derive(Entity)]` generates these impls from `#[outbe(...)]`
//! field roles.

use crate::error::Error;
use crate::{primitive::hash, Fr};

/// A hashable entity over BN254 field elements.
///
/// An entity says what to hash and in what order; Poseidon2 performs the fold.
/// `id_seed` plus
/// `encode_id_body` fold to the id; the id plus `encode_body` fold to the
/// entity hash.
pub trait Entity {
    /// Seed of the id accumulator (a single field element). Fallible
    /// because the seed may be a `bytes32` that must decode canonically.
    fn id_seed(&self) -> Result<Fr, Error>;
    /// Append the fields folded onto the seed to produce the id (push
    /// nothing if the id is stored directly, e.g. a random id).
    fn encode_id_body(&self, out: &mut Vec<Fr>) -> Result<(), Error>;
    /// Append the entity body, folded from the id to produce the hash.
    fn encode_body(&self, out: &mut Vec<Fr>) -> Result<(), Error>;

    /// The entity id (rolling-hash seed of the entity hash).
    fn id(&self) -> Result<Fr, Error> {
        let mut body = Vec::new();
        self.encode_id_body(&mut body)?;
        hash::nft_hash(self.id_seed()?, &body)
    }

    /// The canonical entity hash.
    fn entity_hash(&self) -> Result<Fr, Error> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        hash::nft_hash(self.id()?, &body)
    }
}

/// An entity that carries a `derivedOwner`.
pub trait Owned {
    /// The stored owner commitment. Fallible for the same reason as
    /// [`Entity::id_seed`]: it may decode from a `bytes32`.
    fn owner(&self) -> Result<Fr, Error>;
}

/// The ownership-relevant projection of an entity: its id, `derivedOwner`,
/// and entity hash. This is what an attester sends the client after
/// minting (the full body stays attester-side), and what the client
/// persists to later prove ownership and to reference the NFT on
/// submission (the `id`); `(owner, nft_hash)` provide the ownership
/// statement's entity data.
///
/// It is an [`Entity`] whose hash is *already known*: `entity_hash` returns
/// the stored value, so ownership-witness derivation works on a receipt
/// with no access to the original body.
pub struct OwnershipReceipt {
    /// Stored entity id
    pub id: Fr,
    /// Stored `derivedOwner`.
    pub owner: Fr,
    /// Precomputed entity hash.
    pub nft_hash: Fr,
}

impl Entity for OwnershipReceipt {
    fn id_seed(&self) -> Result<Fr, Error> {
        Ok(self.id)
    }
    fn encode_id_body(&self, _out: &mut Vec<Fr>) -> Result<(), Error> {
        Ok(())
    }
    fn encode_body(&self, _out: &mut Vec<Fr>) -> Result<(), Error> {
        Ok(())
    }
    // The hash is known up front; skip the body fold entirely.
    fn entity_hash(&self) -> Result<Fr, Error> {
        Ok(self.nft_hash)
    }
}
impl Owned for OwnershipReceipt {
    fn owner(&self) -> Result<Fr, Error> {
        Ok(self.owner)
    }
}
