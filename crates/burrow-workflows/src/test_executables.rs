//! The workspace has one way to install executable fixtures. Follow test
//! modules from Cargo's entry points, including helpers with arbitrary names.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

fn test_cfg(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(l) if l.path.is_ident("all") || l.path.is_ident("any") => l
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|args| args.iter().any(test_cfg)),
        _ => false,
    }
}

fn test_attrs(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("test")
            || (a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| test_cfg(&m)))
    })
}

struct Guard {
    file: PathBuf,
    modules: PathBuf,
    testing: bool,
    findings: BTreeSet<String>,
    /// Each file's stand-ins: what its `executable` calls named.
    installed: BTreeSet<(PathBuf, String)>,
    /// Each file's plain writes, by the name of the path written.
    written: BTreeSet<(PathBuf, String)>,
    /// The current function's locals, by the names of the paths each may hold.
    lets: Lets,
    /// How many conditional or deferred bodies (branches, loops, closures,
    /// macros) the visit is inside, counted from the current function.
    maybe: usize,
    /// What the `use` items in scope bring in.
    uses: Imports,
}

impl Guard {
    fn new(file: PathBuf, testing: bool) -> Self {
        Guard {
            file,
            modules: PathBuf::new(),
            testing,
            findings: BTreeSet::new(),
            installed: BTreeSet::new(),
            written: BTreeSet::new(),
            lets: Lets::default(),
            maybe: 0,
            uses: Imports::default(),
        }
    }

    /// Writing over a stand-in leaves its mode as it was, so a plain write
    /// installs an executable as surely as a mode change does: a test that
    /// writes a script, or writes where its file installed a stand-in, holds
    /// a writer that a concurrent fork can keep open until the exec.
    fn finish(mut self) -> BTreeSet<String> {
        for (file, name) in &self.written {
            if self.installed.contains(&(file.clone(), name.clone())) {
                self.findings.insert(format!(
                    "{}: writes over the stand-in `{name}`: use burrow_test",
                    file.display()
                ));
            }
        }
        self.findings
    }

    fn reject(&mut self, what: &str) {
        if self.testing {
            self.findings
                .insert(format!("{}: {what}: use burrow_test", self.file.display()));
        }
    }

    fn file(&mut self, file: &Path, testing: bool) {
        let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let ast = syn::parse_file(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let old = (self.file.clone(), self.modules.clone(), self.testing);
        self.file = file.into();
        self.modules = file.parent().unwrap().into();
        if !matches!(
            file.file_stem().unwrap().to_str().unwrap(),
            "lib" | "main" | "mod"
        ) {
            self.modules.push(file.file_stem().unwrap());
        }
        self.testing = testing || test_attrs(&ast.attrs);
        self.visit_file(&ast);
        (self.file, self.modules, self.testing) = old;
    }
}

impl<'ast> Visit<'ast> for Guard {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let old = (self.modules.clone(), self.testing);
        self.testing |= test_attrs(&m.attrs);
        if let Some((_, items)) = &m.content {
            self.modules.push(m.ident.to_string());
            let uses = self.uses.clone();
            self.import(items.iter());
            for item in items {
                self.visit_item(item);
            }
            self.uses = uses;
        } else {
            let explicit = m.attrs.iter().find_map(|a| {
                if !a.path().is_ident("path") {
                    return None;
                }
                let syn::Meta::NameValue(v) = &a.meta else {
                    return None;
                };
                let syn::Expr::Lit(v) = &v.value else {
                    return None;
                };
                let syn::Lit::Str(v) = &v.lit else {
                    return None;
                };
                Some(self.file.parent().unwrap().join(v.value()))
            });
            let file = explicit.unwrap_or_else(|| {
                let file = self.modules.join(format!("{}.rs", m.ident));
                if file.exists() {
                    file
                } else {
                    self.modules.join(m.ident.to_string()).join("mod.rs")
                }
            });
            self.file(&file, self.testing);
        }
        (self.modules, self.testing) = old;
    }

    fn visit_file(&mut self, f: &'ast syn::File) {
        let old = self.uses.clone();
        self.import(f.items.iter());
        visit::visit_file(self, f);
        self.uses = old;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let old = self.testing;
        self.testing |= test_attrs(&f.attrs);
        self.function(&f.sig, |g| visit::visit_item_fn(g, f));
        self.testing = old;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.function(&f.sig, |g| visit::visit_impl_item_fn(g, f));
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.function(&f.sig, |g| visit::visit_trait_item_fn(g, f));
    }

