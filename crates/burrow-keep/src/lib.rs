//! burrow-keep: picks what a run built for upload to the machine's build
//! store (`burrow.builds`, crates/burrow-builds; docs/gate-design.md, "One
//! build store, written by whoever may sign").
//!
//! On a hosted runner whose store started empty, every path no one signed
//! (other than derivations, which each evaluation makes again) was built
//! here. Downloads (flat fixed-output paths, as fetchurl makes) are left
//! out unless a built path refers to them, near or far.

use std::collections::{BTreeMap, BTreeSet};

pub mod nix;
pub mod upload;

/// The local store's paths, by full name (`/nix/store/…`).
#[derive(Debug, Default)]
pub struct Store {
    /// Where its paths are: `/nix/store`.
    pub dir: String,
    pub paths: BTreeMap<String, Info>,
}

/// What nix has of a path: whether anyone signed it, whether it's a
/// download (flat and fixed-output), the paths it refers to, by full name,
/// and its NAR's hash (`sha256-…`) and size.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub signed: bool,
    pub download: bool,
    pub references: Vec<String>,
    pub nar_hash: String,
    pub nar_size: u64,
}

/// The paths of the store no one signed, but derivations: what this run
/// built on a store that started empty, as a hosted runner's does.
fn own(store: &Store) -> BTreeSet<&String> {
    store
        .paths
        .iter()
        .filter(|(p, info)| !info.signed && !p.ends_with(".drv"))
        .map(|(p, _)| p)
        .collect()
}

/// Of `could`, what's worth uploading: every path but a download, and a
/// download only when one of those refers to it, near or far. `known` says
/// of each whether it's a download, and what it refers to.
fn worth<'a>(
    could: &BTreeSet<&'a String>,
    known: impl Fn(&'a String) -> (bool, &'a [String]),
) -> BTreeSet<&'a String> {
    let mut wanted: BTreeSet<&String> = BTreeSet::new();
    let mut todo: Vec<&String> = could.iter().copied().filter(|p| !known(p).0).collect();
    while let Some(p) = todo.pop() {
        if wanted.insert(p) {
            todo.extend(known(p).1.iter().filter(|r| could.contains(r)));
        }
    }
    wanted
}

/// What a run uploads to the build store (crates/burrow-builds): every path
/// no one signed, but derivations, and a download only when one of those
/// refers to it.
pub fn picked(store: &Store) -> Vec<String> {
    let mine = own(store);
    worth(&mine, |p| {
        let i = &store.paths[p];
        (i.download, &i.references)
    })
    .into_iter()
    .cloned()
    .collect()
}

#[cfg(test)]
mod tests;
