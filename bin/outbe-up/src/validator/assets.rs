use eyre::Result;
use outbe_operator_systemd::joining_units;

pub const OPERATOR_CONFIG_EXAMPLE: &str =
    include_str!("../../templates/validator/operator-config.example.yaml");

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
        assert!(!units.is_empty());
        assert!(units
            .iter()
            .any(|(name, _)| name == "outbe-validator.service"));
        assert!(units.iter().all(|(_, unit)| !unit.contains("@@")));
    }

    #[test]
    fn example_config_is_valid_yaml() {
        serde_yaml_ng::from_str::<OperatorConfig>(OPERATOR_CONFIG_EXAMPLE).unwrap();
    }
}
