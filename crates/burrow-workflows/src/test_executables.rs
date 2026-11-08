//! The workspace has one way to install executable fixtures. Follow test
//! modules from Cargo's entry points, including helpers with arbitrary names.
use std::collections::BTreeSet;
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
}

impl Guard {
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
            for item in items {
                self.visit_item(item);
            }
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

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let old = self.testing;
        self.testing |= test_attrs(&f.attrs);
        visit::visit_item_fn(self, f);
        self.testing = old;
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
            let names: Vec<_> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
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

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.tokens(
            m.tokens.clone(),
            m.path.is_ident("format") || m.path.is_ident("concat"),
        );
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

    fn reject_setter(&mut self, name: &str) {
        if matches!(name, "set_permissions" | "chmod" | "fchmod" | "copy") {
            self.reject(name);
        }
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
    let mut guard = Guard {
        file: PathBuf::new(),
        modules: PathBuf::new(),
        testing: false,
        findings: BTreeSet::new(),
    };
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
    guard.findings
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
    ] {
        let ast = syn::parse_file(&format!("fn fixture() {{ {code} }}")).unwrap();
        let mut guard = Guard {
            file: "fixture.rs".into(),
            modules: PathBuf::new(),
            testing: true,
            findings: BTreeSet::new(),
        };
        guard.visit_file(&ast);
        assert!(!guard.findings.is_empty(), "accepted: {code}");
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
