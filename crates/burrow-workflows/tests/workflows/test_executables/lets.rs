//! What each local may hold, as far as the guard can tell: the names of
//! the paths it may hold.
use std::collections::{BTreeMap, BTreeSet};

/// A name a path may have: a file name a literal spells, or everything a
/// local binding ever holds, which code that may run later (a closure, an
/// async block) sees, since the binding may change before it runs.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Name {
    File(String),
    Ever(usize),
}

pub(super) type Names = BTreeSet<Name>;
pub(super) type Scopes = Vec<BTreeMap<String, Held>>;

/// What a local holds (none known for anything else, which hides any outer
/// local of its name), how many conditional bodies and deferred bodies deep
/// it was bound, and which binding it is.
#[derive(Clone, PartialEq)]
pub(super) struct Held {
    names: Names,
    maybe: usize,
    deferred: usize,
    binding: usize,
}

/// The locals in scope, innermost scope last, and everything each binding
/// ever held.
#[derive(Default)]
pub(super) struct Lets {
    scopes: Scopes,
    /// Everything each binding was bound or assigned, by its number.
    ever: Vec<Names>,
    /// The next binding's number. The same binding site gets the same
    /// number each time a loop's body is seen again.
    next: usize,
    /// How many closures or async blocks the visit is inside.
    pub(super) deferred: usize,
}

impl Lets {
    pub(super) fn get(&self, local: &str) -> Names {
        let Some(held) = self.scopes.iter().rev().find_map(|s| s.get(local)) else {
            return Names::new();
        };
        let mut names = held.names.clone();
        if self.deferred > held.deferred {
            names.insert(Name::Ever(held.binding));
        }
        names
    }

