//! Parity between the Solidity that Rust binds and the JSON the MCP consumes.
//!
//! Rust generates its ABI from `contracts/precompiles/src/I*.sol` via `sol!`,
//! while the TypeScript MCP reads `contracts/precompiles/abi-export/I*.json`.
//! Those are two derivations of one source, and only the second is a committed
//! artifact — so a `.sol` edit without `mise run export-abi` would silently
//! leave the two consumers speaking different ABIs.
//!
//! This compares the full selector set of each interface, computed
//! independently on both sides, so a stale export fails here rather than at a
//! reverted call.

use alloy_primitives::keccak256;
use alloy_sol_types::sol;
use std::collections::BTreeSet;

sol!("../../../contracts/precompiles/src/ITribute.sol");
sol!("../../../contracts/precompiles/src/INod.sol");
sol!("../../../contracts/precompiles/src/IValidatorSet.sol");
sol!("../../../contracts/precompiles/src/IStaking.sol");
sol!("../../../contracts/precompiles/src/IOracle.sol");
sol!("../../../contracts/precompiles/src/IGovernance.sol");
sol!("../../../contracts/precompiles/src/IMetadosis.sol");
sol!("../../../contracts/precompiles/src/ITeeRegistryV1.sol");

/// Canonical ABI type of one input/output entry: tuples expand to `(a,b,c)`,
/// arrays keep their suffix. This is what keccak is taken over.
fn canonical_type(entry: &serde_json::Value) -> String {
    let ty = entry["type"].as_str().unwrap_or_default();
    let Some(rest) = ty.strip_prefix("tuple") else {
        return ty.to_owned();
    };
    let inner: Vec<String> = entry["components"]
        .as_array()
        .map(|components| components.iter().map(canonical_type).collect())
        .unwrap_or_default();
    format!("({}){rest}", inner.join(","))
}

/// Every 4-byte function selector declared by an exported ABI JSON.
fn selectors_from_export(json: &str) -> BTreeSet<[u8; 4]> {
    let abi: serde_json::Value = serde_json::from_str(json).expect("abi-export is valid JSON");
    abi.as_array()
        .expect("abi-export is a JSON array")
        .iter()
        .filter(|entry| entry["type"] == "function")
        .map(|entry| {
            let args: Vec<String> = entry["inputs"]
                .as_array()
                .map(|inputs| inputs.iter().map(canonical_type).collect())
                .unwrap_or_default();
            let name = entry["name"].as_str().unwrap_or_default();
            let signature = format!("{name}({})", args.join(","));
            let hash = keccak256(signature.as_bytes());
            [hash[0], hash[1], hash[2], hash[3]]
        })
        .collect()
}

macro_rules! assert_export_matches {
    ($iface:ident, $calls:ty, $json:literal) => {{
        let exported = selectors_from_export(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/precompiles/abi-export/",
            $json
        )));
        let generated: BTreeSet<[u8; 4]> = <$calls>::SELECTORS.iter().copied().collect();
        assert_eq!(
            generated, exported,
            concat!(
                stringify!($iface),
                ": contracts/precompiles/src/",
                stringify!($iface),
                ".sol and abi-export/",
                $json,
                " disagree — run 'mise run export-abi' and commit the result"
            )
        );
    }};
}

#[test]
fn exported_abis_match_the_solidity_rust_binds() {
    assert_export_matches!(ITribute, ITribute::ITributeCalls, "ITribute.json");
    assert_export_matches!(INod, INod::INodCalls, "INod.json");
    assert_export_matches!(
        IValidatorSet,
        IValidatorSet::IValidatorSetCalls,
        "IValidatorSet.json"
    );
    assert_export_matches!(IStaking, IStaking::IStakingCalls, "IStaking.json");
    assert_export_matches!(IOracle, IOracle::IOracleCalls, "IOracle.json");
    assert_export_matches!(
        IGovernance,
        IGovernance::IGovernanceCalls,
        "IGovernance.json"
    );
    assert_export_matches!(IMetadosis, IMetadosis::IMetadosisCalls, "IMetadosis.json");
    assert_export_matches!(
        ITeeRegistryV1,
        ITeeRegistryV1::ITeeRegistryV1Calls,
        "ITeeRegistryV1.json"
    );
}