    fn visit_block(&mut self, b: &'ast syn::Block) {
        let old = self.uses.clone();
        self.import(b.stmts.iter().filter_map(|s| match s {
            syn::Stmt::Item(i) => Some(i),
            _ => None,
        }));
        self.scope(|g| visit::visit_block(g, b));
        self.uses = old;
    }

    /// A `let ... else` block diverges, so what it assigns may never be seen.
    fn visit_local(&mut self, l: &'ast syn::Local) {
        self.visit_pat(&l.pat);
        if let Some(init) = &l.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.maybe(|g| g.visit_expr(diverge));
            }
        }
        let named = l
            .init
            .as_ref()
            .map(|i| name(&i.expr, &self.lets))
            .unwrap_or_default();
        self.lets.bind(&l.pat, named, self.maybe);
    }

    fn visit_expr_assign(&mut self, e: &'ast syn::ExprAssign) {
        visit::visit_expr_assign(self, e);
        if let syn::Expr::Path(p) = &*e.left
            && let Some(local) = p.path.get_ident()
        {
            let named = name(&e.right, &self.lets);
            self.lets.assign(&local.to_string(), named, self.maybe);
        }
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.scope(|g| {
            g.maybe(|g| {
                for input in &c.inputs {
                    g.lets.bind(input, BTreeSet::new(), g.maybe);
                }
                visit::visit_expr_closure(g, c);
            })
        });
    }

    fn visit_expr_async(&mut self, e: &'ast syn::ExprAsync) {
        self.maybe(|g| visit::visit_expr_async(g, e));
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        self.scope(|g| {
            g.maybe(|g| {
                g.lets.bind(&a.pat, BTreeSet::new(), g.maybe);
                visit::visit_arm(g, a);
            })
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        self.scope(|g| {
            g.maybe(|g| {
                g.lets.bind(&f.pat, BTreeSet::new(), g.maybe);
                g.visit_block(&f.body);
            })
        });
    }

    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.maybe(|g| visit::visit_expr_loop(g, e));
    }

    /// A labeled block can be left before its end.
    fn visit_expr_block(&mut self, e: &'ast syn::ExprBlock) {
        if e.label.is_some() {
            self.maybe(|g| visit::visit_expr_block(g, e));
        } else {
            visit::visit_expr_block(self, e);
        }
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        self.visit_expr(&e.left);
        if matches!(e.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.maybe(|g| g.visit_expr(&e.right));
        } else {
            self.visit_expr(&e.right);
        }
    }

    /// `if let` and `while let` bind for the rest of their condition and body.
    fn visit_expr_let(&mut self, e: &'ast syn::ExprLet) {
        visit::visit_expr_let(self, e);
        self.lets.bind(&e.pat, BTreeSet::new(), self.maybe);
    }

    /// The condition's bindings reach its body but not its `else`.
    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.maybe(|g| {
            g.scope(|g| {
                g.visit_expr(&e.cond);
                g.visit_block(&e.then_branch);
            });
            if let Some((_, otherwise)) = &e.else_branch {
                g.visit_expr(otherwise);
            }
        });
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.scope(|g| g.maybe(|g| visit::visit_expr_while(g, e)));
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let names: Vec<_> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if names.ends_with(&["fs".into(), "copy".into()]) {
            self.reject("fs::copy");
        }
        visit::visit_path(self, p);
    }

    fn visit_path_segment(&mut self, p: &'ast syn::PathSegment) {
        if matches!(
            p.ident.to_string().as_str(),
            "set_permissions" | "from_mode" | "set_mode" | "fchmod" | "chmod"
        ) {
            self.reject(&p.ident.to_string());
        }
        visit::visit_path_segment(self, p);
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if e.method == "write_all" && e.args.first().is_some_and(script) {
            self.reject("writes a script in place");
        }
        if e.method == "set_mode"
            || e.method == "set_permissions"
            || (e.method == "mode" && !e.args.is_empty())
        {
            self.reject(&e.method.to_string());
        }
        if e.method == "arg" || e.method == "args" {
            let mut shell = Shell(false);
            for arg in &e.args {
                shell.visit_expr(arg);
            }
            if shell.0 {
                self.reject("shell changes executable permissions");
            }
        }
        visit::visit_expr_method_call(self, e);
    }

    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*e.func {
            let names = self.resolve(&p.path);
            let writer = names.ends_with(&["fs".into(), "write".into()])
                || names.ends_with(&["File".into(), "create".into()]);
            if self.testing {
                let paths = e
                    .args
                    .first()
                    .map(|p| name(p, &self.lets))
                    .unwrap_or_default();
                for path in paths {
                    if names.last().is_some_and(|n| n == "executable") {
                        self.installed.insert((self.file.clone(), path));
                    } else if writer {
                        self.written.insert((self.file.clone(), path));
                    }
                }
            }
            if writer && e.args.iter().nth(1).is_some_and(script) {
                self.reject("writes a script in place");
            }
            if names.ends_with(&["Command".into(), "new".into()]) {
                for arg in &e.args {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = arg
                        && matches!(s.value().as_str(), "chmod" | "install")
                    {
                        self.reject("executable command");
                    }
                }
            }
        }
        visit::visit_expr_call(self, e);
    }

    /// A macro's arguments are tokens to syn: where they parse as Rust,
    /// every check sees them as if written outside it. A macro may not run
    /// them all (`debug_assert!`), so what they assign may never be seen.
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.tokens(
            m.tokens.clone(),
            m.path.is_ident("format") || m.path.is_ident("concat"),
        );
        self.scope(|g| {
            g.maybe(|g| {
                if let Ok(args) = m.parse_body_with(
                    syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
                ) {
                    let mut formatting = false;
                    for arg in &args {
                        match arg {
                            // After a format string, `name = value` names an
                            // argument and assigns nothing.
                            syn::Expr::Assign(a) if formatting && local(&a.left) => {
                                g.visit_expr(&a.right)
                            }
                            _ => g.visit_expr(arg),
                        }
                        formatting |= matches!(
                            arg,
                            syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(_),
                                ..
                            })
                        );
                    }
                } else if let Ok(stmts) = m.parse_body_with(syn::Block::parse_within) {
                    let uses = g.uses.clone();
                    g.import(stmts.iter().filter_map(|s| match s {
                        syn::Stmt::Item(i) => Some(i),
                        _ => None,
                    }));
                    for stmt in &stmts {
                        g.visit_stmt(stmt);
                    }
                    g.uses = uses;
                }
            })
        });
        visit::visit_macro(self, m);
    }

    fn visit_use_tree(&mut self, t: &'ast syn::UseTree) {
        if let syn::UseTree::Name(n) = t {
            self.reject_setter(&n.ident.to_string());
        }
        if let syn::UseTree::Rename(n) = t {
            self.reject_setter(&n.ident.to_string());
        }
        visit::visit_use_tree(self, t);
    }
}

