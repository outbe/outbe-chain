//! Offline OCOMP observations at the verified current canonical state.

use alloy_primitives::{B256, U256};
use eyre::ensure;
use outbe_intex::schema::SeriesId;
use outbe_primitives::time::WorldwideDay;

use super::Incomplete;

use super::canonical_state::CanonicalState;
use outbe_intex::schema::CertifiedContributorGenerationProjection;
use outbe_nod::schema::NodCertifiedGenerationProjection;
use outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1;
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_snapshot::layout::{validate_layout, ProtectedPaths};
use reth_ethereum::provider::db::{
    cursor::DbCursorRO,
    database::Database,
    mdbx::{create_db, DatabaseArguments},
    table::{Table, TableInfo},
    transaction::{DbTx, DbTxMut},
    DatabaseEnv, TableSet,
};
use std::path::Path;

/// Scratch-only sets avoid retaining the permanent series index in RAM.
#[derive(Debug)]
struct InventoryRows;
impl Table for InventoryRows {
    const NAME: &'static str = "SnapshotOcompInventory";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for InventoryRows {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        false
    }
}
impl TableSet for InventoryRows {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        Box::new(std::iter::once(Box::new(Self) as Box<dyn TableInfo>))
    }
}

#[derive(Default)]
pub(crate) struct InventoryBounds {
    pub active_intents: u64,
    pub nod_head: u64,
    pub nod_tail: u64,
    pub nod_entries: u64,
    pub series: u64,
    pub days: u64,
    pub unpaid_days: u64,
    pub bitmap_words: u64,
}

/// Canonical obligation discovery is independent of local job directories.
/// All observations borrow the same immutable verified E; visiting this inventory
/// does not infer that the corresponding local artifacts have been validated.
pub(crate) struct CanonicalInventory<'a, 'b> {
    state: &'a CanonicalState<'b>,
    db: DatabaseEnv,
    active_jobs: Vec<(B256, OcompJobRecordV1)>,
    pub bounds: InventoryBounds,
    _directory: tempfile::TempDir,
}

fn inventory_key(kind: u8, identity: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + identity.len());
    key.push(kind);
    key.extend_from_slice(identity);
    key
}

fn scan_budget(label: &str, visited: u64, total: u64, maximum: Option<u64>) -> eyre::Result<()> {
    if maximum.is_some_and(|maximum| visited >= maximum) {
        return Err(Incomplete(format!("{label} scan stopped at {visited}/{total}")).into());
    }
    Ok(())
}

