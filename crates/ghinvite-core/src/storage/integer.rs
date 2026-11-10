//! Identical integer contract for native SQLite, JSON projections and D1.
use super::{Error, Result};
pub const MAX_EXACT: u64 = (1u64 << 53) - 1;
pub fn unsigned(value: u64) -> Result<i64> {
    if value > MAX_EXACT {
        return Err(Error::Database(
            "integer exceeds portable storage range".into(),
        ));
    }
    Ok(value as i64)
}
pub fn signed(value: i64) -> Result<i64> {
    if value.unsigned_abs() > MAX_EXACT {
        return Err(Error::Database(
            "integer exceeds portable storage range".into(),
        ));
    }
    Ok(value)
}
pub fn validate_json(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(v) = n.as_u64() {
                unsigned(v)?;
            } else if let Some(v) = n.as_i64() {
                signed(v)?;
            } else if n.as_f64().is_some_and(|v| v.abs() > MAX_EXACT as f64) {
                return Err(Error::Database(
                    "number exceeds portable storage range".into(),
                ));
            }
        }
        serde_json::Value::Array(a) => {
            for value in a {
                validate_json(value)?;
            }
        }
        serde_json::Value::Object(o) => {
            for value in o.values() {
                validate_json(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn validate<T: serde::Serialize>(value: &T) -> Result<()> {
    validate_json(
        &serde_json::to_value(value).map_err(|_| Error::Corrupt("invalid storage value".into()))?,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_include_negative_and_nested_json() {
        assert_eq!(unsigned(MAX_EXACT).unwrap(), MAX_EXACT as i64);
        assert!(unsigned(MAX_EXACT + 1).is_err());
        assert!(signed(i64::MIN).is_err());
        assert!(validate_json(&serde_json::json!({"nested":[MAX_EXACT+1]})).is_err());
        assert!(validate_json(&serde_json::json!({"nested":[-(MAX_EXACT as i64)]})).is_ok());
    }
}
