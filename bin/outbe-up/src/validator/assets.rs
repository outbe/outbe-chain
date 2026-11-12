use eyre::Result;

use crate::support::template;

pub const OPERATOR_CONFIG_EXAMPLE: &str =
    include_str!("../../templates/validator/operator-config.example.yaml");

const UNITS: &[(&str, &str)] = &[
    (
        "outbe-enclave.service",
        include_str!("../../templates/validator/systemd/outbe-enclave.service"),
    ),
    (
        "outbe-feeder.service",
        include_str!("../../templates/validator/systemd/outbe-feeder.service"),
    ),
    (
        "outbe-full-node.service",
        include_str!("../../templates/validator/systemd/outbe-full-node.service"),
    ),
    (
        "outbe-radicle.service",
        include_str!("../../templates/validator/systemd/outbe-radicle.service"),
    ),
    (
        "outbe-tee-renew.service",
        include_str!("../../templates/validator/systemd/outbe-tee-renew.service"),
    ),
    (
        "outbe-tee-renew.timer",
        include_str!("../../templates/validator/systemd/outbe-tee-renew.timer"),
    ),
    (
        "outbe-validator.service",
        include_str!("../../templates/validator/systemd/outbe-validator.service"),
    ),
    (
        "outbe-validator.target",
        include_str!("../../templates/validator/systemd/outbe-validator.target"),
    ),
];

pub fn systemd_units() -> Result<Vec<(&'static str, String)>> {
    UNITS
        .iter()
        .map(|(name, source)| {
            template::render(source, &[("OUTBE_UP", "/usr/local/bin/outbe-up")])
                .map(|rendered| (*name, rendered))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::config::OperatorConfig;

    #[test]
    fn all_units_render_without_tokens() {
        let units = systemd_units().unwrap();
        assert_eq!(units.len(), UNITS.len());
        assert!(units.iter().all(|(_, unit)| !unit.contains("@OUTBE_")));
    }

    #[test]
    fn example_config_is_valid_yaml() {
        serde_yaml_ng::from_str::<OperatorConfig>(OPERATOR_CONFIG_EXAMPLE).unwrap();
    }
}