    pub(super) fn push(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    pub(super) fn pop(&mut self) {
        self.scopes.pop();
    }

    /// A function sees none of its caller's locals.
    pub(super) fn take_scopes(&mut self) -> Scopes {
        std::mem::take(&mut self.scopes)
    }

    pub(super) fn restore_scopes(&mut self, scopes: Scopes) {
        self.scopes = scopes;
    }

    /// The number a loop body's first binding gets.
    pub(super) fn first(&self) -> usize {
        self.next
    }

    /// Back to a loop body's first binding number, to see it again: the
    /// same binding site keeps its number. Returns the locals in scope
    /// before this run.
    pub(super) fn rewind(&mut self, first: usize) -> Scopes {
        self.next = first;
        self.scopes.clone()
    }

    pub(super) fn changed(&self, before: &Scopes) -> bool {
        self.scopes != *before
    }

    /// A pattern binds a single local to `named`; any other binds each of
    /// its locals to nothing known.
    pub(super) fn bind(&mut self, pat: &syn::Pat, named: Names, maybe: usize) {
        let mut locals = Vec::new();
        bound(pat, &mut locals);
        let single = match pat {
            syn::Pat::Type(t) => matches!(&*t.pat, syn::Pat::Ident(i) if i.subpat.is_none()),
            syn::Pat::Ident(i) => i.subpat.is_none(),
            _ => false,
        };
        if self.scopes.is_empty() {
            self.push();
        }
        for local in locals {
            let names = if single { named.clone() } else { Names::new() };
            let binding = self.next;
            self.next += 1;
            match self.ever.get_mut(binding) {
                Some(ever) => ever.extend(names.iter().cloned()),
                None => self.ever.push(names.clone()),
            }
            let held = Held {
                names,
                maybe,
                deferred: self.deferred,
                binding,
            };
            self.scopes.last_mut().unwrap().insert(local, held);
        }
    }

    /// An assignment changes the innermost local of its name, wherever it
    /// was bound. Where it may not run, the local may still hold what it
    /// did, so the new names add to the old.
    pub(super) fn assign(&mut self, local: &str, named: Names, maybe: usize) {
        if let Some(held) = self.scopes.iter_mut().rev().find_map(|s| s.get_mut(local)) {
            self.ever[held.binding].extend(named.iter().cloned());
            if held.maybe == maybe {
                held.names = named;
            } else {
                held.names.extend(named);
            }
        }
    }

    /// The file names `name` may stand for.
    pub(super) fn files(&self, name: &Name) -> BTreeSet<String> {
        let mut files = BTreeSet::new();
        let mut todo = vec![name.clone()];
        let mut seen = BTreeSet::new();
        while let Some(name) = todo.pop() {
            match name {
                Name::File(f) => {
                    files.insert(f);
                }
                Name::Ever(b) if seen.insert(b) => todo.extend(self.ever[b].iter().cloned()),
                Name::Ever(_) => {}
            }
        }
        files
    }
}

/// The locals a pattern binds.
fn bound(pat: &syn::Pat, locals: &mut Vec<String>) {
    match pat {
        syn::Pat::Ident(i) => {
            locals.push(i.ident.to_string());
            if let Some((_, sub)) = &i.subpat {
                bound(sub, locals);
            }
        }
        syn::Pat::Type(t) => bound(&t.pat, locals),
        syn::Pat::Reference(r) => bound(&r.pat, locals),
        syn::Pat::Paren(p) => bound(&p.pat, locals),
        syn::Pat::Tuple(t) => t.elems.iter().for_each(|p| bound(p, locals)),
        syn::Pat::TupleStruct(t) => t.elems.iter().for_each(|p| bound(p, locals)),
        syn::Pat::Slice(s) => s.elems.iter().for_each(|p| bound(p, locals)),
        syn::Pat::Or(o) => o.cases.iter().for_each(|p| bound(p, locals)),
        syn::Pat::Struct(s) => s.fields.iter().for_each(|f| bound(&f.pat, locals)),
        _ => {}
    }
}

/// Whether an expression is a bare local's name.
pub(super) fn local(e: &syn::Expr) -> bool {
    matches!(e, syn::Expr::Path(p) if p.path.get_ident().is_some())
}

/// The names an expression gives the path it builds: the last file name a
/// `join` or literal spells, else the field that holds it, or what the local
/// holding it may hold, seen through borrows and conversions. None when it's
/// computed some other way or comes from a parameter: a helper's `path`
/// names nothing.
pub(super) fn name(e: &syn::Expr, lets: &Lets) -> Names {
    match e {
        syn::Expr::Reference(r) => name(&r.expr, lets),
        syn::Expr::Paren(p) => name(&p.expr, lets),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => s
            .value()
            .rsplit('/')
            .next()
            .map(|f| Name::File(f.into()))
            .into_iter()
            .collect(),
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(n) => Names::from([Name::File(n.to_string())]),
            syn::Member::Unnamed(_) => Names::new(),
        },
        syn::Expr::Path(p) => p
            .path
            .get_ident()
            .map(|i| lets.get(&i.to_string()))
            .unwrap_or_default(),
        syn::Expr::MethodCall(m) if m.method == "join" => {
            m.args.first().map(|a| name(a, lets)).unwrap_or_default()
        }
        syn::Expr::MethodCall(m)
            if matches!(
                m.method.to_string().as_str(),
                "as_ref"
                    | "as_path"
                    | "as_deref"
                    | "borrow"
                    | "clone"
                    | "expect"
                    | "into"
                    | "to_owned"
                    | "to_path_buf"
                    | "unwrap"
            ) =>
        {
            name(&m.receiver, lets)
        }
        syn::Expr::Call(c) => {
            match &*c.func {
                syn::Expr::Path(p)
                    if p.path.segments.last().is_some_and(|s| {
                        matches!(s.ident.to_string().as_str(), "new" | "from")
                    }) && c.args.len() == 1 =>
                {
                    c.args.first().map(|a| name(a, lets)).unwrap_or_default()
                }
                _ => Names::new(),
            }
        }
        _ => Names::new(),
    }
}
