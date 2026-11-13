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
    /// The current function's locals, by the name of the path each holds.
    lets: Lets,
    /// What each name a `use` brought into scope stands for.
    uses: BTreeMap<String, Vec<String>>,
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
            uses: BTreeMap::new(),
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

    fn visit_local(&mut self, l: &'ast syn::Local) {
        visit::visit_local(self, l);
        let named = l.init.as_ref().and_then(|i| name(&i.expr, &self.lets));
        self.lets.bind(&l.pat, named);
    }

    fn visit_expr_assign(&mut self, e: &'ast syn::ExprAssign) {
        visit::visit_expr_assign(self, e);
        if let syn::Expr::Path(p) = &*e.left
            && let Some(local) = p.path.get_ident()
        {
            let named = name(&e.right, &self.lets);
            self.lets.assign(&local.to_string(), named);
        }
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.scope(|g| {
            for input in &c.inputs {
                g.lets.bind(input, None);
            }
            visit::visit_expr_closure(g, c);
        });
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        self.scope(|g| {
            g.lets.bind(&a.pat, None);
            visit::visit_arm(g, a);
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        self.scope(|g| {
            g.lets.bind(&f.pat, None);
            g.visit_block(&f.body);
        });
    }

    /// `if let` and `while let` bind for the rest of their condition and body.
    fn visit_expr_let(&mut self, e: &'ast syn::ExprLet) {
        visit::visit_expr_let(self, e);
        self.lets.bind(&e.pat, None);
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.scope(|g| visit::visit_expr_if(g, e));
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.scope(|g| visit::visit_expr_while(g, e));
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
            if self.testing
                && let Some(path) = e.args.first().and_then(|p| name(p, &self.lets))
            {
                if names.last().is_some_and(|n| n == "executable") {
                    self.installed.insert((self.file.clone(), path));
                } else if writer {
                    self.written.insert((self.file.clone(), path));
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
    /// every check sees them as if written outside it.
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.tokens(
            m.tokens.clone(),
            m.path.is_ident("format") || m.path.is_ident("concat"),
        );
        self.scope(|g| {
            if let Ok(args) = m.parse_body_with(
                syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
            ) {
                for arg in &args {
                    match arg {
                        // A named format argument, `name = value`, assigns nothing.
                        syn::Expr::Assign(a) => g.visit_expr(&a.right),
                        _ => g.visit_expr(arg),
                    }
                }
            } else if let Ok(stmts) = m.parse_body_with(syn::Block::parse_within) {
                for stmt in &stmts {
                    g.visit_stmt(stmt);
                }
            }
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

    /// A function sees none of its caller's locals, and its parameters name
    /// nothing.
    fn function(&mut self, sig: &syn::Signature, visit: impl FnOnce(&mut Self)) {
        let old = std::mem::take(&mut self.lets);
        self.scope(|g| {
            for input in &sig.inputs {
                if let syn::FnArg::Typed(t) = input {
                    g.lets.bind(&t.pat, None);
                }
            }
            visit(g);
        });
        self.lets = old;
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
        // A `use` may start from a name an earlier one brought in.
        let mut alias = |name: String, path: Vec<String>| {
            let path = match path.first().and_then(|head| self.uses.get(head)) {
                Some(full) => full.iter().chain(&path[1..]).cloned().collect(),
                None => path,
            };
            self.uses.insert(name, path);
        };
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.import_tree(&p.tree, prefix);
            }
            syn::UseTree::Name(n) if n.ident == "self" => {
                if let Some(last) = prefix.last().cloned() {
                    alias(last, prefix);
                }
            }
            syn::UseTree::Name(n) => {
                prefix.push(n.ident.to_string());
                alias(n.ident.to_string(), prefix);
            }
            syn::UseTree::Rename(r) => {
                if r.ident != "self" {
                    prefix.push(r.ident.to_string());
                }
                alias(r.rename.to_string(), prefix);
            }
            syn::UseTree::Glob(_) => {
                let names: &[&str] = match prefix.last().map(String::as_str) {
                    Some("fs") => &["write", "File"],
                    Some("burrow_test") => &["executable"],
                    _ => &[],
                };
                for name in names {
                    let mut path = prefix.clone();
                    path.push(name.to_string());
                    alias(name.to_string(), path);
                }
            }
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.import_tree(tree, prefix.clone());
                }
            }
        }
    }

    /// A path's segments, with a head that a `use` brought in spelled out.
    fn resolve(&self, path: &syn::Path) -> Vec<String> {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        match names.first().and_then(|head| self.uses.get(head)) {
            Some(full) if path.leading_colon.is_none() => {
                full.iter().chain(&names[1..]).cloned().collect()
            }
            _ => names,
        }
    }

    fn reject_setter(&mut self, name: &str) {
        if matches!(name, "set_permissions" | "chmod" | "fchmod" | "copy") {
            self.reject(name);
        }
    }
}

/// The locals in scope, innermost scope last: the name of the path each
/// holds, or `None` for one that holds something else, which hides any outer
/// local of its name.
#[derive(Default)]
struct Lets(Vec<BTreeMap<String, Option<String>>>);

impl Lets {
    fn get(&self, local: &str) -> Option<String> {
        self.0
            .iter()
            .rev()
            .find_map(|s| s.get(local))
            .cloned()
            .flatten()
    }

    /// A pattern binds a single local to `named`; any other binds each of
    /// its locals to nothing known.
    fn bind(&mut self, pat: &syn::Pat, named: Option<String>) {
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
            scope.insert(local, if single { named.clone() } else { None });
        }
    }

    /// An assignment changes the innermost local of its name, wherever it
    /// was bound.
    fn assign(&mut self, local: &str, named: Option<String>) {
        if let Some(held) = self.0.iter_mut().rev().find_map(|s| s.get_mut(local)) {
            *held = named;
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

/// The name an expression gives the path it builds: the last file name a
/// `join` or literal spells, else the field that holds it, or what the local
/// holding it was bound to, seen through borrows and conversions. `None` when
/// it's computed some other way or comes from a parameter: a helper's `path`
/// names nothing.
fn name(e: &syn::Expr, lets: &Lets) -> Option<String> {
    match e {
        syn::Expr::Reference(r) => name(&r.expr, lets),
        syn::Expr::Paren(p) => name(&p.expr, lets),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => s.value().rsplit('/').next().map(String::from),
        syn::Expr::Field(f) => match &f.member {
            syn::Member::Named(n) => Some(n.to_string()),
            syn::Member::Unnamed(_) => None,
        },
        syn::Expr::Path(p) => p.path.get_ident().and_then(|i| lets.get(&i.to_string())),
        syn::Expr::MethodCall(m) if m.method == "join" => {
            m.args.first().and_then(|a| name(a, lets))
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
                    c.args.first().and_then(|a| name(a, lets))
                }
                _ => None,
            }
        }
        _ => None,
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

#[test]
fn workspace_executables_use_the_child_installer() {
    let root = std::env::var_os("BURROW_TEST_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::harness::repo_file("crates"));
    let findings = check(&root);
    assert!(
        findings.is_empty(),
        "{}",
        findings.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn setters_cannot_bypass_the_guard() {
    for code in [
        "fs::set_permissions(p, Permissions::from_mode(0o755)).unwrap();",
        "use std::fs::set_permissions as change; change(p, mode);",
        "p.set_mode(0o755);",
        "fs::copy(source, destination).unwrap();",
        "use std::fs::copy as duplicate; duplicate(source, destination);",
        "File::options().mode(0o755);",
        "Command::new(\"chmod\").args([\"+x\", \"stand-in\"]);",
        "Command::new(\"sh\").arg(\"chmod +x stand-in\");",
        "wrapper!(fs::set_permissions(p, mode));",
        "fs::write(&p, \"#!/bin/sh\\nexit 0\\n\").unwrap();",
        "File::create(&p).unwrap().write_all(b\"#!/bin/sh\\n\").unwrap();",
        "let gh = bin.join(\"gh\"); burrow_test::executable(&gh, GH); fs::write(&gh, OTHER).unwrap();",
        "executable(bin.join(\"gh\"), GH); File::create(dir.path().join(\"bin/gh\")).unwrap();",
    ] {
        let ast = syn::parse_file(&format!("fn fixture() {{ {code} }}")).unwrap();
        let mut guard = Guard::new("fixture.rs".into(), true);
        guard.visit_file(&ast);
        assert!(!guard.finish().is_empty(), "accepted: {code}");
    }
}

/// The usage tests' notice capture (#902): the rig installs `notify`, and the
/// test wrote its new body over it in place, keeping its mode.
#[test]
fn writing_over_a_stand_in_is_rejected() {
    let rig = r##"
        fn rig() {
            let notify = d.path().join("notify");
            burrow_test::executable(&notify, format!("#!/bin/sh\necho \"$1\" >>{dir}/told\n"));
            cfg["notify"] = json!(notify);
        }
    "##;
    let overwrite = r##"
        #[test]
        fn old_holds_are_normalized_for_status_reminders_and_extensions() {
            fs::write(
                cfg.notify.as_ref().unwrap(),
                format!("#!/bin/sh\nprintf '%s' \"$4\" >'{}'\n", capture.display()),
            )
            .unwrap();
        }
    "##;
    let found = |code: &str| {
        let mut guard = Guard::new("usage/tests.rs".into(), true);
        guard.visit_file(&syn::parse_file(code).unwrap());
        guard.finish()
    };
    let old = found(&format!("{rig}{overwrite}"));
    assert!(old.iter().any(|f| f.contains("script in place")), "{old:?}");
    // Its path alone gives it away, whatever it writes.
    let renamed = overwrite.replace("format!(\"#!", "format!(\"");
    let renamed = found(&format!("{rig}{renamed}"));
    assert!(
        renamed
            .iter()
            .any(|f| f.contains("over the stand-in `notify`")),
        "{renamed:?}"
    );
    let fixed = overwrite.replace("fs::write(", "burrow_test::executable(");
    let fixed = fixed.replace(".unwrap();\n        }", ";\n        }");
    assert_eq!(found(&format!("{rig}{fixed}")), BTreeSet::new());
    // Data beside a stand-in is still written plainly.
    let data = "#[test] fn data() { executable(bin.join(\"gh\"), GH); fs::write(bin.join(\"says\"), \"[]\").unwrap(); }";
    assert_eq!(found(data), BTreeSet::new());
}

fn fixture_findings(code: &str) -> BTreeSet<String> {
    let mut guard = Guard::new("fixture.rs".into(), true);
    guard.visit_file(&syn::parse_file(code).unwrap());
    guard.finish()
}

/// A write is no safer for sitting in a macro, coming through a `use`, or
/// reaching its stand-in through a typed, shadowed or reassigned local.
#[test]
fn writes_cannot_hide_from_the_guard() {
    for code in [
        // Macro arguments, nested macros and statement bodies.
        "assert!(fs::write(cfg.notify.as_ref().unwrap(), \"#!/bin/sh\\n\").is_ok());",
        "executable(cfg.notify.as_ref().unwrap(), N); assert!(fs::write(cfg.notify.as_ref().unwrap(), OTHER).is_ok());",
        "wrapper!(assert!(File::create(&p).unwrap().write_all(b\"#!/bin/sh\\n\").is_ok()));",
        "executable(bin.join(\"gh\"), GH); assert!(File::create(bin.join(\"gh\")).is_ok());",
        "assert_eq!(executable(bin.join(\"gh\"), GH), ()); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        "wrapper! { let p = bin.join(\"gh\"); executable(&p, GH); fs::write(&p, OTHER).unwrap(); }",
        // Direct, grouped, renamed, module and glob imports.
        "use std::fs::write; write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::{write as put}; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::{fs::{self as files}}; files::write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs as files; use files::write as put; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::*; write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::write; executable(bin.join(\"gh\"), GH); write(bin.join(\"gh\"), OTHER).unwrap();",
        "use std::fs::File as F; executable(bin.join(\"gh\"), GH); F::create(bin.join(\"gh\")).unwrap();",
        "use burrow_test::executable as script; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        "use burrow_test::{self as bt}; use bt::executable as script; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        // Typed bindings, outer bindings after a nested scope, reassignment.
        "let p: PathBuf = bin.join(\"gh\"); executable(&p, GH); fs::write(&p, OTHER).unwrap();",
        "let p = bin.join(\"gh\"); executable(&p, GH); { let p = dir.join(\"data\"); fs::write(&p, DATA).unwrap(); } fs::write(&p, OTHER).unwrap();",
        "let mut p = dir.join(\"data\"); executable(bin.join(\"gh\"), GH); p = bin.join(\"gh\"); fs::write(&p, OTHER).unwrap();",
        "let mut p = dir.join(\"data\"); executable(bin.join(\"gh\"), GH); { p = bin.join(\"gh\"); } fs::write(&p, OTHER).unwrap();",
    ] {
        let found = fixture_findings(&format!("fn fixture() {{ {code} }}"));
        assert!(!found.is_empty(), "accepted: {code}");
    }
    // A test module sees its parent's imports through `use super::*`.
    let module = "use std::fs::write as put;\nmod tests { use super::*; fn t() { put(&p, \"#!/bin/sh\\n\").unwrap(); } }";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
    let module =
        "mod tests { use std::fs::write as put; fn t() { put(&p, \"#!/bin/sh\\n\").unwrap(); } }";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
}

/// What names nothing, or names something else by the time it's written,
/// is plain data beside a stand-in.
#[test]
fn data_beside_a_stand_in_is_still_written() {
    for code in [
        "fn install(notify: &Path) { executable(notify, N); } fn data(notify: &Path) { fs::write(notify, \"[]\").unwrap(); }",
        "fn fixture() { let mut p = bin.join(\"gh\"); executable(&p, GH); p = bin.join(\"says\"); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let mut p = bin.join(\"gh\"); executable(&p, GH); p = helper(); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); { let p = helper(); fs::write(&p, \"[]\").unwrap(); } }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); let (p, _) = (helper(), 0); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); let put = |p: &Path| fs::write(p, \"[]\"); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); for p in helpers() { fs::write(&p, \"[]\").unwrap(); } }",
        "fn fixture() { let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); println!(\"{p:?}\", p = bin.join(\"gh\")); fs::write(&p, \"[]\").unwrap(); }",
        "use std::fs::write; fn fixture() { executable(bin.join(\"gh\"), GH); write(bin.join(\"says\"), \"[]\").unwrap(); }",
    ] {
        assert_eq!(fixture_findings(code), BTreeSet::new(), "{code}");
    }
}

#[test]
fn test_helpers_are_followed_regardless_of_their_names() {
    let dir = tempfile::tempdir().unwrap();
    let member = dir.path().join("example");
    fs::create_dir_all(member.join("src/fixtures")).unwrap();
    fs::write(member.join("Cargo.toml"), "").unwrap();
    fs::write(
        member.join("src/lib.rs"),
        "fn production() { fs::set_permissions(p, mode); }\n#[cfg(test)] mod fixtures;",
    )
    .unwrap();
    fs::write(member.join("src/fixtures.rs"), "mod arbitrary;").unwrap();
    fs::write(
        member.join("src/fixtures/arbitrary.rs"),
        "fn helper() { fs::set_permissions(p, Permissions::from_mode(0o755)); }",
    )
    .unwrap();
    let found = check(dir.path());
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found.iter().all(|f| f.contains("arbitrary.rs")),
        "{found:?}"
    );
}
