//! Running the project's own wiring tasks against a chain.
//!
//! Deploy scripts place contracts; the roles and peer registrations that make
//! them callable live in hardhat tasks, and production wires through exactly
//! these. Reimplementing them here would drift the moment one changes.

use std::path::Path;
use std::process::Command;

use eyre::{bail, Result};

use crate::world::forge::DEPLOYER_KEY;

/// Network name the tasks are pointed at. The hardhat config builds this entry
/// from the environment, so a scenario's throwaway endpoint needs no config.
const NETWORK: &str = "e2e";

/// Run one wiring task against `url`.
pub(crate) fn task(
    dir: &Path,
    name: &str,
    args: &[(&str, String)],
    url: &str,
    chain_id: u64,
) -> Result<()> {
    let mut cmd = Command::new("yarn");
    cmd.current_dir(dir)
        .args(["hardhat", name, "--network", NETWORK])
        .env("WIRE_NETWORK", NETWORK)
        .env("WIRE_RPC_URL", url)
        .env("WIRE_CHAIN_ID", chain_id.to_string())
        .env("OUTBE_PRIVATE_KEY", DEPLOYER_KEY)
        .env("WIRE_PRIVATE_KEY", DEPLOYER_KEY);
    for (flag, value) in args {
        cmd.arg(flag).arg(value);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        bail!(
            "hardhat {name} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}
