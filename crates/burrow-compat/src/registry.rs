use crate::Failure;
use std::collections::BTreeSet;

/// What a registry holds: a format of any value type, checked alone.
pub trait Checked {
    fn id(&self) -> &str;
    fn check(&self) -> Vec<Failure>;
}

/// Every registered format, by unique id.
#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn Checked>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a format. An id already taken is refused: ids are the stable name
    /// a record is known by, so two formats never share one.
    pub fn register(&mut self, format: impl Checked + 'static) -> Result<(), String> {
        if self.formats.iter().any(|f| f.id() == format.id()) {
            return Err(format!("format `{}` is registered twice", format.id()));
        }
        self.formats.push(Box::new(format));
        Ok(())
    }

    pub fn ids(&self) -> BTreeSet<&str> {
        self.formats.iter().map(|f| f.id()).collect()
    }

    /// Every failure of every format.
    pub fn check(&self) -> Vec<Failure> {
        self.formats.iter().flat_map(|f| f.check()).collect()
    }
}
