use crate::AdapterError;

/// The version a JSON record declares in `key`, or `None` when it declares
/// none: a record written before the format was versioned. A registered
/// format adopts those by giving its first version the legacy shape, so
/// they read as that version, and adds the key only once every deployed
/// reader tolerates it (docs/compatibility.md, "Adopting unversioned
/// records"). `Format::check` proves that order: a writer that stamps the
/// key fails the backward check against a strict legacy reader, naming it.
pub fn declared_version(bytes: &[u8], key: &str) -> Result<Option<u32>, AdapterError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let Some(object) = value.as_object() else {
        return Err(AdapterError::new("record is not a JSON object"));
    };
    match object.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| AdapterError::field(key, format!("`{key}` is not a version number"))),
    }
}
