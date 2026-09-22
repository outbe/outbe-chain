//! Full native CE verification at its actual saved marker, independently of H/E/P.

use outbe_compressed_entities::{
    CeAuditReport, CeAuditVisitor, CeAuditWork, CeMdbxReadOnly, ExactParentIdentity,
};
use outbe_primitives::OutbeHeader;

use super::headers::verify_header_ce_marker;

/// The caller selects the retained canonical header at Q and supplies external
/// scratch already checked against every installed and protected native path.
/// The visitor can be `CeBodyAudit` when live body equality is also requested.
pub(crate) fn verify_ce(
    reader: &CeMdbxReadOnly,
    header: &OutbeHeader,
    work: &CeAuditWork,
    visitor: &mut impl CeAuditVisitor,
) -> eyre::Result<CeAuditReport> {
    let marker = reader.marker()?;
    verify_header_ce_marker(header, &marker)?;
    Ok(reader.audit_exact(
        ExactParentIdentity {
            commitment_scheme_version: marker.commitment_scheme_version,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        },
        work,
        visitor,
    )?)
}
