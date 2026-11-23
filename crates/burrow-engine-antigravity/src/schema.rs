//! The `--json-schema` a structured invocation asks for, compiled once and
//! held to: output that parses as JSON but breaks the schema is refused, not
//! reported as a structured success.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// A compiled `--json-schema`.
pub struct Schema {
    schemas: boon::Schemas,
    index: boon::SchemaIndex,
}

impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Schema")
    }
}

impl Schema {
    /// Compile `spec`, given as `--json-schema` takes it: inline JSON, or the
    /// path of a file holding it.
    pub fn compile(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim();
        let schema: Value = match serde_json::from_str(trimmed) {
            Ok(inline) => inline,
            Err(inline_err) => {
                let path = Path::new(trimmed);
                if !path.is_file() {
                    return Err(format!(
                        "json_schema is neither inline JSON ({inline_err}) nor a readable file"
                    ));
                }
                let text = fs::read_to_string(path)
                    .map_err(|e| format!("cannot read json_schema {}: {e}", path.display()))?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("json_schema file {} is not JSON: {e}", path.display()))?
            }
        };
        let mut schemas = boon::Schemas::new();
        let mut compiler = boon::Compiler::new();
        compiler
            .add_resource("urn:antigravity-json-schema", schema)
            .map_err(|e| format!("json_schema is not a usable schema: {e}"))?;
        let index = compiler
            .compile("urn:antigravity-json-schema", &mut schemas)
            .map_err(|e| format!("json_schema is not a usable schema: {e}"))?;
        Ok(Self { schemas, index })
    }

    /// Whether `value` follows the schema; why not when it doesn't.
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        self.schemas
            .validate(value, self.index)
            .map_err(|e| format!("{e:#}"))
    }
}
