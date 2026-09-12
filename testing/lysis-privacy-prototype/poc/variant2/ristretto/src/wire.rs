use crate::crypto::{self, Result};
use crate::link::{LinkCircuit, LinkPublic, LinkWitness};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub version: u16,
    pub derived_owner: String,
    pub nft_hash: String,
    pub binding_hash: String,
    pub merkle_root: String,
    pub sender: String,
    pub chain_id: u64,
    pub day: u64,
    pub currency: u16,
    pub reference_currency: u16,
    pub exclude_from_intex: bool,
    pub source_count: u16,
    pub source_ids: Vec<String>,
    pub issuance_vwap: String,
    pub reference_vwap: String,
    pub reference_scurve: String,
    pub commitment: String,
    pub opening_binding: String,
}
impl Offer {
    pub fn from_public(p: &LinkPublic) -> Result<Self> {
        Ok(Self {
            version: 1,
            derived_owner: crypto::field_hex(p.derived_owner),
            nft_hash: crypto::field_hex(p.nft_hash),
            binding_hash: crypto::field_hex(p.binding_hash),
            merkle_root: crypto::field_hex(p.merkle_root),
            sender: p.sender.to_string(),
            chain_id: p.chain_id,
            day: p.day,
            currency: p.currency,
            reference_currency: p.reference_currency,
            exclude_from_intex: p.exclude_from_intex,
            source_count: p.source_count,
            source_ids: p.source_ids.iter().map(|x| crypto::field_hex(*x)).collect(),
            issuance_vwap: p.issuance_vwap.to_string(),
            reference_vwap: p.reference_vwap.to_string(),
            reference_scurve: p.reference_scurve.to_string(),
            commitment: hex::encode(p.commitment.compress().to_bytes()),
            opening_binding: crypto::field_hex(p.opening_binding),
        })
    }
    pub fn to_public(&self) -> Result<LinkPublic> {
        if self.version != 1 || self.source_ids.is_empty() || self.source_ids.len() > 1024 {
            return Err("unsupported offer version/source capacity".into());
        }
        let p = LinkPublic {
            derived_owner: crypto::field_from_hex(&self.derived_owner)?,
            nft_hash: crypto::field_from_hex(&self.nft_hash)?,
            binding_hash: crypto::field_from_hex(&self.binding_hash)?,
            merkle_root: crypto::field_from_hex(&self.merkle_root)?,
            sender: crypto::integer(&self.sender)?,
            chain_id: self.chain_id,
            day: self.day,
            currency: self.currency,
            reference_currency: self.reference_currency,
            exclude_from_intex: self.exclude_from_intex,
            source_count: self.source_count,
            source_ids: self
                .source_ids
                .iter()
                .map(|x| crypto::field_from_hex(x))
                .collect::<Result<_>>()?,
            issuance_vwap: crypto::integer(&self.issuance_vwap)?,
            reference_vwap: crypto::integer(&self.reference_vwap)?,
            reference_scurve: crypto::integer(&self.reference_scurve)?,
            commitment: crypto::point_decode(&self.commitment)?,
            opening_binding: crypto::field_from_hex(&self.opening_binding)?,
        };
        p.validate()?;
        Ok(p)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wallet {
    pub offer: Offer,
    pub draft_id: String,
    pub base: u64,
    pub atto: u64,
    pub nominal: String,
    pub blinder: String,
    pub salt: String,
}
impl Wallet {
    pub fn from_circuit(c: &LinkCircuit) -> Result<Self> {
        Ok(Self {
            offer: Offer::from_public(&c.public)?,
            draft_id: crypto::field_hex(c.private.draft_id),
            base: c.private.base,
            atto: c.private.atto,
            nominal: c.private.nominal.to_string(),
            blinder: crypto::scalar_integer(c.private.blinder).to_string(),
            salt: crypto::field_hex(c.private.salt),
        })
    }
    pub fn circuit(&self) -> Result<LinkCircuit> {
        let nominal = crypto::integer(&self.nominal)?;
        if nominal.bits() > 104 || self.atto >= 1_000_000 {
            return Err("wallet source outside canonical current-source bounds".into());
        }
        Ok(LinkCircuit {
            public: self.offer.to_public()?,
            private: LinkWitness {
                draft_id: crypto::field_from_hex(&self.draft_id)?,
                base: self.base,
                atto: self.atto,
                nominal,
                blinder: crypto::scalar(&crypto::integer(&self.blinder)?)?,
                salt: crypto::field_from_hex(&self.salt)?,
            },
        })
    }
}
