//! End-to-end build store substitution with real Nix and a local signed HTTP
//! binary cache (`burrow_test::build_store`):
//! - a leg substitutes a signed check from the store using `check`'s
//!   `install-nix-action` `extra_nix_config`, without running its failing
//!   builder and with no restore step;
//! - the negative job substitutes a check's signed dependency from the store
//!   using `negative`'s `install-nix-action` `extra_nix_config`, without
//!   running its failing builder and with no restore step;
//! - when the store is down (either skipped after `soft/builds` fails, or
//!   unreachable mid-run with `fallback = true`), a run builds cold;
//! - a leg with `BUILDS` set still refuses a flake's own `nixConfig` and
//!   enforces `sandbox = true`.

use std::io::Write as _;
use std::process::Stdio;

use super::*;
use crate::expr::{Ctx, V, value};

/// Render `extra_nix_config` from `job`'s `cachix/install-nix-action` step
/// for the given store variables and `soft/builds` outcome.
fn extra_nix_config(job_name: &str, url: &str, key: &str, probe_outcome: &str) -> String {
    let wf = workflow(".github/workflows/flake-check.yml");
    let job = &wf["jobs"][job_name];
    let install = crate::harness::steps(job)
        .iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|u| u.starts_with("cachix/install-nix-action@"))
        })
        .expect("install-nix-action");
    let c: Ctx = [
        ("vars.BURROW_BUILDS_URL", V::from(url)),
        ("vars.BURROW_BUILDS_PUBLIC_KEY", V::from(key)),
        ("steps.builds.outcome", V::from(probe_outcome)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    value(text(&install["with"]["extra_nix_config"]), &c).text()
}

/// Real Nix on a chroot store of its own (no `nix build` stub) with a signed
/// HTTP binary cache beside it.
struct RealStore {
    s: Scratch,
    root: PathBuf,
    cache: PathBuf,
    sk: PathBuf,
    pk: String,
    system: String,
}

impl RealStore {
    fn new() -> Self {
        let s = Scratch::default();
        let root = s.join("root");
        let cache = s.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let cfg = format!(
            "experimental-features = nix-command flakes\nstore = local?root={}\nsubstituters =\nsubstitute = true\nflake-registry =\nsandbox = false",
            root.display()
        );
        let run = |program: &str, args: &[&str]| {
            let mut c = s.command(which(program));
            c.args(args)
                .env("NIX_CONFIG", &cfg)
                .env("XDG_CACHE_HOME", s.join("xdg-cache"))
                .stdin(Stdio::null());
            let o = output(c);
            assert!(
                o.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8(o.stdout).unwrap().trim().to_owned()
        };
        let system = run("nix", &["config", "show", "system"]);
        let (sk, pk_file) = (s.join("sk"), s.join("pk"));
        run(
            "nix-store",
            &[
                "--generate-binary-cache-key",
                "machine-1",
                sk.to_str().unwrap(),
                pk_file.to_str().unwrap(),
            ],
        );
        let pk = read(&pk_file).trim().to_owned();
        write(&cache.join("nix-cache-info"), "StoreDir: /nix/store\n");
        Self {
            s,
            root,
            cache,
            sk,
            pk,
            system,
        }
    }

    fn base_config(&self, root: &Path) -> String {
        format!(
            "experimental-features = nix-command flakes\nstore = local?root={}\nsubstituters =\nsubstitute = true\nflake-registry =\nsandbox = false",
            root.display()
        )
    }

    /// Pre-populate and sign the output of `drv` in `self.cache`, then
    /// delete it from the helper store so only the HTTP build store has it.
    fn populate_signed(&self, drv: &str, contents: &str) -> String {
        let cfg = self.base_config(&self.root);
        let run = |program: &str, args: &[&str]| {
            let mut c = self.s.command(which(program));
            c.args(args)
                .env("NIX_CONFIG", &cfg)
                .env("XDG_CACHE_HOME", self.s.join("xdg-cache"))
                .stdin(Stdio::null());
            let o = output(c);
            assert!(
                o.status.success(),
                "{program} {args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8(o.stdout).unwrap().trim().to_owned()
        };
        let out = run("nix-store", &["--query", "--outputs", drv]);
        let dst = self.root.join(out.trim_start_matches('/'));
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        write(&dst, contents);
        let mut reg = self
            .s
            .command(which("nix-store"))
            .args(["--register-validity", "--reregister"])
            .env("NIX_CONFIG", &cfg)
            .env("XDG_CACHE_HOME", self.s.join("xdg-cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(reg.stdin.take().unwrap(), "{out}\n\n0").unwrap();
        let reg = reg.wait_with_output().unwrap();
        assert!(
            reg.status.success(),
            "register-validity: {}",
            String::from_utf8_lossy(&reg.stderr)
        );
        run(
            "nix",
            &[
                "store",
                "sign",
                "--key-file",
                self.sk.to_str().unwrap(),
                &out,
            ],
        );
        run(
            "nix",
            &[
                "copy",
                "--to",
                &format!("file://{}", self.cache.display()),
                &out,
            ],
        );
        run("nix", &["store", "delete", &out]);
        out
    }

    fn instantiate(&self, expr: &str) -> String {
        let mut c = self.s.command(which("nix-instantiate"));
        c.args(["--expr", expr])
            .env("NIX_CONFIG", self.base_config(&self.root))
            .env("XDG_CACHE_HOME", self.s.join("xdg-cache"))
            .stdin(Stdio::null());
        let o = output(c);
        assert!(
            o.status.success(),
            "nix-instantiate: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    }

    /// Stub only `nix-build` for compiler provisioning so `gate.sh` and
    /// `negative.sh` compile the real runner while invoking REAL `nix`.
    fn stub_compiler(&self) {
        let compiler = std::process::Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .unwrap();
        assert!(compiler.status.success());
        let sysroot = String::from_utf8(compiler.stdout).unwrap();
        fs::create_dir_all(self.s.join("compiler/bin")).unwrap();
        crate::harness::executable(
            &self.s.join("compiler/bin/rustc"),
            &format!("exec {}/bin/rustc \"$@\"\n", sysroot.trim()),
        );
        std::os::unix::fs::symlink(which("cc"), self.s.join("compiler/bin/cc")).unwrap();
        let expression = repo_file(".github/flake-check/compiler.nix")
            .canonicalize()
            .unwrap();
        self.s.stub(
            "nix-build",
            &format!(
                r#"[ "$#" = 5 ] && [ "$1" = --extra-experimental-features ] && [ "$2" = 'nix-command flakes' ] || exit 90
[ "$3" = '{}' ] && [ "$4" = --out-link ] && [ -d "${{5%/*}}" ] || exit 90
printf '%s\n' '{}'
"#,
                expression.display(),
                self.s.join("compiler").display()
            ),
        );
    }
}

/// Both a leg (`check`) and the negative (`negative`) substitute signed
/// outputs from the HTTP build store using their job's `install-nix-action`
/// `extra_nix_config`, with no restore step: the pre-populated derivation's
/// builder exits 42 if ever executed, so passing proves substitution.
#[test]
fn a_leg_and_the_negative_substitute_from_the_store_with_no_restore_step() {
    let rs = RealStore::new();
    rs.stub_compiler();
    let system = &rs.system;

    // A derivation whose builder fails if executed: only substitution from
    // the signed build store can realise it.
    let cached_expr = format!(
        r#"derivation {{ name = "from-store"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "exit 42" ]; }}"#
    );
    let cached_drv = rs.instantiate(&cached_expr);
    let cached_out = rs.populate_signed(&cached_drv, "substituted from build store\n");
    let url = burrow_test::build_store(&rs.cache);

    // 1. A leg (`check`'s gate step) substitutes `from-store` from the HTTP
    //    store and builds `cold` locally, with no restore step.
    let leg_flake = rs.s.join("leg-flake");
    fs::create_dir_all(&leg_flake).unwrap();
    write(
        &leg_flake.join("flake.nix"),
        &format!(
            r#"{{
  outputs = {{ self }}: {{
    checks.{system} = {{
      cached = {cached_expr};
      cold = derivation {{ name = "cold"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo cold > $out" ]; }};
    }};
  }};
}}
"#
        ),
    );
    rs.s.git(&leg_flake, &["init", "-q"]);
    rs.s.git(&leg_flake, &["add", "flake.nix"]);
    rs.s.git(&leg_flake, &["commit", "-qm", "flake"]);

    let leg_root = rs.s.join("leg-root");
    let leg_cfg = format!(
        "{}\n{}",
        rs.base_config(&leg_root),
        extra_nix_config("check", &url, &rs.pk, "success")
    );
    let wf = workflow(".github/workflows/flake-check.yml");
    let leg_temp = rs.s.mkdtemp();
    let ran = rs.s.bash(
        script(step(&wf["jobs"]["check"], "gate")),
        &leg_flake,
        &[
            ("NIX_CONFIG", &leg_cfg),
            ("XDG_CACHE_HOME", rs.s.join("leg-xdg").to_str().unwrap()),
            ("APART", "true"),
            ("SKIP", ""),
            ("LEG", "1"),
            ("LEGS", "1"),
            ("TREE_SOURCE", "false"),
            ("BUILDS", ""),
            ("RUNNER_TEMP", leg_temp.to_str().unwrap()),
            ("TOOLS", repo_file(".github/flake-check").to_str().unwrap()),
        ],
    );
    assert_eq!(ran.code, 0, "leg failed to substitute:\n{}", ran.said());
    assert!(
        leg_root.join(cached_out.trim_start_matches('/')).is_file(),
        "cached output wasn't substituted into leg store:\n{}",
        ran.said()
    );

    // 2. The negative job substitutes `from-store` as a check dependency
    //    from the HTTP build store, with no restore step, while failing the
    //    PR's new test on the base's code.
    let origin = rs.s.join("neg-origin");
    fs::create_dir_all(&origin).unwrap();
    write(
        &origin.join("flake.nix"),
        &format!(
            r#"{{
  outputs = {{ self }}:
    let
      copy = path: builtins.path {{ inherit path; name = baseNameOf path; }};
      dep = {cached_expr};
    in {{
      checks.{system}.t = derivation {{
        name = "t";
        system = "{system}";
        builder = "/bin/sh";
        args = [ "-c" "[ -f ${{dep}} ] && read -r a < ${{copy ./lib.txt}} && read -r b < ${{copy ./tests/want.txt}} && [ \"$a\" = \"$b\" ] && echo ok > $out" ];
      }};
    }};
}}
"#
        ),
    );
    write(&origin.join("lib.txt"), "1\n");
    write(&origin.join("tests/want.txt"), "1\n");
    rs.s.git(&origin, &["init", "-q"]);
    rs.s.git(&origin, &["add", "-A"]);
    rs.s.git(&origin, &["commit", "-qm", "main"]);
    let co = rs.s.join("neg-co");
    rs.s.git(
        rs.s.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            co.to_str().unwrap(),
        ],
    );
    rs.s.git(&origin, &["checkout", "-q", "-b", "pr"]);
    write(&origin.join("lib.txt"), "2\n");
    write(&origin.join("tests/want.txt"), "2\n");
    rs.s.git(&origin, &["add", "-A"]);
    rs.s.git(&origin, &["commit", "-qm", "pr"]);
    let head = rs.s.git(&origin, &["rev-parse", "HEAD"]);
    rs.s.git(
        &co,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    rs.s.git(&co, &["checkout", "-q", "--detach", &head]);

    let neg_root = rs.s.join("neg-root");
    let neg_cfg = format!(
        "{}\n{}",
        rs.base_config(&neg_root),
        extra_nix_config("negative", &url, &rs.pk, "success")
    );
    let neg_temp = rs.s.mkdtemp();
    let neg_job = &wf["jobs"]["negative"];
    let paths = rs.s.bash(
        script(step(neg_job, "paths")),
        &co,
        &[
            ("NIX_CONFIG", &neg_cfg),
            ("XDG_CACHE_HOME", rs.s.join("neg-xdg").to_str().unwrap()),
            ("RUNNER_TEMP", neg_temp.to_str().unwrap()),
            ("TOOLS", repo_file(".github/flake-check").to_str().unwrap()),
            ("BASE", "main"),
            ("HEAD_SHA", &head),
            ("SKIP", ""),
            ("PATHS_ONLY", "true"),
        ],
    );
    assert_eq!(paths.code, 0, "{}", paths.said());
    assert!(paths.outputs.contains("needs=true"), "{}", paths.outputs);
    let neg = rs.s.bash(
        script(step(neg_job, "negative")),
        &co,
        &[
            ("NIX_CONFIG", &neg_cfg),
            ("XDG_CACHE_HOME", rs.s.join("neg-xdg").to_str().unwrap()),
            ("RUNNER_TEMP", neg_temp.to_str().unwrap()),
            ("TOOLS", repo_file(".github/flake-check").to_str().unwrap()),
            ("BASE", "main"),
            ("HEAD_SHA", &head),
            ("SKIP", ""),
            ("PATHS_ONLY", "false"),
        ],
    );
    assert_eq!(
        neg.code,
        0,
        "negative failed to substitute:\n{}",
        neg.said()
    );
    let found: serde_json::Value =
        serde_json::from_str(&read(&neg_temp.join("negative.json"))).unwrap();
    assert_eq!(found["result"], "fail", "{found}");
    assert!(
        neg_root.join(cached_out.trim_start_matches('/')).is_file(),
        "dep wasn't substituted into negative store:\n{}",
        neg.said()
    );
}

/// When the build store is unreachable—both when `soft/builds` fails and
/// omits it from `extra_nix_config`, when the store drops mid-run, and when
/// a narinfo is served but its NAR fails to download (`fallback = true`)—a
/// leg builds cold and passes.
#[test]
fn the_store_down_builds_cold() {
    let rs = RealStore::new();
    rs.stub_compiler();
    let system = &rs.system;
    let cold_expr = format!(
        r#"derivation {{ name = "cold"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo built-cold > $out" ]; }}"#
    );
    let flake = rs.s.join("cold-flake");
    fs::create_dir_all(&flake).unwrap();
    write(
        &flake.join("flake.nix"),
        &format!(
            r#"{{
  outputs = {{ self }}: {{
    checks.{system}.cold = {cold_expr};
  }};
}}
"#
        ),
    );
    rs.s.git(&flake, &["init", "-q"]);
    rs.s.git(&flake, &["add", "flake.nix"]);
    rs.s.git(&flake, &["commit", "-qm", "flake"]);

    // Populate a signed narinfo for `cold` in the cache, then truncate its
    // NAR so the store serves a corrupt download: only `fallback = true`
    // builds cold instead of failing the leg.
    let cold_drv = rs.instantiate(&cold_expr);
    rs.populate_signed(&cold_drv, "from-cache\n");
    for entry in fs::read_dir(rs.cache.join("nar")).unwrap() {
        write(&entry.unwrap().path(), "truncated");
    }
    let broken_nar_url = burrow_test::build_store(&rs.cache);

    let closed = burrow_test::ClosedPort::new();
    let down_url = format!("http://{}/builds", closed.addr());
    let wf = workflow(".github/workflows/flake-check.yml");

    // Case A: probe failed -> extra_nix_config omits the dead store;
    // Case B: probe succeeded earlier, store died mid-run -> builds cold;
    // Case C: narinfo served, NAR download fails -> fallback = true builds cold.
    for (case, url, probe_outcome, expect_cold_msg) in [
        ("probe-failed", down_url.as_str(), "failure", true),
        ("mid-run-drop", down_url.as_str(), "success", true),
        ("nar-fails", broken_nar_url.as_str(), "success", false),
    ] {
        let root = rs.s.join(&format!("root-{case}"));
        let cfg = format!(
            "{}\nconnect-timeout = 2\ndownload-attempts = 1\n{}",
            rs.base_config(&root),
            extra_nix_config("check", url, &rs.pk, probe_outcome)
        );
        let temp = rs.s.mkdtemp();
        let ran = rs.s.bash(
            script(step(&wf["jobs"]["check"], "gate")),
            &flake,
            &[
                ("NIX_CONFIG", &cfg),
                (
                    "XDG_CACHE_HOME",
                    rs.s.join(&format!("xdg-{case}")).to_str().unwrap(),
                ),
                ("APART", "true"),
                ("SKIP", ""),
                ("LEG", "1"),
                ("LEGS", "1"),
                ("TREE_SOURCE", "false"),
                ("BUILDS", ""),
                ("RUNNER_TEMP", temp.to_str().unwrap()),
                ("TOOLS", repo_file(".github/flake-check").to_str().unwrap()),
            ],
        );
        assert_eq!(ran.code, 0, "{case} failed to build cold:\n{}", ran.said());
        if expect_cold_msg {
            assert!(
                ran.said().contains("cold build: 1 derivations"),
                "{case}:\n{}",
                ran.said()
            );
        }
    }
}

/// With a build store named (`BUILDS` set), every build the leg starts
/// refuses a flake's own `nixConfig` and is sandboxed; without one, the
/// runner's config stands.
#[test]
fn a_writing_leg_refuses_flake_config_and_enforces_sandbox() {
    let g = Gate::new();
    let flake = g.flake(r#"{ checks.${system}.a = drv "a"; }"#);
    let runner = format!("{}\naccept-flake-config = true", g.nix_config);

    let ran = g.run(
        &flake,
        Run {
            env: &[
                ("NIX_CONFIG", &runner),
                ("BUILDS", "https://builds.example/builds"),
            ],
            ..Run::default()
        },
    );
    assert_eq!(ran.code, 0, "{}", ran.out);
    let built = lines(&g.s.join("nix.log.config"));
    assert!(!built.is_empty(), "{}", ran.out);
    for c in &built {
        assert!(
            c.contains(r#""accept-flake-config":false"#) && c.contains(r#""sandbox":true"#),
            "a writing build took flake config or dropped sandbox: {c}"
        );
    }

    let ran = g.run(
        &flake,
        Run {
            env: &[("NIX_CONFIG", &runner), ("BUILDS", "")],
            ..Run::default()
        },
    );
    assert_eq!(ran.code, 0, "{}", ran.out);
    for c in lines(&g.s.join("nix.log.config")) {
        assert!(c.contains(r#""accept-flake-config":true"#), "{c}");
    }
}
