pub mod chain;
pub mod epoch;
pub mod monitor;
pub mod oracle;
pub mod pledgenote;
pub mod rad;
pub mod radicle;
pub mod rewards;
pub mod slash;
pub mod stablecoin;
pub mod staking;
pub mod tee;
pub mod tribute;
pub mod validator;
pub mod vote;
pub mod zerofee;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use crate::tx::TxSigner;
use alloy_primitives::U256;
use eyre::{ensure, Result};
use serde::Serialize;
use zeroize::Zeroizing;

/// Parse a non-zero amount in decimal base units.
pub fn parse_amount(amount: &str) -> Result<U256> {
    let amount =
        U256::from_str_radix(amount, 10).map_err(|e| eyre::eyre!("invalid amount: {e}"))?;
    if amount.is_zero() {
        eyre::bail!("amount must be non-zero");
    }
    Ok(amount)
}

pub fn require_signer(private_key: Option<&str>) -> Result<TxSigner> {
    let key =
        private_key.ok_or_else(|| eyre::eyre!("--private-key required for this operation"))?;
    TxSigner::new(key)
}

fn private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => {
            // Persist the new directory entry as well as the file it will hold.
            #[cfg(unix)]
            fs::File::open(
                dir.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(dir)?.is_dir(),
                "{} must be a directory, not a symlink",
                dir.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    fs::metadata(dir)?.permissions().mode() & 0o022 == 0,
                    "{} must not be writable by other users",
                    dir.display()
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Immutable, durable publication: interruption leaves either no file or all of it.
pub(crate) fn save_json(dir: &Path, name: &str, value: &impl Serialize) -> Result<PathBuf> {
    private_dir(dir)?;
    let path = dir.join(name);
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(value)?);
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?; // owner-only on Unix
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file(),
                "refusing to overwrite {}",
                path.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    metadata.permissions().mode() & 0o077 == 0,
                    "{} must have owner-only permissions",
                    path.display()
                );
            }
            let existing = Zeroizing::new(fs::read(&path)?);
            ensure!(
                *existing == *bytes,
                "refusing to overwrite different contents in {}",
                path.display()
            );
        }
        Err(error) => return Err(error.error.into()),
    }
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(path)
}

fn format_decimals(value: alloy_primitives::U256, decimals: u8) -> String {
    use alloy_primitives::U256;

    if value.is_zero() {
        return "0".to_string();
    }

    let divisor = U256::from(10u64).pow(U256::from(decimals));
    let whole = value / divisor;
    let frac = value % divisor;

    if frac.is_zero() {
        format!("{whole}")
    } else {
        let frac_str = format!("{frac:0>width$}", width = usize::from(decimals));
        let trimmed = frac_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}

/// Format a raw native COEN amount in 18-decimal base units.
pub fn format_coen_amount(value: alloy_primitives::U256) -> String {
    format_decimals(value, 18)
}

/// Format a six-decimal protocol amount, such as a COEN/ISO oracle rate.
pub fn format_protocol_amount(value: alloy_primitives::U256) -> String {
    format_decimals(value, 6)
}

/// Format an independently owned FP18 value.
pub fn format_generic_fp18(value: alloy_primitives::U256) -> String {
    format_decimals(value, 18)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_format_unit_zero() {
        assert_eq!(format_coen_amount(U256::ZERO), "0");
    }

    #[test]
    fn test_format_unit_one_coen() {
        assert_eq!(
            format_coen_amount(U256::from(1_000_000_000_000_000_000u128)),
            "1"
        );
    }

    #[test]
    fn test_format_unit_large_whole() {
        let val = U256::from(1_000_000_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1000");
    }

    #[test]
    fn test_format_unit_fractional() {
        let val = U256::from(1_500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.5");
    }

    #[test]
    fn test_format_unit_pure_fraction() {
        let val = U256::from(500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "0.5");
    }

    #[test]
    fn test_format_unit_one_native_unit() {
        assert_eq!(format_coen_amount(U256::from(1u64)), "0.000000000000000001");
    }

    #[test]
    fn test_format_unit_trailing_zeros_trimmed() {
        let val = U256::from(1_200_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.2");
    }

    #[test]
    fn test_format_unit_all_decimal_places() {
        let val = U256::from(999_999_999_999_999_999u128);
        assert_eq!(format_coen_amount(val), "0.999999999999999999");
    }

    #[test]
    fn test_format_protocol_amount_remains_six_decimals() {
        assert_eq!(format_protocol_amount(U256::from(1_500_000u64)), "1.5");
    }

    #[test]
    fn saved_json_is_private_and_never_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("notes");
        let note = serde_json::json!({ "amount": 1 });
        let path = save_json(&dir, "note.json", &note).unwrap();
        assert_eq!(save_json(&dir, "note.json", &note).unwrap(), path);
        let original = fs::read(&path).unwrap();
        assert!(save_json(&dir, "note.json", &serde_json::json!({ "amount": 2 })).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let link = temp.path().join("linked");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert!(save_json(&link, "other.json", &note).is_err());
        }
    }

    #[test]
    fn test_format_unit_max_u256_does_not_panic() {
        let result = format_coen_amount(U256::MAX);
        assert!(!result.is_empty());
        assert!(result.contains('.'));
    }
}
