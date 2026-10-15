//! The main config file and the pairs file that a top-level `pairs` key names.

use eyre::{Context, Result};
use std::path::Path;

use crate::errors::ConfigError;

/// Reads the main config. A top-level `pairs` key names a second file,
/// relative to the main one, whose sections join the table. A section set in
/// both files is an error.
pub(super) fn read_with_pairs(path: &str) -> Result<toml::Table> {
    let mut table = read_table(Path::new(path))?;
    let Some(pairs) = table.remove("pairs") else {
        return Ok(table);
    };
    let pairs = pairs.as_str().ok_or(ConfigError::PairsPathNotString)?;
    let pairs_path = Path::new(path)
        .parent()
        .unwrap_or(Path::new(""))
        .join(pairs);
    for (key, value) in read_table(&pairs_path)? {
        if table.insert(key.clone(), value).is_some() {
            return Err(ConfigError::SectionInBothFiles {
                key,
                main: path.to_owned(),
                pairs: pairs_path.display().to_string(),
            }
            .into());
        }
    }
    Ok(table)
}

fn read_table(path: &Path) -> Result<toml::Table> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}
