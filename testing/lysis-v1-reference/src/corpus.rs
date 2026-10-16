use std::{collections::BTreeSet, path::Path};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::strict_json;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorpusCase {
    pub(crate) case_id: String,
    pub(crate) requirements: Vec<String>,
    pub(crate) input: CorpusInput,
    pub(crate) expected: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorpusInput {
    pub(crate) worldwide_day: u32,
    pub(crate) logical_evaluation_time: u64,
    pub(crate) lysis_limit_minor: String,
    pub(crate) tributes: Vec<CorpusTribute>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorpusTribute {
    pub(crate) tribute_id: String,
    pub(crate) owner: String,
    pub(crate) worldwide_day: u32,
    pub(crate) issuance_currency: u16,
    pub(crate) nominal: String,
    pub(crate) reference_currency: u16,
    pub(crate) tribute_price: String,
    pub(crate) f1: Option<u16>,
    pub(crate) f2: Option<u16>,
    pub(crate) entry_price: Option<String>,
    pub(crate) nod_target_available: bool,
}

pub(crate) fn load_cases(path: &Path) -> Result<Vec<CorpusCase>> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed reading {}", path.display()))?;
    let mut identifiers = BTreeSet::new();
    let mut cases = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let case: CorpusCase = strict_json(line)
            .wrap_err_with(|| format!("invalid case at {}:{}", path.display(), index + 1))?;
        ensure!(
            identifiers.insert(case.case_id.clone()),
            "duplicate case_id {:?} at line {}",
            case.case_id,
            index + 1
        );
        cases.push(case);
    }
    Ok(cases)
}
