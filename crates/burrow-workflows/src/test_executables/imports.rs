//! The `use` items in scope, one frame per module or block, each resolved
//! in the frame that declared it: an inner alias never changes what an
//! outer `use` means.
use std::collections::{BTreeMap, BTreeSet};

/// Where a frame's imports were declared: `self::` and `super::` count
/// modules, not blocks.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    Module,
    Block,
}

struct Frame {
    kind: Kind,
    /// What each name brought in stands for, as written.
    names: BTreeMap<String, Vec<String>>,
    /// The paths a `use ...::*` brings everything in from, as written.
    globs: Vec<Vec<String>>,
}

/// The frames in scope, innermost last. A child module sees its parent's
/// imports, as `use super::*` would give it.
#[derive(Default)]
pub(super) struct Imports(Vec<Frame>);

impl Imports {
    /// Opens a frame with what the `use` items among `items` bring in, kept
    /// as written: a `use` may start from a name another brings in,
    /// declared before or after it.
    pub(super) fn enter<'a>(&mut self, kind: Kind, items: impl Iterator<Item = &'a syn::Item>) {
        let mut frame = Frame {
            kind,
            names: BTreeMap::new(),
            globs: Vec::new(),
        };
        for item in items {
            if let syn::Item::Use(u) = item {
                frame.tree(&u.tree, Vec::new());
            }
        }
        self.0.push(frame);
    }

    pub(super) fn leave(&mut self) {
        self.0.pop();
    }

    /// A path's segments, spelled out through the imports in scope.
    pub(super) fn resolve(&self, path: &syn::Path) -> Vec<String> {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if path.leading_colon.is_some() {
            names
        } else {
            self.within(self.0.len(), names, true, &mut BTreeSet::new())
        }
    }

    /// The frames up to the nearest module's, of the first `seen`.
    fn module(&self, seen: usize) -> usize {
        (0..seen)
            .rev()
            .find(|&i| self.0[i].kind == Kind::Module)
            .map_or(0, |i| i + 1)
    }

    /// `path` with its head replaced by what it stands for among the first
    /// `seen` frames, innermost first. Each import found resolves among
    /// the frames up to its own; one already being resolved doesn't name
    /// itself, so the search goes on outward.
    fn within(
        &self,
        mut seen: usize,
        mut path: Vec<String>,
        globs: bool,
        resolving: &mut BTreeSet<(usize, String)>,
    ) -> Vec<String> {
        while path.len() > 1 {
            seen = match path[0].as_str() {
                // The module's own frame, and its blocks' too: seeing more
                // imports only finds more writers.
                "self" => seen,
                "super" => self.module(self.module(seen).saturating_sub(1)),
                "crate" => seen.min(1),
                _ => break,
            };
            path.remove(0);
        }
        let Some(head) = path.first().cloned() else {
            return path;
        };
        for at in (0..seen).rev() {
            if resolving.contains(&(at, head.clone())) {
                continue;
            }
            let frame = &self.0[at];
            let full = match frame.names.get(&head) {
                Some(full) => {
                    resolving.insert((at, head.clone()));
                    let full = self.within(at + 1, full.clone(), globs, resolving);
                    resolving.remove(&(at, head.clone()));
                    Some(full)
                }
                None if globs => self.glob(at, &head, resolving),
                None => None,
            };
            if let Some(mut full) = full {
                full.extend(path.drain(1..));
                return full;
            }
        }
        path
    }

    /// What a glob in frame `at` brings in under `name`, for the writers
    /// and the installer.
    fn glob(
        &self,
        at: usize,
        name: &str,
        resolving: &mut BTreeSet<(usize, String)>,
    ) -> Option<Vec<String>> {
        let module = match name {
            "write" | "File" => "fs",
            "executable" => "burrow_test",
            _ => return None,
        };
        self.0[at].globs.iter().find_map(|prefix| {
            let mut full = self.within(at + 1, prefix.clone(), false, resolving);
            (full.last().map(String::as_str) == Some(module)).then(|| {
                full.push(name.into());
                full
            })
        })
    }
}

impl Frame {
    fn tree(&mut self, tree: &syn::UseTree, mut prefix: Vec<String>) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.tree(&p.tree, prefix);
            }
            syn::UseTree::Name(n) if n.ident == "self" => {
                if let Some(last) = prefix.last().cloned() {
                    self.names.insert(last, prefix);
                }
            }
            syn::UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                self.names.insert(n.ident.to_string(), prefix);
            }
            syn::UseTree::Rename(r) => {
                if r.ident != "self" {
                    prefix.push(r.ident.to_string());
                }
                self.names.insert(r.rename.to_string(), prefix);
            }
            syn::UseTree::Glob(_) => self.globs.push(prefix),
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.tree(tree, prefix.clone());
                }
            }
        }
    }
}
