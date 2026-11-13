//! Configured prices: the host's assumption, never a fact of the stream, so
//! every estimate names the price key it used and reports print the prices.
//! Generic code carries no prices or model names; the host sets them.
use super::{Meta, Tokens};
use serde::Deserialize;
use std::collections::BTreeMap;

/// USD per million tokens of each kind.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Keyed `ENGINE/MODEL`, or `ENGINE` for any of its models: the more
/// specific key wins.
pub type Prices = BTreeMap<String, Price>;

/// Money a turn would cost at a configured price.
#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub usd: f64,
    /// The price key it used.
    pub basis: String,
}

pub(crate) fn estimate(prices: &Prices, meta: &Meta, tokens: &Tokens) -> Option<Estimate> {
    let specific = meta
        .model
        .as_ref()
        .map(|model| format!("{}/{model}", meta.engine));
    let (basis, price) = specific
        .and_then(|key| prices.get_key_value(&key))
        .or_else(|| prices.get_key_value(&meta.engine))?;
    let rates = [
        price.input,
        price.output,
        price.cache_read,
        price.cache_write,
    ];
    let usd = tokens
        .iter()
        .zip(rates)
        .map(|(n, rate)| *n as f64 * rate / 1e6)
        .sum();
    Some(Estimate {
        usd,
        basis: basis.clone(),
    })
}

/// The prices as a report shows them, so every estimate's assumption is
/// in view.
pub fn describe(prices: &Prices) -> String {
    if prices.is_empty() {
        return "no prices configured: cost an engine's stream doesn't carry is unavailable".into();
    }
    let each: Vec<String> = prices
        .iter()
        .map(|(key, p)| {
            format!(
                "{key} {}/{}/{}/{}",
                p.input, p.output, p.cache_read, p.cache_write
            )
        })
        .collect();
    format!(
        "configured prices (USD per million input/output/cache-read/cache-write tokens), used only where a stream carries no cost: {}",
        each.join("; ")
    )
}
