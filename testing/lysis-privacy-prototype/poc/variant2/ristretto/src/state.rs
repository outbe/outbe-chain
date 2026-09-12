use crate::{crypto::*, vss::point_hex};
use curve25519_dalek::scalar::Scalar;
use num_bigint::BigUint;
use num_traits::One;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub value: String,
    pub blinds: Vec<String>,
}
impl Note {
    pub fn fresh(v: BigUint) -> Result<Self> {
        if v.bits() > 256 {
            return Err("uint256 note overflow".into());
        }
        Ok(Self {
            value: v.to_string(),
            blinds: (0..4)
                .map(|_| scalar_integer(Scalar::random(&mut OsRng)).to_string())
                .collect(),
        })
    }
    pub fn zero() -> Self {
        Self {
            value: "0".into(),
            blinds: vec!["0".into(); 4],
        }
    }
    pub fn commitments(&self) -> Result<Vec<String>> {
        if self.blinds.len() != 4 || integer(&self.value)?.bits() > 256 {
            return Err("invalid note width".into());
        }
        let n = integer(&self.value)?;
        let mask = (BigUint::one() << 64usize) - 1u32;
        (0..4)
            .map(|i| {
                point_hex(commit(
                    &((&n >> (64 * i)) & &mask),
                    scalar(&integer(&self.blinds[i])?)?,
                )?)
            })
            .collect()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Public {
    pub kind: String,
    pub context: String,
    pub notes: Vec<Vec<String>>,
    pub source: String,
    pub fraction: String,
    pub price: String,
    pub amount: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub public: Public,
    pub notes: Vec<Note>,
    pub nominal: String,
    pub blinder: String,
}
fn count(kind: &str) -> Result<usize> {
    match kind {
        "claim" => Ok(5),
        "move" => Ok(4),
        "withdraw" => Ok(2),
        "mint" | "pledge" => Ok(3),
        _ => Err("unknown state relation".into()),
    }
}
impl Public {
    pub fn validate(&self) -> Result<()> {
        if self.notes.len() != count(&self.kind)?
            || integer(&self.fraction)?.bits() > 256
            || integer(&self.price)?.bits() > 256
            || integer(&self.amount)?.bits() > 256
        {
            return Err("public state shape/range".into());
        }
        field_from_hex(&self.context)?;
        crate::vss::point(&self.source)?;
        for n in &self.notes {
            if n.len() != 4 {
                return Err("note limb count".into());
            }
            for p in n {
                crate::vss::point(p)?;
            }
        }
        Ok(())
    }
}
