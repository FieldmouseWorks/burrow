//! The workspace has one way to install executable fixtures. Follow test
//! modules from Cargo's entry points, including helpers with arbitrary names.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

use imports::{Imports, Kind};
use lets::{Lets, Name, Names, local, name};

mod imports;
mod lets;

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
    installed: BTreeSet<(PathBuf, Name)>,
    /// Each file's plain writes, by the name of the path written.
    written: BTreeSet<(PathBuf, Name)>,
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
        let files = |names: &BTreeSet<(PathBuf, Name)>| {
            names
                .iter()
                .flat_map(|(file, name)| {
                    self.lets
                        .files(name)
                        .into_iter()
                        .map(move |n| (file.clone(), n))
                })
                .collect::<BTreeSet<_>>()
        };
        let installed = files(&self.installed);
        for (file, name) in files(&self.written).intersection(&installed) {
            self.findings.insert(format!(
                "{}: writes over the stand-in `{name}`: use burrow_test",
                file.display()
            ));
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
            self.uses.enter(Kind::Module, items.iter());
            for item in items {
                self.visit_item(item);
            }
            self.uses.leave();
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
        self.uses.enter(Kind::Module, f.items.iter());
        visit::visit_file(self, f);
        self.uses.leave();
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
        self.uses.enter(Kind::Block, items(&b.stmts));
        self.scope(|g| visit::visit_block(g, b));
        self.uses.leave();
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
        self.deferred(|g| {
            g.scope(|g| {
                for input in &c.inputs {
                    g.lets.bind(input, Names::new(), g.maybe);
                }
                visit::visit_expr_closure(g, c);
            })
        });
    }

    fn visit_expr_async(&mut self, e: &'ast syn::ExprAsync) {
        self.deferred(|g| visit::visit_expr_async(g, e));
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        self.scope(|g| {
            g.maybe(|g| {
                g.lets.bind(&a.pat, Names::new(), g.maybe);
                visit::visit_arm(g, a);
            })
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        self.repeat(|g| {
            g.lets.bind(&f.pat, Names::new(), g.maybe);
            g.visit_block(&f.body);
        });
    }

    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.repeat(|g| g.visit_block(&e.body));
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
        self.lets.bind(&e.pat, Names::new(), self.maybe);
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
        self.repeat(|g| {
            g.visit_expr(&e.cond);
            g.visit_block(&e.body);
        });
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
            let names = self.uses.resolve(&p.path);
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
                    let named = named_arguments(&m.path, &args.iter().collect::<Vec<_>>());
                    for (i, arg) in args.iter().enumerate() {
                        match arg {
                            // After a formatting macro's format string,
                            // `name = value` names an argument and assigns
                            // nothing.
                            syn::Expr::Assign(a) if i >= named && local(&a.left) => {
                                g.visit_expr(&a.right)
                            }
                            _ => g.visit_expr(arg),
                        }
                    }
                } else if let Ok(stmts) = m.parse_body_with(syn::Block::parse_within) {
                    g.uses.enter(Kind::Block, items(&stmts));
                    for stmt in &stmts {
                        g.visit_stmt(stmt);
                    }
                    g.uses.leave();
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
        self.lets.push();
        visit(self);
        self.lets.pop();
    }

    /// Runs a loop's body, which may run again with what its last run
    /// assigned, until a run changes no local in scope: a write sees every
    /// path a later run's assignment may give it. `continue` reaches the
    /// next run with no more than the end of this one holds, since what the
    /// body assigns to outer locals only adds to them.
    fn repeat(&mut self, visit: impl Fn(&mut Self)) {
        let first = self.lets.first();
        loop {
            let before = self.lets.rewind(first);
            self.scope(|g| g.maybe(&visit));
            if !self.lets.changed(&before) {
                break;
            }
        }
    }

    /// Runs `visit` as code that may run later, any number of times: what
    /// it reads from an outer local is anything that local ever holds.
    fn deferred(&mut self, visit: impl FnOnce(&mut Self)) {
        self.lets.deferred += 1;
        self.maybe(visit);
        self.lets.deferred -= 1;
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
        let old = (self.lets.take_scopes(), self.maybe, self.lets.deferred);
        (self.maybe, self.lets.deferred) = (0, 0);
        self.scope(|g| {
            for input in &sig.inputs {
                if let syn::FnArg::Typed(t) = input {
                    g.lets.bind(&t.pat, Names::new(), 0);
                }
            }
            visit(g);
        });
        let scopes;
        (scopes, self.maybe, self.lets.deferred) = old;
        self.lets.restore_scopes(scopes);
    }

    fn reject_setter(&mut self, name: &str) {
        if matches!(name, "set_permissions" | "chmod" | "fchmod" | "copy") {
            self.reject(name);
        }
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

/// The items among a block's statements.
fn items(stmts: &[syn::Stmt]) -> impl Iterator<Item = &syn::Item> {
    stmts.iter().filter_map(|s| match s {
        syn::Stmt::Item(i) => Some(i),
        _ => None,
    })
}

/// Where a macro's named format arguments start: after its format string,
/// for the standard formatting macros and the common ones built on them.
/// Any other macro (`dbg!`, `vec!`, a wrapper) takes expressions, so a
/// `name = value` in it assigns.
fn named_arguments(path: &syn::Path, args: &[&syn::Expr]) -> usize {
    let Some(last) = path.segments.last() else {
        return usize::MAX;
    };
    let format = match last.ident.to_string().as_str() {
        "format" | "format_args" | "print" | "println" | "eprint" | "eprintln" | "panic"
        | "unreachable" | "todo" | "unimplemented" | "anyhow" | "bail" | "trace" | "debug"
        | "info" | "warn" | "error" => 0,
        "write" | "writeln" | "assert" | "debug_assert" | "ensure" => 1,
        "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => 2,
        _ => return usize::MAX,
    };
    match args.get(format) {
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(_),
            ..
        })) => format + 1,
        _ => usize::MAX,
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
