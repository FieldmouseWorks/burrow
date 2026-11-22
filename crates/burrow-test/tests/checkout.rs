//! Two checkouts with one target directory: real Cargo, with the repo's own
//! .cargo and `manifest_dir!`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const READS: &str = r#"
#[test]
fn reads() {
    let read = std::fs::read_to_string(burrow_test::manifest_dir!().join("../../fact")).unwrap();
    let bin = std::process::Command::new(env!("CARGO_BIN_EXE_member")).output().unwrap();
    println!(
        "saw read={read} code=SIDE lib={} included={} bin={}.",
        member::CODE,
        include_str!("../../../fact"),
        String::from_utf8(bin.stdout).unwrap(),
    );
}
"#;

fn put(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A checkout laid out as burrow's is, every file of it saying `side`.
fn checkout(root: &Path, side: &str) {
    let crates = burrow_test::manifest_dir!().parent().unwrap();
    let (from, to) = (crates.join("../.cargo"), root.join(".cargo"));
    fs::create_dir_all(&to).unwrap();
    burrow_test::copy_file(from.join("config.toml"), to.join("config.toml"));
    burrow_test::executable(
        to.join("this-checkout"),
        fs::read(from.join("this-checkout")).unwrap(),
    );
    let package = |name: &str, rest: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{rest}")
    };
    let files = [
        ("fact", side.to_owned()),
        (
            "crates/Cargo.toml",
            "[workspace]\nresolver = \"3\"\nmembers = [\"burrow-test\", \"member\"]\n".to_owned(),
        ),
        ("crates/burrow-test/Cargo.toml", package("burrow-test", "")),
        (
            "crates/burrow-test/src/lib.rs",
            "pub mod checkout;\n".to_owned(),
        ),
        (
            "crates/burrow-test/src/checkout.rs",
            include_str!("../src/checkout.rs").to_owned(),
        ),
        (
            "crates/member/Cargo.toml",
            package(
                "member",
                "[dev-dependencies]\nburrow-test = { path = \"../burrow-test\" }\n",
            ),
        ),
        (
            "crates/member/src/lib.rs",
            format!("pub const CODE: &str = \"{side}\";\n"),
        ),
        (
            "crates/member/src/main.rs",
            format!("fn main() {{\n    print!(\"{side}\");\n}}\n"),
        ),
        ("crates/member/tests/reads.rs", READS.replace("SIDE", side)),
    ];
    for (path, text) in files {
        put(&root.join(path), &text);
    }
}

struct Checkouts {
    dir: tempfile::TempDir,
}

impl Checkouts {
    /// Both written before either is built, so each one's files are older
    /// than the other's build: what Cargo takes as fresh.
    fn new() -> Checkouts {
        let dir = tempfile::tempdir().unwrap();
        checkout(&dir.path().join("a"), "A");
        checkout(&dir.path().join("b"), "B");
        Checkouts { dir }
    }

    fn crates(&self, side: &str) -> PathBuf {
        self.dir.path().join(side).join("crates")
    }

    /// `cargo test` of `side`'s workspace, run from `from`, into the one
    /// target directory: whether it passed, and what it said.
    fn test(&self, from: &Path, side: &str) -> (bool, String) {
        let mut cargo = Command::new(std::env::var_os("CARGO").expect("cargo test sets CARGO"));
        cargo
            .current_dir(from)
            .args(["test", "--offline", "--test", "reads", "--manifest-path"])
            .arg(self.crates(side).join("member/Cargo.toml"))
            .args(["--", "--nocapture"]);
        // Nothing of how this test's own build was set up.
        for (name, _) in std::env::vars_os() {
            let text = name.to_string_lossy();
            if text.starts_with("CARGO_") && text != "CARGO_HOME"
                || matches!(
                    &*text,
                    "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER" | "RUSTFLAGS"
                )
            {
                cargo.env_remove(&name);
            }
        }
        let out = cargo
            .env("CARGO_TARGET_DIR", self.dir.path().join("target"))
            .output()
            .unwrap();
        let said = [out.stdout, out.stderr].concat();
        (
            out.status.success(),
            String::from_utf8_lossy(&said).into_owned(),
        )
    }

    fn reads_itself(&self, from: &Path, side: &str) {
        let (passed, said) = self.test(from, side);
        let s = side.to_uppercase();
        let saw = format!("saw read={s} code={s} lib={s} included={s} bin={s}.");
        assert!(passed && said.contains(&saw), "not {saw:?}:\n{said}");
    }

    fn refuses(&self, from: &Path, side: &str) {
        let (passed, said) = self.test(from, side);
        assert!(
            !passed && said.contains("this test was built from") && !said.contains("saw read="),
            "ran on another checkout's build:\n{said}"
        );
    }
}

#[test]
fn each_checkout_runs_its_own_tests_files_and_programs() {
    let both = Checkouts::new();
    // And back: the second's build leaves the first's as it was.
    for side in ["a", "b", "a", "b"] {
        both.reads_itself(&both.crates(side), side);
    }
    // From anywhere inside it.
    both.reads_itself(&both.dir.path().join("a"), "a");
}

#[test]
fn a_run_from_another_checkout_refuses() {
    let both = Checkouts::new();
    both.reads_itself(&both.crates("a"), "a");
    both.refuses(&both.crates("a"), "b");
}

#[test]
fn a_run_from_outside_refuses_another_checkouts_build() {
    let both = Checkouts::new();
    both.reads_itself(both.dir.path(), "a");
    both.refuses(both.dir.path(), "b");
}

/// `env!` of it anywhere else is a test that reads whichever checkout built it.
#[test]
fn tests_find_the_repo_through_manifest_dir() {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "target" {
                    walk(&path, found);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && fs::read_to_string(&path)
                    .unwrap()
                    .contains(concat!("\"CARGO_", "MANIFEST_DIR\""))
            {
                found.push(path);
            }
        }
    }
    let crates = burrow_test::manifest_dir!().parent().unwrap();
    let mut found = Vec::new();
    walk(crates, &mut found);
    assert_eq!(
        found,
        [crates.join("burrow-test/src/checkout.rs")],
        "find the repo with burrow_test::manifest_dir!()"
    );
}
