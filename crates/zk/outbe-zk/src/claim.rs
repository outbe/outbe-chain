//! TributeDraft claim folded into the circuit's `nft_hash`.
//!
//! Field order matches the former `#[derive(Entity)]` preimage: id is the seed,
//! then owner, day, currency, base, atto, and the sorted `su_ids` set.

#[cfg(feature = "alloy")]
use crate::codec::{FieldElement, FieldEncode, SortedSet};
#[cfg(feature = "alloy")]
use crate::error::Error;
#[cfg(feature = "alloy")]
use crate::protocol::entity::{Entity, Owned};
#[cfg(feature = "alloy")]
use crate::Fr;
#[cfg(feature = "alloy")]
use alloy_primitives::B256;
#[cfg(feature = "alloy")]
#[derive(Clone, Debug)]
pub struct TributeDraftClaim {
    pub id: B256,
    pub derived_owner: B256,
    pub worldwide_day: u64,
    pub currency: u16,
    pub base: u64,
    pub atto: u64,
    pub su_ids: Vec<B256>,
}

#[cfg(feature = "alloy")]
impl Entity for TributeDraftClaim {
    fn id_seed(&self) -> Result<Fr, Error> {
        self.id.to_field()
    }

    fn encode_id_body(&self, _out: &mut Vec<Fr>) -> Result<(), Error> {
        Ok(())
    }

    fn encode_body(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        out.push(self.derived_owner.to_field()?);
        out.push(self.worldwide_day.to_field()?);
        out.push(self.currency.to_field()?);
        out.push(self.base.to_field()?);
        out.push(self.atto.to_field()?);
        SortedSet(&self.su_ids).encode(out)?;
        Ok(())
    }
}

#[cfg(feature = "alloy")]
impl Owned for TributeDraftClaim {
    fn owner(&self) -> Result<Fr, Error> {
        self.derived_owner.to_field()
    }
}
