//! burrow-keep against real nix: stores of their own standing in for three
//! fresh runners, a binary cache of what a public cache would serve them,
//! and derivations nothing builds (their outputs are registered as built).
//! What each push keeps is what the next substitutes from, with what it
//! refers to coming from the public cache, as it would on a runner.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

struct Case {
    dir: tempfile::TempDir,
}

/// Two derivations: a check, which refers to a library a public cache
/// serves, and what the check was built from, which it doesn't refer to.
const DRVS: &str = r#"
  let
    drv = name: extra: derivation ({ inherit name; system = builtins.currentSystem; builder = "/bin/sh"; } // extra);
    dep = drv "dep" { };
  in { check = drv "check" { inherit dep; }; inherit dep; }
"#;

impl Case {
    fn new() -> Case {
        Case {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// A command run on `store`, with nothing of this process's environment
    /// but PATH, and the caches given as substituters.
    fn on(&self, store: &str, program: &str, substituters: &[&str]) -> Command {
        let subs: Vec<String> = substituters
            .iter()
            .map(|s| format!("file://{}?trusted=true", self.path(s).display()))
            .collect();
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("HOME", self.dir.path())
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env(
                "NIX_CONFIG",
                format!(
                    "experimental-features = nix-command\nstore = {}\nsubstituters = {}\nrequire-sigs = false\nbuild-users-group =\nsandbox = false\n",
                    self.path(store).display(),
                    subs.join(" ")
                ),
            );
        cmd
    }

    fn run(&self, mut cmd: Command) -> String {
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{cmd:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn nix(&self, store: &str, args: &[&str], substituters: &[&str]) -> String {
        let mut cmd = self.on(store, "nix", substituters);
        cmd.args(args);
        self.run(cmd)
    }

    fn nix_store(&self, store: &str, args: &[&str], substituters: &[&str]) -> String {
        let mut cmd = self.on(store, "nix-store", substituters);
        cmd.args(args);
        self.run(cmd)
    }

    /// The derivations in `store`, and the paths of their outputs: the
    /// check's, then dep's.
    fn drvs(&self, store: &str) -> (String, String) {
        let mut cmd = self.on(store, "nix-instantiate", &[]);
        cmd.args(["--expr", DRVS, "-A", "check", "-A", "dep"]);
        let made = self.run(cmd);
        let drvs: Vec<&str> = made.lines().collect();
        let outs: Vec<String> = drvs
            .iter()
            .map(|d| self.nix_store(store, &["--query", "--outputs", d], &[]))
            .collect();
        (outs[0].clone(), outs[1].clone())
    }

    /// A path in `store` as nix has it once something built it (or fetched
    /// it unsigned): its contents, and what it refers to.
    fn register(&self, store: &str, path: &str, contents: &str, refs: &[&str]) {
        let file = self.path(&format!("{store}{path}"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, contents).unwrap();
        let mut dump = Command::new("nix-store");
        dump.arg("--dump").arg(&file);
        let nar = piped(dump, b"");
        let digest: String = Sha256::digest(&nar)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut db = format!("{path}\n{digest}\n{}\n\n{}\n", nar.len(), refs.len());
        for r in refs {
            db.push_str(r);
            db.push('\n');
        }
        let mut load = self.on(store, "nix-store", &[]);
        load.arg("--load-db");
        piped(load, db.as_bytes());
    }

    /// A download in `store`, unsigned, as a fetchurl leaves one: a flat
    /// fixed-output path holding this.
    fn download(&self, store: &str, name: &str, contents: &str) -> String {
        let file = self.path(name);
        fs::write(&file, contents).unwrap();
        self.nix_store(
            store,
            &["--add-fixed", "sha256", file.to_str().unwrap()],
            &[],
        )
    }

    /// A push's keep step: burrow-keep on `store`, with `run/kept` holding
    /// what was restored.
    fn keep(&self, store: &str, run: &str) -> String {
        let mut cmd = self.on(store, env!("CARGO_BIN_EXE_burrow-keep"), &[]);
        cmd.arg(self.path(run));
        self.run(cmd)
    }

    /// The paths a cache holds.
    fn cached(&self, cache: &str) -> Vec<String> {
        let mut paths: Vec<String> = fs::read_dir(self.path(cache))
            .unwrap()
            .filter_map(|f| {
                let f = f.unwrap().path();
                f.extension().filter(|e| *e == "narinfo")?;
                let text = fs::read_to_string(f).unwrap();
                Some(
                    text.lines()
                        .next()?
                        .trim_start_matches("StorePath: ")
                        .to_owned(),
                )
            })
            .collect();
        paths.sort();
        paths
    }

    fn valid(&self, store: &str, path: &str) -> bool {
        let mut cmd = self.on(store, "nix-store", &[]);
        cmd.args(["--check-validity", path]);
        cmd.output().unwrap().status.success()
    }
}

/// What it printed, given this on its stdin; it must succeed.
fn piped(mut cmd: Command, input: &[u8]) -> Vec<u8> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for f in fs::read_dir(from).unwrap() {
        let f = f.unwrap();
        if f.file_type().unwrap().is_dir() {
            copy_dir(&f.path(), &to.join(f.file_name()));
        } else {
            fs::copy(f.path(), to.join(f.file_name())).unwrap();
        }
    }
}

#[test]
fn what_a_push_keeps_the_next_substitutes_with_what_a_public_cache_serves() {
    let c = Case::new();
    // Paths nothing makes, so any hash will do.
    let lib = &format!("/nix/store/{}-lib", "1".repeat(32));
    let old = &format!("/nix/store/{}-old-check", "2".repeat(32));

    let wrapper = &format!("/nix/store/{}-wrapper", "3".repeat(32));

    // Push 1 built the check and what it was built from, and an old check
    // no derivation makes any more; the library came signed from a public
    // cache. It fetched a tarball, and a script that a wrapper it built
    // runs.
    let (check, dep) = c.drvs("s1");
    c.register("s1", lib, "lib", &[]);
    c.register("s1", &dep, "dep", &[]);
    c.register("s1", &check, &format!("check of {lib}"), &[lib]);
    c.register("s1", old, "old", &[]);
    let tarball = c.download("s1", "tarball", "a tarball");
    let script = c.download("s1", "script", "a script");
    c.register("s1", wrapper, &format!("runs {script}"), &[&script]);
    let key = c.path("key");
    fs::write(
        &key,
        c.nix(
            "s1",
            &["key", "generate-secret", "--key-name", "public-1"],
            &[],
        ),
    )
    .unwrap();
    c.nix(
        "s1",
        &["store", "sign", "--key-file", key.to_str().unwrap(), lib],
        &[],
    );
    let public = format!("file://{}", c.path("public").display());
    c.nix("s1", &["copy", "--to", &public, lib], &[]);
    fs::create_dir_all(c.path("run1/kept")).unwrap();
    let said = c.keep("s1", "run1");
    assert!(
        said.starts_with("kept 5 paths from this gate and 0 from the kept builds")
            && said.ends_with("left out 1 downloads, 1 MiB"),
        "{said}"
    );
    let mut first = vec![
        check.clone(),
        dep.clone(),
        old.to_owned(),
        wrapper.to_owned(),
        script.clone(),
    ];
    first.sort();
    assert_eq!(c.cached("run1/kept"), first);
    assert!(!c.cached("run1/kept").contains(&tarball));

    // A fresh runner substitutes the check from what push 1 kept, and the
    // library it refers to from the public cache; and the wrapper, with the
    // script it runs.
    c.nix_store(
        "s2",
        &["--realise", &check, wrapper],
        &["run1/kept", "public"],
    );
    assert!(c.valid("s2", &check) && c.valid("s2", lib));
    assert!(c.valid("s2", wrapper) && c.valid("s2", &script));

    // Push 2: the check hadn't changed, so it came whole from the kept
    // builds, and dep, which it was built from, never reached the store.
    // dep is carried, as its derivation still makes it; the old check goes.
    c.drvs("s3");
    c.nix_store("s3", &["--realise", &check], &["run1/kept", "public"]);
    assert!(!c.valid("s3", &dep));
    copy_dir(&c.path("run1/kept"), &c.path("run2/kept"));
    let said = c.keep("s3", "run2");
    assert!(
        said.starts_with("kept 1 paths from this gate and 1 from the kept builds"),
        "{said}"
    );
    let mut second = vec![check.clone(), dep.clone()];
    second.sort();
    assert_eq!(c.cached("run2/kept"), second);

    // The next change to the check takes dep from what push 2 kept instead
    // of building it.
    c.nix_store("s4", &["--realise", &dep], &["run2/kept", "public"]);
    assert!(c.valid("s4", &dep));
}
