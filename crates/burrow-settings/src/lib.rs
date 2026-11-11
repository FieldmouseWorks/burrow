//! The pipeline's options, each declared once ([`schema::SETTINGS`]): its
//! name, type, allowed values, default, help, the part that reads it, and
//! whether it changes live or needs a deploy. [`schema::json_schema`] is the
//! same catalog as JSON Schema, and [`show::Document`] is what `settings
//! show` prints: each option's value in effect and where it came from,
//! built only from options the schema declares (docs/settings.md).
pub mod schema;
pub mod show;