impl<'a, 'b> CanonicalInventory<'a, 'b> {
    pub(crate) fn scan(
        state: &'a CanonicalState<'b>,
        scratch_parent: &Path,
        protected: &ProtectedPaths,
        maximum_records: Option<u64>,
    ) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
        // The owner bounds its native aggregate before allocation and validates
        // live scheduler/FSM/job equivalence. No local directory seeds this list.
        let active_jobs = state.live_ocomp_jobs()?;
        let active_intents = u64::try_from(active_jobs.len())?;
        if maximum_records.is_some_and(|maximum| active_intents > maximum) {
            return Err(Incomplete(format!(
                "active intent scan requires {active_intents} records, exceeding configured budget"
            ))
            .into());
        }
        let directory = tempfile::Builder::new()
            .prefix("outbe-ocomp-inventory-")
            .tempdir_in(scratch_parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        let tx = db.tx_mut()?;
        let (nod_head, nod_tail) = state.nod_materialization_bounds()?;
        ensure!(
            nod_head > 0 && nod_head <= nod_tail,
            "invalid NOD FIFO bounds"
        );
        ensure!(
            state.nod_materialization_day(nod_tail)?.value() == 0,
            "NOD FIFO next-free tail is occupied"
        );
        let head = state.nod_materialization_head()?;
        ensure!(
            head.is_some() == (nod_head != nod_tail),
            "NOD FIFO head presence differs"
        );
        let mut bounds = InventoryBounds {
            active_intents,
            nod_head,
            nod_tail,
            ..Default::default()
        };
        for sequence in nod_head..nod_tail {
            scan_budget(
                "NOD FIFO",
                bounds.nod_entries,
                nod_tail - nod_head,
                maximum_records,
            )?;
            let day = state.nod_materialization_day(sequence)?;
            ensure!(
                day.value() != 0 && day.is_valid(),
                "invalid NOD FIFO day at {sequence}"
            );
            let projection = state
                .nod_certified_generation(day)?
                .ok_or_else(|| eyre::eyre!("missing certified NOD generation at {sequence}"))?;
            ensure!(
                projection.worldwide_day == day
                    && !projection.job_id.is_zero()
                    && !projection.protocol_bundle_hash.is_zero()
                    && !projection.program_semantics_hash.is_zero()
                    && projection.next_nod_ordinal < projection.nod_count,
                "invalid or completed NOD generation remains queued at {sequence}"
            );
            if sequence == nod_head {
                ensure!(
                    head.as_ref()
                        == Some(&NodMaterializationHeadV1 {
                            queue_sequence: sequence,
                            job_id: projection.job_id,
                            program_semantics_hash: projection.program_semantics_hash,
                            worldwide_day: day.value(),
                            generation: projection.generation,
                            nod_root: projection.nod_root,
                            nod_count: projection.nod_count,
                            next_nod_ordinal: projection.next_nod_ordinal,
                            last_progress_height: projection.last_progress_height,
                        }),
                    "NOD FIFO first projection differs from native head"
                );
            }
            for key in [
                inventory_key(b'd', &day.value().to_be_bytes()),
                inventory_key(b'j', projection.job_id.as_slice()),
            ] {
                ensure!(
                    tx.get::<InventoryRows>(key.clone())?.is_none(),
                    "duplicate NOD FIFO day/job"
                );
                tx.put::<InventoryRows>(key, Vec::new())?;
            }
            bounds.nod_entries += 1;
        }

        let total_series = state.intex_total_series()?;
        for index in 0..total_series {
            scan_budget("Intex series", index, total_series, maximum_records)?;
            let id = state.intex_series_id_at(index)?;
            let day = verify_series_day(id)?;
            let record = state.intex_read_series(id)?;
            ensure!(
                record.series_id == id && record.worldwide_day == day,
                "Intex series record identity/day differs from permanent index"
            );
            let key = inventory_key(b's', id.as_bytes());
            ensure!(
                tx.get::<InventoryRows>(key.clone())?.is_none(),
                "duplicate Intex series identity"
            );
            tx.put::<InventoryRows>(key, Vec::new())?;
            bounds.series += 1;
            let day_key = inventory_key(b'w', &day.value().to_be_bytes());
            if tx.get::<InventoryRows>(day_key.clone())?.is_some() {
                continue;
            }
            tx.put::<InventoryRows>(day_key, Vec::new())?;
            bounds.days += 1;
            let certified = state.intex_certified_contributor_generation(day)?;
            let Some(round) = state.intex_certified_payout_round(day.value())? else {
                continue;
            };
            let certified =
                certified.ok_or_else(|| eyre::eyre!("payout round lacks certified generation"))?;
            ensure!(
                round.wwd == day.value()
                    && round.active != 0
                    && certified.worldwide_day == day.value()
                    && certified.contributor_count > 0
                    && round.paid_so_far <= round.amount,
                "inconsistent certified payout round"
            );
            let bitmap = verify_paid_bitmap(
                certified.contributor_count,
                round.paid_leaf_count,
                maximum_records,
                |word| state.intex_paid_leaves_word(day.value(), word),
            )?;
            bounds.bitmap_words = bounds
                .bitmap_words
                .checked_add(bitmap.words)
                .ok_or_else(|| eyre::eyre!("payout bitmap word count overflow"))?;
            if bitmap.unpaid > 0 {
                tx.put::<InventoryRows>(
                    inventory_key(b'p', &day.value().to_be_bytes()),
                    Vec::new(),
                )?;
                bounds.unpaid_days += 1;
            }
        }
        tx.commit()?;
        Ok(Self {
            state,
            db,
            active_jobs,
            bounds,
            _directory: directory,
        })
    }

