//! Module-local protocol constants.

/// How long a pledge quote stays exercisable. The quote fixes the entry price and
/// therefore the whole geometry of the position, so a stale one cannot be spent —
/// and the vault credit it claimed cannot be parked forever either, which is why
/// the reservation and the quote share this one deadline.
///
/// The product paper lists this as TBD (§10); 15 minutes is the placeholder until
/// it is set.
pub const PLEDGE_QUOTE_TTL_SECS: u64 = 15 * 60;