impl Guard {
    fn tokens(&mut self, tokens: proc_macro2::TokenStream, shell: bool) {
        if tokens.to_string().contains("fs :: copy") {
            self.reject("fs::copy");
        }
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(g) => self.tokens(g.stream(), shell),
                proc_macro2::TokenTree::Ident(i) => {
                    if matches!(
                        i.to_string().as_str(),
                        "set_permissions" | "from_mode" | "set_mode"
                    ) {
                        self.reject(&i.to_string());
                    }
                }
                proc_macro2::TokenTree::Literal(l) if shell => {
                    if let Ok(s) = syn::parse_str::<syn::LitStr>(&l.to_string())
                        && shell_setter(&s.value())
                    {
                        self.reject("shell changes executable permissions");
                    }
                }
                _ => {}
            }
        }
    }

    /// Runs `visit` in a scope of its own: the locals it binds end with it.
    fn scope(&mut self, visit: impl FnOnce(&mut Self)) {
        self.lets.0.push(BTreeMap::new());
        visit(self);
        self.lets.0.pop();
    }

    /// Runs `visit` as code that may not run, or may run later: what it
    /// assigns to an outer local adds to what that local may hold.
    fn maybe(&mut self, visit: impl FnOnce(&mut Self)) {
        self.maybe += 1;
        visit(self);
        self.maybe -= 1;
    }

    /// A function sees none of its caller's locals, and its parameters name
    /// nothing.
    fn function(&mut self, sig: &syn::Signature, visit: impl FnOnce(&mut Self)) {
        let old = (std::mem::take(&mut self.lets), self.maybe);
        self.maybe = 0;
        self.scope(|g| {
            for input in &sig.inputs {
                if let syn::FnArg::Typed(t) = input {
                    g.lets.bind(&t.pat, BTreeSet::new(), 0);
                }
            }
            visit(g);
        });
        (self.lets, self.maybe) = old;
    }

    /// Records what the `use` items among `items` bring into scope. A child
    /// module keeps its parent's, as `use super::*` would give it.
    fn import<'a>(&mut self, items: impl Iterator<Item = &'a syn::Item>) {
        for item in items {
            if let syn::Item::Use(u) = item {
                self.import_tree(&u.tree, Vec::new());
            }
        }
    }

    fn import_tree(&mut self, tree: &syn::UseTree, mut prefix: Vec<String>) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.import_tree(&p.tree, prefix);
            }
            syn::UseTree::Name(n) if n.ident == "self" => {
                if let Some(last) = prefix.last().cloned() {
                    self.uses.names.insert(last, prefix);
                }
            }
            syn::UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                self.uses.names.insert(n.ident.to_string(), prefix);
            }
            syn::UseTree::Rename(r) => {
                if r.ident != "self" {
                    prefix.push(r.ident.to_string());
                }
                self.uses.names.insert(r.rename.to_string(), prefix);
            }
            syn::UseTree::Glob(_) => self.uses.globs.push(prefix),
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.import_tree(tree, prefix.clone());
                }
            }
        }
    }

    /// A path's segments, spelled out through the imports in scope.
    fn resolve(&self, path: &syn::Path) -> Vec<String> {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if path.leading_colon.is_some() {
            names
        } else {
            self.uses.resolve(names, true)
        }
    }

    fn reject_setter(&mut self, name: &str) {
        if matches!(name, "set_permissions" | "chmod" | "fchmod" | "copy") {
            self.reject(name);
        }
    }
}

