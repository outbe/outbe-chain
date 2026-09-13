use reth_ethereum::TransactionSigned;

use super::{
    decode_system_tx_kind, is_reserved_system_tx, BodyZone, OcompLifecycleActivation,
    SystemTxError, SystemTxKind, MAX_SYSTEM_TXS_PER_BLOCK,
};

/// Structural split of block transactions into system begin-prefix, user middle,
/// and system end-suffix.
#[derive(Debug, Clone)]
pub struct SystemTxLayout<'a> {
    pub begin: Vec<&'a TransactionSigned>,
    pub user: Vec<&'a TransactionSigned>,
    pub end: Vec<&'a TransactionSigned>,
}

impl<'a> SystemTxLayout<'a> {
    pub fn is_empty(&self) -> bool {
        self.begin.is_empty() && self.user.is_empty() && self.end.is_empty()
    }

    pub fn system_tx_count(&self) -> usize {
        self.begin.len() + self.end.len()
    }

    pub fn begin_block_kinds(&self) -> Result<Vec<SystemTxKind>, SystemTxError> {
        self.begin
            .iter()
            .map(|tx| decode_system_tx_kind(tx))
            .collect()
    }

    pub fn end_block_kinds(&self) -> Result<Vec<SystemTxKind>, SystemTxError> {
        self.end
            .iter()
            .map(|tx| decode_system_tx_kind(tx))
            .collect()
    }

    /// True if the begin zone contains a system tx of `kind`. Used to derive the
    /// layout-signaled optional-phase flags (e.g. the one-time
    /// [`SystemTxKind::TeeBootstrap`]). A decode failure - which a
    /// successful [`split_system_layout`] precludes - is treated as absent.
    pub fn has_begin_kind(&self, kind: SystemTxKind) -> bool {
        self.begin_block_kinds()
            .map(|kinds| kinds.contains(&kind))
            .unwrap_or(false)
    }
}

pub fn split_system_layout<'a>(
    txs: &'a [TransactionSigned],
) -> Result<SystemTxLayout<'a>, SystemTxError> {
    let mut begin = Vec::new();
    let mut prefix_end = 0usize;
    let mut previous_begin = None;

    while prefix_end < txs.len() && is_reserved_system_tx(&txs[prefix_end]) {
        let kind = decode_system_tx_kind(&txs[prefix_end])?;
        if kind.body_zone() == BodyZone::EndBlock {
            break;
        }
        ensure_system_tx_in_zone(kind, BodyZone::BeginBlock)?;
        ensure_monotonic(BodyZone::BeginBlock, previous_begin, kind)?;
        previous_begin = Some(kind);
        begin.push(&txs[prefix_end]);
        prefix_end += 1;
    }

    let mut suffix_entries: Vec<(usize, SystemTxKind)> = Vec::new();
    let mut suffix_start = txs.len();
    while suffix_start > prefix_end && is_reserved_system_tx(&txs[suffix_start - 1]) {
        suffix_start -= 1;
        let kind = decode_system_tx_kind(&txs[suffix_start])?;
        ensure_system_tx_in_zone(kind, BodyZone::EndBlock)?;
        suffix_entries.push((suffix_start, kind));
    }
    suffix_entries.reverse();

    let mut previous_end = None;
    let mut end = Vec::with_capacity(suffix_entries.len());
    for (index, kind) in suffix_entries {
        ensure_monotonic(BodyZone::EndBlock, previous_end, kind)?;
        previous_end = Some(kind);
        end.push(&txs[index]);
    }

    for (offset, tx) in txs[prefix_end..suffix_start].iter().enumerate() {
        if is_reserved_system_tx(tx) {
            return Err(SystemTxError::MidBlockSystemTx {
                index: prefix_end + offset,
            });
        }
    }

    Ok(SystemTxLayout {
        begin,
        user: txs[prefix_end..suffix_start].iter().collect(),
        end,
    })
}

pub fn expected_begin_block_kinds(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
) -> Vec<SystemTxKind> {
    expected_begin_block_kinds_for_activation(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        OcompLifecycleActivation::Disabled,
    )
}

