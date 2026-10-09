//! The config files of the proxy (`pumboprox.yml`, `permissions.yml`,
//! plugin manifests and configs) are YAML 1.2, read the same way everywhere.

use serde::de::DeserializeOwned;

/// Reads YAML 1.2: only `true` and `false` are booleans (`no`, `off`, `on`
/// stay text), and an error names the line and column.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let options = serde_saphyr::options! { strict_booleans: true, with_snippet: false };
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_1_2_and_positions() {
        let v: serde_json::Value = from_str("a: no\nb: off\nc: true\n").unwrap();
        assert_eq!(v, serde_json::json!({ "a": "no", "b": "off", "c": true }));
        let e = from_str::<serde_json::Value>("a:\n  b: 1\n c: 2\n").unwrap_err();
        assert!(e.contains("line 3, column 2"), "{e}");
    }
}
