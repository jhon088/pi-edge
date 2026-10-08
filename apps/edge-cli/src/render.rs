//! Output rendering. The CLI prints exactly what the core returned through
//! `edge-protocol`; it never computes or reformats a business value.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Flat `path = value` lines, one per field.
    Text,
    /// Pretty-printed protocol DTO.
    Json,
}

pub fn render(value: &Value, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        OutputFormat::Text => {
            let mut out = String::new();
            flatten("", value, &mut out);
            if out.is_empty() {
                out.push_str("(empty)\n");
            }
            out
        }
    }
}

fn flatten(prefix: &str, value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, child, out);
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str(&format!("{prefix} = []\n"));
            }
            for (index, child) in items.iter().enumerate() {
                flatten(&format!("{prefix}[{index}]"), child, out);
            }
        }
        Value::Null => out.push_str(&format!("{prefix} = -\n")),
        Value::String(text) => out.push_str(&format!("{prefix} = {text}\n")),
        other => out.push_str(&format!("{prefix} = {other}\n")),
    }
}
