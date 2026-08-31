//! Running the project's own deploy scripts against a chain.
//!
//! Both venues a scenario can own - the committee's chain and a local target -
//! are deployed by the same scripts with the same throwaway key, so the runner
//! and its inputs live here rather than in either of them.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
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
    let mut cmd = Command::new(forge_binary(&crate::env::environment().repo));
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

fn forge_binary(repo: &Path) -> PathBuf {
    resolve_forge_binary(
        repo,
        std::env::var_os("OUTBE_FORGE_BIN"),
        std::env::var_os("PATH"),
    )
}

fn resolve_forge_binary(
    repo: &Path,
    explicit: Option<OsString>,
    path: Option<OsString>,
) -> PathBuf {
    if let Some(explicit) = explicit.filter(|value| !value.is_empty()) {
        return explicit.into();
    }
    if let Some(found) = path
        .as_deref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("forge"))
        .find(|candidate| candidate.is_file())
    {
        return found;
    }
    for root in repo.ancestors() {
        let candidate = root.join(".local/share/mise/installs/foundry/latest/forge");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(OsStr::new("forge"))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn sanitized_sudo_path_falls_back_to_the_repo_owners_mise_forge() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("work/outbe-chain");
        let forge = root
            .path()
            .join(".local/share/mise/installs/foundry/latest/forge");
        fs::create_dir_all(repo.as_path()).unwrap();
        fs::create_dir_all(forge.parent().unwrap()).unwrap();
        fs::write(&forge, b"fixture").unwrap();

        assert_eq!(
            resolve_forge_binary(&repo, None, Some(OsString::from("/usr/bin:/bin"))),
            forge
        );
    }

    #[test]
    fn explicit_forge_path_wins() {
        assert_eq!(
            resolve_forge_binary(
                Path::new("/workspace/outbe-chain"),
                Some(OsString::from("/toolchain/forge")),
                None,
            ),
            PathBuf::from("/toolchain/forge")
        );
    }
}
