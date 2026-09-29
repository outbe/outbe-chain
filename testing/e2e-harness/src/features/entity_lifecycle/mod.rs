//! What the Gem, Intex and Nod lifecycle scenarios share: the markets they settle
//! against, one step per phase they all go through, and the chain reads every phase
//! is judged by. What an entity does differently sits behind [`entity::Lifecycle`].

pub(crate) mod chain;
pub(crate) mod entity;
pub(crate) mod guards;
pub(crate) mod markets;
pub(crate) mod payment;
pub(crate) mod phases;
pub(crate) mod redeem;

/// What the shared phases recorded as a scenario ran.
#[derive(Debug, Default)]
pub struct LifecycleLedger {
    pub(crate) payments: Vec<payment::Payment>,
    /// How many of `payments` a vault check already covered.
    pub(crate) verified: usize,
    /// The block and unallocated pool just before the call notice was let lapse.
    pub(crate) pool_before_forfeit: Option<(u64, alloy_primitives::U256)>,
    pub(crate) mined: Vec<redeem::Mined>,
    pub(crate) redeemed: Vec<redeem::Redeemed>,
}