    pub(crate) fn active_jobs(&self) -> &[(B256, OcompJobRecordV1)] {
        &self.active_jobs
    }

    pub(crate) fn visit_nod(
        &self,
        visitor: &mut impl FnMut(u64, NodCertifiedGenerationProjection) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        for sequence in self.bounds.nod_head..self.bounds.nod_tail {
            let day = self.state.nod_materialization_day(sequence)?;
            let projection = self
                .state
                .nod_certified_generation(day)?
                .ok_or_else(|| eyre::eyre!("validated NOD generation disappeared"))?;
            visitor(sequence, projection)?;
        }
        Ok(())
    }

    pub(crate) fn visit_payouts(
        &self,
        visitor: &mut impl FnMut(
            WorldwideDay,
            CertifiedContributorGenerationProjection,
        ) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let tx = self.db.tx()?;
        let mut cursor = tx.cursor_read::<InventoryRows>()?;
        for row in cursor.walk(Some(vec![b'p']))? {
            let (key, _) = row?;
            if key.first() != Some(&b'p') {
                break;
            }
            let day = WorldwideDay::new(u32::from_be_bytes(key[1..].try_into()?));
            let certified = self
                .state
                .intex_certified_contributor_generation(day)?
                .ok_or_else(|| eyre::eyre!("validated contributor generation disappeared"))?;
            visitor(day, certified)?;
        }
        Ok(())
    }
}

/// Counts from a complete native payout bitmap, including non-prefix payments.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BitmapAudit {
    pub words: u64,
    pub paid: u64,
    pub unpaid: u64,
}

pub(crate) fn verify_paid_bitmap(
    contributor_count: u32,
    paid_leaf_count: u32,
    maximum_words: Option<u64>,
    mut read_word: impl FnMut(u32) -> eyre::Result<U256>,
) -> eyre::Result<BitmapAudit> {
    ensure!(
        paid_leaf_count <= contributor_count,
        "paid leaf count exceeds certified count"
    );
    let words = u64::from(contributor_count).div_ceil(256);
    let mut paid = 0_u64;
    for index in 0..words {
        if maximum_words.is_some_and(|maximum| index >= maximum) {
            return Err(Incomplete(format!(
                "payout bitmap scan stopped at {index}/{words} words"
            ))
            .into());
        }
        let word = read_word(u32::try_from(index)?)?;
        let bits = (u64::from(contributor_count) - index * 256).min(256);
        if bits < 256 {
            ensure!(
                (word >> bits).is_zero(),
                "paid bitmap contains bits above certified contributor count"
            );
        }
        paid = paid
            .checked_add(word.count_ones() as u64)
            .ok_or_else(|| eyre::eyre!("paid bitmap count overflow"))?;
    }
    ensure!(
        paid == u64::from(paid_leaf_count),
        "paid bitmap popcount differs from paid leaf count"
    );
    Ok(BitmapAudit {
        words,
        paid,
        unpaid: u64::from(contributor_count) - paid,
    })
}

/// Native SeriesId::worldwide_day is intentionally permissive; validate its
/// stored spelling and date before using it to discover canonical obligations.
pub(crate) fn verify_series_day(id: SeriesId) -> eyre::Result<WorldwideDay> {
    let bytes = id.as_bytes();
    ensure!(
        bytes[..8].iter().all(u8::is_ascii_digit) && bytes[8] == b'-' && bytes[12] == b'-',
        "noncanonical series identity"
    );
    let day = id.worldwide_day();
    ensure!(
        day.value() != 0 && day.is_valid(),
        "invalid series worldwide day"
    );
    let issuance = [bytes[9], bytes[10], bytes[11]];
    ensure!(
        SeriesId::pack(day, issuance, bytes[13])? == id,
        "noncanonical series codes"
    );
    Ok(day)
}
