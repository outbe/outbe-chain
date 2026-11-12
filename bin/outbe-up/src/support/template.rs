use std::collections::BTreeMap;

use eyre::{bail, Result};

pub fn render(input: &str, values: &[(&str, &str)]) -> Result<String> {
    let values = values.iter().copied().collect::<BTreeMap<_, _>>();
    let mut rendered = input.to_owned();
    for (name, value) in values {
        rendered = rendered.replace(&format!("@{name}@"), value);
    }
    if let Some(start) = rendered.find("@OUTBE_") {
        let suffix = &rendered[start + 1..];
        let end = suffix
            .find('@')
            .map_or(rendered.len(), |end| start + end + 2);
        bail!("unresolved template token: {}", &rendered[start..end]);
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unresolved_outbe_token() {
        assert!(render("@OUTBE_UNKNOWN@", &[]).is_err());
    }
}
