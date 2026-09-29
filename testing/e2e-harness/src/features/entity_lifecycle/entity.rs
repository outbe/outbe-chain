//! The entity a lifecycle phase runs against, and what each entity supplies to it.

use std::str::FromStr;

use alloy_primitives::{Address, FixedBytes, U256};
use cucumber::Parameter;

use super::markets::{EUR_ISO, MYR_ISO};
use super::redeem::Mined;
use crate::world::settlement_currency::USD_ISO;
use crate::world::World;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Parameter)]
#[param(name = "entity", regex = "gem|Intex series|Nod")]
pub(crate) enum Entity {
    Gem,
    Series,
    Nod,
}

impl FromStr for Entity {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "gem" => Ok(Self::Gem),
            "Intex series" => Ok(Self::Series),
            "Nod" => Ok(Self::Nod),
            other => Err(format!("unknown lifecycle entity {other:?}")),
        }
    }
}

impl Entity {
    pub(crate) fn lifecycle(self) -> &'static dyn Lifecycle {
        match self {
            Self::Gem => &crate::features::gem_lifecycle::GemLifecycle,
            Self::Series => &crate::features::intex_lifecycle::IntexLifecycle,
            Self::Nod => &crate::features::nod_lifecycle::NodLifecycle,
        }
    }
}

/// The two points of a lifecycle where an entity can be paid for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Parameter)]
#[param(name = "phase", regex = "qualified|called")]
pub(crate) enum Phase {
    Qualified,
    Called,
}

impl FromStr for Phase {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "qualified" => Ok(Self::Qualified),
            "called" => Ok(Self::Called),
            other => Err(format!("unknown payment phase {other:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Parameter)]
#[param(name = "rail", regex = "ERC20|PayNote")]
pub(crate) enum Rail {
    Erc20,
    PayNote,
}

impl FromStr for Rail {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "ERC20" => Ok(Self::Erc20),
            "PayNote" => Ok(Self::PayNote),
            other => Err(format!("unknown payment rail {other:?}")),
        }
    }
}

/// A settlement currency, named by its ISO 4217 letters in the scenario text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Parameter)]
#[param(name = "currency", regex = "USD|MYR|EUR")]
pub(crate) struct Currency(pub(crate) u16);

impl FromStr for Currency {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "USD" => Ok(Self(USD_ISO)),
            "MYR" => Ok(Self(MYR_ISO)),
            "EUR" => Ok(Self(EUR_ISO)),
            other => Err(format!("unknown settlement currency {other:?}")),
        }
    }
}

/// What one payment settles: a whole gem or Nod, or some units of one Intex series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Item {
    Gem(U256),
    Series { id: FixedBytes<14>, units: u32 },
    Nod(U256),
}

/// One payable holding, its owner, and the currency it was issued in.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub(crate) item: Item,
    pub(crate) owner: Address,
    pub(crate) owner_key: String,
    pub(crate) issuance_currency: u16,
}

impl Target {
    /// A holding pays in its reference currency, USD, or in the currency it was issued in.
    pub(crate) fn accepts(&self, currency: u16) -> bool {
        currency == USD_ISO || currency == self.issuance_currency
    }
}

/// What an entity supplies to the phases every lifecycle shares.
pub(crate) trait Lifecycle: Sync {
    /// The floor every entity of the scenario shares.
    fn floor(&self, world: &World) -> U256;
    /// The call price every entity of the scenario shares.
    fn call_price(&self, world: &World) -> U256;
    fn assert_issued(&self, world: &mut World);
    fn qualified(&self, world: &World) -> bool;
    /// The two holdings paid in `phase`, in the order the scenario names their payments.
    fn targets(&self, world: &World, phase: Phase) -> [Target; 2];
    /// Every holding left unpaid reads Called.
    fn called(&self, world: &World) -> bool;
    fn assert_paid_settled(&self, world: &World);
    fn lapse_notice(&self, world: &mut World);
    fn assert_forfeited(&self, world: &mut World);
    /// Mine every paid holding into its owner's balance, one record per owner.
    fn mine_paid(&self, world: &mut World) -> Vec<Mined>;
    fn assert_soulbound(&self, world: &World);
}
