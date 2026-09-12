use super::*;

#[test]
fn input_roundtrips_every_system_tx_kind() {
    for kind in [
        SystemTxKind::CertifiedParentAccounting,
        SystemTxKind::LateFinalizeCredits,
        SystemTxKind::CycleTick,
        SystemTxKind::RewardsGemDelivery,
        SystemTxKind::BoundaryOutcome,
        SystemTxKind::TeeBootstrap,
        SystemTxKind::OracleSlashWindow,
        SystemTxKind::HookEvents,
    ] {
        let input = input_for(kind);
        let encoded = input.encode().expect("input encodes");
        let decoded = SystemTxInputV2::decode(&encoded).expect("input decodes");
        assert_eq!(decoded, input);
        assert_eq!(decoded.kind(), kind);
    }
}
