//! What a judge job says it decided, in its answers file, held to its form
//! before it becomes the runner's answer: one of the contract's verdict
//! words, and its reasons.
use conary_core::plugin::Judgment;
use serde::Deserialize;

/// The answers file, as the brief tells the job to write it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Said {
    pub verdict: Judgment,
    pub why: String,
}

impl Said {
    /// The job's verdict and reasons, if they read and say something.
    pub fn read(bytes: &[u8]) -> Result<(Judgment, String), String> {
        let said: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("the judge job's answers don't read: {e}"))?;
        if said.why.trim().is_empty() {
            return Err("the judge job gave no reasons for its verdict".into());
        }
        Ok((said.verdict, said.why))
    }
}
