//! Resolve operator-selected checks before opening any native store.

use std::collections::BTreeSet;

use super::report::CheckName;

pub(crate) struct CheckSelection {
    pub checks: BTreeSet<CheckName>,
}

impl CheckSelection {
    pub(crate) fn resolve(
        requested: &str,
        has_artifact_metadata: bool,
        has_expected_signer: bool,
    ) -> eyre::Result<Self> {
        use CheckName::*;
        let mut checks = BTreeSet::new();
        if requested == "all" {
            checks.extend([Headers, Evm, Ce, Bodies, Ocomp]);
            if has_artifact_metadata {
                checks.extend([Files, Provenance]);
            }
        } else {
            for name in requested.split(',') {
                checks.insert(match name {
                    "files" => Files,
                    "provenance" => Provenance,
                    "headers" => Headers,
                    "evm" => Evm,
                    "ce" => Ce,
                    "bodies" => Bodies,
                    "ocomp" => Ocomp,
                    _ => eyre::bail!("unknown snapshot check {name:?}"),
                });
            }
        }
        if has_expected_signer || checks.contains(&Files) {
            checks.insert(Provenance);
        }
        if checks.contains(&Bodies) {
            checks.insert(Ce);
        }
        if checks.contains(&Ocomp) {
            checks.insert(Evm);
        }
        if checks.contains(&Evm) || checks.contains(&Ce) {
            checks.insert(Headers);
        }
        Ok(Self { checks })
    }

    pub(crate) fn needs_projection(&self) -> bool {
        [CheckName::Files, CheckName::Bodies, CheckName::Ocomp]
            .iter()
            .any(|check| self.checks.contains(check))
    }
}