pub fn expected_begin_block_kinds_for_activation(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
    ocomp_activation: OcompLifecycleActivation,
) -> Vec<SystemTxKind> {
    let mut expected = match block_number {
        0 => Vec::new(),
        1 => Vec::new(),
        _ => {
            vec![
                SystemTxKind::CertifiedParentAccounting,
                // mandatory inclusion-window phase, ordered after Phase 1
                // and before CycleTick for every block >= 2 (empty when nothing to
                // credit; its body still drives the matured-window settlement).
                SystemTxKind::LateFinalizeCredits,
            ]
        }
    };
    if block_number > 0 && ocomp_activation.is_active_at(block_number) {
        expected.push(SystemTxKind::OcompLifecycleBegin);
    }
    if block_number > 0 {
        expected.push(SystemTxKind::CycleTick);
        expected.push(SystemTxKind::RewardsGemDelivery);
    }
    if block_number > 0 && has_boundary_outcome {
        expected.push(SystemTxKind::BoundaryOutcome);
    }
    if block_number > 0 && has_tee_bootstrap {
        expected.push(SystemTxKind::TeeBootstrap);
    }
    if block_number > 0 {
        expected.push(SystemTxKind::OracleSlashWindow);
        expected.push(SystemTxKind::HookEvents);
    }
    expected
}

pub fn expected_end_block_kinds(
    block_number: u64,
    ocomp_activation: OcompLifecycleActivation,
) -> Vec<SystemTxKind> {
    if block_number > 0 && ocomp_activation.is_active_at(block_number) {
        vec![SystemTxKind::OcompTerminalRequest]
    } else {
        Vec::new()
    }
}

pub fn validate_active_system_tx_set(
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
) -> Result<(), SystemTxError> {
    validate_system_tx_set_for_activation(
        layout,
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        OcompLifecycleActivation::Disabled,
    )
}

pub fn validate_system_tx_set_for_activation(
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
    ocomp_activation: OcompLifecycleActivation,
) -> Result<(), SystemTxError> {
    let actual = layout.system_tx_count();
    if actual > usize::from(MAX_SYSTEM_TXS_PER_BLOCK) {
        return Err(SystemTxError::TooManySystemTxs {
            actual,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        });
    }

    // / V2: block 1 mandatorily carries the genesis bootstrap
    // BoundaryOutcome. Reject the layout if the proposer omitted it; the
    // expected-kinds list rejection below is structural, this rejection is
    // protocol-level for V2 greenfield.
    if block_number == 1 && !has_boundary_outcome {
        return Err(SystemTxError::V2Block1MissingBoundaryOutcome);
    }
    if block_number == 1 && !has_tee_bootstrap {
        return Err(SystemTxError::V2Block1MissingTeeBootstrap);
    }
    if block_number != 1 && has_tee_bootstrap {
        return Err(SystemTxError::TeeBootstrapWrongHeight { block_number });
    }
    if block_number == 1 && !layout.user.is_empty() {
        return Err(SystemTxError::V2Block1ContainsUserTransactions {
            actual: layout.user.len(),
        });
    }

    let expected_begin = expected_begin_block_kinds_for_activation(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
        ocomp_activation,
    );
    let expected_end = expected_end_block_kinds(block_number, ocomp_activation);
    let actual_begin = layout.begin_block_kinds()?;
    let actual_end = layout.end_block_kinds()?;
    if actual_begin != expected_begin || actual_end != expected_end {
        return Err(SystemTxError::ActiveSystemTxSetMismatch {
            expected_begin,
            expected_end,
            actual_begin,
            actual_end,
        });
    }
    Ok(())
}

fn ensure_system_tx_in_zone(kind: SystemTxKind, actual: BodyZone) -> Result<(), SystemTxError> {
    let expected = kind.body_zone();
    if expected != actual {
        return Err(SystemTxError::SystemTxInWrongZone {
            kind,
            expected,
            actual,
        });
    }
    Ok(())
}

fn ensure_monotonic(
    zone: BodyZone,
    previous: Option<SystemTxKind>,
    current: SystemTxKind,
) -> Result<(), SystemTxError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    let previous_order = previous
        .order_in(zone)
        .ok_or(SystemTxError::SystemTxInWrongZone {
            kind: previous,
            expected: previous.body_zone(),
            actual: zone,
        })?;
    let current_order = current
        .order_in(zone)
        .ok_or(SystemTxError::SystemTxInWrongZone {
            kind: current,
            expected: current.body_zone(),
            actual: zone,
        })?;
    if current_order <= previous_order {
        return Err(SystemTxError::OutOfOrder {
            zone,
            previous,
            current,
        });
    }
    Ok(())
}
