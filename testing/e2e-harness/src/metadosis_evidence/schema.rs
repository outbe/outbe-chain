//! Metadosis-owned runner receipts. PASS is deliberately absent: the
//! assembler derives assertion status from discovery and process exit facts.

use serde::{Deserialize, Serialize};

use crate::verification_ledger::{MemberDigestV1, SourceIdentityV1};

pub const METADOSIS_RUN_RECEIPT_SCHEMA_VERSION: u32 = 2;
pub const METADOSIS_RUN_RECEIPT_KIND: &str = "outbe.metadosis-verification-run";
pub const METADOSIS_NAMESPACE: &str = "METADOSIS";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandReceiptV1 {
    pub argv: Vec<String>,
    pub attempt: u32,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: MemberDigestV1,
    pub stderr: MemberDigestV1,
}

impl CommandReceiptV1 {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CargoDiscoveryReceiptV1 {
    pub normal: CommandReceiptV1,
    pub ignored: CommandReceiptV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRunReceiptV1 {
    pub test_id: String,
    pub discovery: Option<CargoDiscoveryReceiptV1>,
    pub execution: CommandReceiptV1,
    #[serde(default)]
    pub process_artifacts: Vec<MemberDigestV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetadosisRunReceiptV1 {
    pub schema_version: u32,
    pub kind: String,
    pub namespace: String,
    pub source: SourceIdentityV1,
    pub artifact_profile: String,
    pub lane: String,
    pub linux_image_digest: String,
    pub launcher_nonce: String,
    pub tests: Vec<TestRunReceiptV1>,
}
