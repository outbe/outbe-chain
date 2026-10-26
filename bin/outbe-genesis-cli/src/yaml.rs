//! Network YAML parser backed by `serde_yaml_ng`.

use std::fmt;
use std::fs;
use std::path::Path;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::error::{GenesisError, YamlError};

/// Wrapper around `serde_json::Value` that deserializes from `serde_yaml_ng` while rejecting
/// duplicate mapping keys, floating-point scalars, and null values.
#[derive(Debug)]
struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a valid Outbe network YAML value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_string())))
    }

    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_string())))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(
            "floating-point numbers are not supported in network YAML",
        ))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(
            "null or empty values are not supported; omit the key",
        ))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(
            "null or empty values are not supported; omit the key",
        ))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(StrictJsonValue(elem)) = seq.next_element::<StrictJsonValue>()? {
            items.push(elem);
        }
        Ok(StrictJsonValue(Value::Array(items)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key '{key}'")));
            }
            let StrictJsonValue(val) = map.next_value::<StrictJsonValue>()?;
            out.insert(key, val);
        }
        Ok(StrictJsonValue(Value::Object(out)))
    }
}

#[derive(Debug)]
struct StrictRootMap(Map<String, Value>);

impl<'de> Deserialize<'de> for StrictRootMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RootMapVisitor;
        impl<'de> Visitor<'de> for RootMapVisitor {
            type Value = StrictRootMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a top-level YAML mapping")
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let StrictJsonValue(Value::Object(out)) = StrictJsonVisitor.visit_map(map)? else {
                    unreachable!();
                };
                Ok(StrictRootMap(out))
            }
        }
        deserializer.deserialize_map(RootMapVisitor)
    }
}

/// Parse a YAML file via `serde_yaml_ng` into a JSON `Value::Object`.
pub fn load_yaml(path: &Path) -> Result<Value, GenesisError> {
    let text = fs::read_to_string(path).map_err(|err| GenesisError::io(path, err))?;
    parse_yaml_str(path, &text).map_err(GenesisError::from)
}

/// Parse a YAML string via `serde_yaml_ng` using `path` for error diagnostics.
pub fn parse_yaml_str(path: &Path, text: &str) -> Result<Value, YamlError> {
    let has_content = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .any(|l| !l.trim().is_empty());
    if !has_content {
        return Ok(Value::Object(Map::new()));
    }
    let deserializer = serde_yaml_ng::Deserializer::from_str(text);
    let StrictRootMap(map) =
        StrictRootMap::deserialize(deserializer).map_err(|source| YamlError {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Value::Object(map))
}