/// The `use` items in scope, kept as written: a `use` may start from a name
/// another brings in, declared before or after it.
#[derive(Clone, Default)]
struct Imports {
    /// What each name brought in stands for.
    names: BTreeMap<String, Vec<String>>,
    /// The paths a `use ...::*` brings everything in from.
    globs: Vec<Vec<String>>,
}

impl Imports {
    /// `path` with its head replaced by what it stands for, until the head
    /// is nothing brought in. `self::`, `super::` and `crate::` reach the
    /// imports in scope, since a module keeps its parent's.
    fn resolve(&self, mut path: Vec<String>, globs: bool) -> Vec<String> {
        let mut seen = BTreeSet::new();
        loop {
            while path.len() > 1 && matches!(path[0].as_str(), "self" | "super" | "crate") {
                path.remove(0);
            }
            let Some(head) = path.first().cloned() else {
                return path;
            };
            if !seen.insert(head.clone()) {
                return path;
            }
            let full = match self.names.get(&head) {
                Some(full) => full.clone(),
                None if globs => match self.glob(&head) {
                    Some(full) => full,
                    None => return path,
                },
                None => return path,
            };
            path.splice(0..1, full);
        }
    }

    /// What a glob brings in under `name`, for the writers and the installer.
    fn glob(&self, name: &str) -> Option<Vec<String>> {
        let module = match name {
            "write" | "File" => "fs",
            "executable" => "burrow_test",
            _ => return None,
        };
        self.globs.iter().find_map(|prefix| {
            let mut full = self.resolve(prefix.clone(), false);
            (full.last().map(String::as_str) == Some(module)).then(|| {
                full.push(name.into());
                full
            })
        })
    }
}

/// What a local holds, as far as the guard can tell: the names of the paths
/// it may hold (none known for anything else, which hides any outer local of
/// its name), and how many conditional bodies deep it was bound.
struct Held {
    names: BTreeSet<String>,
    maybe: usize,
}

/// The locals in scope, innermost scope last.
#[derive(Default)]
struct Lets(Vec<BTreeMap<String, Held>>);

impl Lets {
    fn get(&self, local: &str) -> BTreeSet<String> {
        self.0
            .iter()
            .rev()
            .find_map(|s| s.get(local))
            .map(|h| h.names.clone())
            .unwrap_or_default()
    }

