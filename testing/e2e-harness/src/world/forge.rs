//! Running the project's own deploy scripts against a chain.
//!
//! Both venues a scenario can own — the committee's chain and a local target —
//! are deployed by the same scripts with the same throwaway key, so the runner
//! and its inputs live here rather than in either of them.

use std::path::Path;
use std::process::Command;

use alloy_primitives::Address;
use eyre::{bail, eyre, Result};

/// The well-known first anvil account. Its key is public by construction, which
/// is the point: a throwaway venue is deployed without a production key. The
/// `e2e-test` contract addresses the node is built against derive from it, and
/// the committee's genesis funds it so the same signer works on both chains.
pub(crate) const DEPLOYER_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub(crate) const DEPLOYER_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// CREATE3 salt namespace for the throwaway set, kept away from the pinned one.
pub(crate) const SALT_VERSION: &str = "e2e-test";

/// Run a deploy script or `forge create` and return its stdout.
///
/// The environment is inherited so a mise-provisioned foundry resolves; only
/// the deploy inputs the scripts read are added.
pub(crate) fn run(dir: &Path, args: &[&str], env: &[(&str, String)], url: &str) -> Result<String> {
    run_with_ctor(dir, args, &[], env, url)
}

/// `--constructor-args` is variadic, so it can only ever be the last flag:
/// anything after it is swallowed as a constructor value.
pub(crate) fn run_with_ctor(
    dir: &Path,
    args: &[&str],
    ctor: &[&str],
    env: &[(&str, String)],
    url: &str,
) -> Result<String> {
    let mut cmd = Command::new("forge");
    cmd.current_dir(dir)
        .args(args)
        .args(["--broadcast", "--rpc-url", url]);
    // `forge create` takes the key directly; scripts read it from the env.
    if args.first() == Some(&"create") {
        cmd.args(["--private-key", DEPLOYER_KEY]);
    } else {
        cmd.env("DEPLOYER_PK", DEPLOYER_KEY)
            .env("DEPLOYER_PRIVATE_KEY", DEPLOYER_KEY);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    if !ctor.is_empty() {
        cmd.arg("--constructor-args").args(ctor);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        // Forge reports the failing call and its estimate on stdout, so a
        // stderr-only error hides which transaction was too heavy.
        bail!(
            "forge {} in {} failed: {}\n--- stdout ---\n{}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The address a deploy script logged after `label`.
pub(crate) fn address_from(output: &str, label: &str) -> Result<Address> {
    output
        .lines()
        .rev()
        .find_map(|line| line.split_once(label))
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .ok_or_else(|| eyre!("no address logged after {label:?}"))?
        .parse()
        .map_err(|error| eyre!("unparseable address after {label:?}: {error}"))
}
