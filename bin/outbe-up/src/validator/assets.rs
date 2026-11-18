use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
};

use eyre::{Context, Result};
use outbe_operator_systemd::joining_units;

pub const OPERATOR_CONFIG_EXAMPLE: &str =
    include_str!("../../templates/validator/operator-config.example.yaml");

pub fn write_config_example(path: &Path) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create operator config: {}", path.display()))?;
    file.write_all(OPERATOR_CONFIG_EXAMPLE.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    println!("operator config: {}", path.display());
    Ok(())
}

pub fn systemd_units() -> Result<Vec<(String, String)>> {
    Ok(joining_units("/usr/local/bin/outbe-up")
        .into_iter()
        .map(|unit| (unit.name, unit.content))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::config::OperatorConfig;

    #[test]
    fn all_units_render_without_tokens() {
        let units = systemd_units().unwrap();
        assert_eq!(units.len(), 7);
        assert!(units.iter().all(|(_, unit)| !unit.contains("@@")));
    }

    #[test]
    fn example_config_is_valid_yaml() {
        serde_yaml_ng::from_str::<OperatorConfig>(OPERATOR_CONFIG_EXAMPLE).unwrap();
    }

    #[test]
    fn config_example_writes_the_embedded_example_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("operator-config.yaml");
        write_config_example(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), OPERATOR_CONFIG_EXAMPLE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(write_config_example(&path).is_err());
    }
}
