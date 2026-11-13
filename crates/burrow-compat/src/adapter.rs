use std::fmt;

/// Why an adapter or migration refused, and the field it blames if it knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterError {
    pub field: Option<String>,
    pub message: String,
}

impl AdapterError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            field: None,
            message: message.into(),
        }
    }

    pub fn field(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: Some(field.into()),
            message: message.into(),
        }
    }

    /// A serde_json error, with the field serde names (`unknown field`,
    /// `missing field`, `duplicate field`) lifted out of its message.
    pub fn json(e: &serde_json::Error) -> Self {
        let message = e.to_string();
        let field = ["unknown field `", "missing field `", "duplicate field `"]
            .iter()
            .find_map(|lead| {
                let rest = message.split(lead).nth(1)?;
                Some(rest.split('`').next()?.to_owned())
            });
        Self { field, message }
    }
}

impl From<serde_json::Error> for AdapterError {
    fn from(e: serde_json::Error) -> Self {
        Self::json(&e)
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
