//! Running the project's own wiring tasks against a chain.
//!
//! Deploy scripts place contracts; the roles and peer registrations that make
//! them callable live in hardhat tasks, and production wires through exactly
//! these. Reimplementing them here would drift the moment one changes.

use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{bail, Result};

use crate::world::forge::DEPLOYER_KEY;

/// Network name the tasks are pointed at. The hardhat config builds this entry
/// from the environment, so a scenario's throwaway endpoint needs no config.
const NETWORK: &str = "e2e";

fn resolve_hardhat_cli(dir: &Path) -> Result<PathBuf> {
    let cli = dir.join("node_modules/.bin/hardhat");
    if !cli.is_file() {
        bail!(
            "repository-local Hardhat CLI is missing at {}",
            cli.display()
        );
    }
    Ok(cli)
}

/// Run one wiring task against `url`.
pub(crate) fn task(
    dir: &Path,
    name: &str,
    args: &[(&str, String)],
    url: &str,
    chain_id: u64,
) -> Result<()> {
    let mut cmd = Command::new(resolve_hardhat_cli(dir)?);
    cmd.current_dir(dir)
        .args([name, "--network", NETWORK])
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::resolve_hardhat_cli;

    #[test]
    fn repository_local_hardhat_cli_is_the_only_command_source() {
        let root = tempdir().unwrap();
        let cli = root.path().join("node_modules/.bin/hardhat");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(&cli, b"#!/bin/sh\n").unwrap();

        assert_eq!(resolve_hardhat_cli(root.path()).unwrap(), cli);
    }

    #[test]
    fn missing_repository_local_hardhat_cli_fails_with_its_exact_path() {
        let root = tempdir().unwrap();
        let expected = root.path().join("node_modules/.bin/hardhat");

        let error = resolve_hardhat_cli(root.path()).unwrap_err().to_string();
        assert!(error.contains(&expected.display().to_string()));
    }
}