    /// A pattern binds a single local to `named`; any other binds each of
    /// its locals to nothing known.
    fn bind(&mut self, pat: &syn::Pat, named: BTreeSet<String>, maybe: usize) {
        let mut locals = Vec::new();
        bound(pat, &mut locals);
        let single = match pat {
            syn::Pat::Type(t) => matches!(&*t.pat, syn::Pat::Ident(i) if i.subpat.is_none()),
            syn::Pat::Ident(i) => i.subpat.is_none(),
            _ => false,
        };
        if self.0.is_empty() {
            self.0.push(BTreeMap::new());
        }
        let scope = self.0.last_mut().unwrap();
        for local in locals {
            let names = if single {
                named.clone()
            } else {
                BTreeSet::new()
            };
            scope.insert(local, Held { names, maybe });
        }
    }

    /// An assignment changes the innermost local of its name, wherever it
    /// was bound. Where it may not run, the local may still hold what it
    /// did, so the new names add to the old.
    fn assign(&mut self, local: &str, named: BTreeSet<String>, maybe: usize) {
        if let Some(held) = self.0.iter_mut().rev().find_map(|s| s.get_mut(local)) {
            if held.maybe == maybe {
                held.names = named;
            } else {
                held.names.extend(named);
            }
        }
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
fn local(e: &syn::Expr) -> bool {
    matches!(e, syn::Expr::Path(p) if p.path.get_ident().is_some())
}

/// The names an expression gives the path it builds: the last file name a
/// `join` or literal spells, else the field that holds it, or what the local
/// holding it may hold, seen through borrows and conversions. None when it's
/// computed some other way or comes from a parameter: a helper's `path`
/// names nothing.
fn name(e: &syn::Expr, lets: &Lets) -> BTreeSet<String> {
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
            .map(String::from)
            .into_iter()
            .collect(),
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(n) => BTreeSet::from([n.to_string()]),
            syn::Member::Unnamed(_) => BTreeSet::new(),
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
                _ => BTreeSet::new(),
            }
        }
        _ => BTreeSet::new(),
    }
}

/// Whether an expression is a script's text: a literal, or a `format!` or
/// `concat!`, starting with a shebang.
fn script(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Reference(r) => script(&r.expr),
        syn::Expr::Paren(p) => script(&p.expr),
        syn::Expr::MethodCall(m)
            if matches!(m.method.to_string().as_str(), "as_bytes" | "as_str") =>
        {
            script(&m.receiver)
        }
        syn::Expr::Lit(syn::ExprLit { lit, .. }) => match lit {
            syn::Lit::Str(s) => s.value().starts_with("#!"),
            syn::Lit::ByteStr(s) => s.value().starts_with(b"#!"),
            _ => false,
        },
        syn::Expr::Macro(m) if m.mac.path.is_ident("format") || m.mac.path.is_ident("concat") => m
            .mac
            .parse_body_with(
                syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|args| args.first().is_some_and(script)),
        _ => false,
    }
}

fn shell_setter(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || ";|&".contains(c))
        .any(|word| word == "chmod" || word == "/bin/chmod")
}

struct Shell(bool);
impl<'ast> Visit<'ast> for Shell {
    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        self.0 |= shell_setter(&s.value());
    }
}

fn check(root: &Path) -> BTreeSet<String> {
    let mut guard = Guard::new(PathBuf::new(), false);
    for entry in fs::read_dir(root).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.join("Cargo.toml").is_file() {
            continue;
        }
        for name in ["src/lib.rs", "src/main.rs"] {
            let file = dir.join(name);
            if file.exists() {
                guard.file(&file, false);
            }
        }
        for (subdir, testing) in [("tests", true), ("src/bin", false)] {
            let path = dir.join(subdir);
            if !path.exists() {
                continue;
            }
            for e in fs::read_dir(path).unwrap() {
                let file = e.unwrap().path();
                if file.extension().is_some_and(|e| e == "rs") {
                    // Cargo entry points resolve sibling modules from their directory.
                    let text = fs::read_to_string(&file).unwrap();
                    let ast = syn::parse_file(&text).unwrap();
                    guard.file = file.clone();
                    guard.modules = file.parent().unwrap().into();
                    guard.testing = testing || test_attrs(&ast.attrs);
                    guard.visit_file(&ast);
                } else if file.is_dir() && file.join("main.rs").exists() {
                    guard.file(&file.join("main.rs"), testing);
                }
            }
        }
    }
    guard.finish()
}

#[cfg(test)]
mod tests;
