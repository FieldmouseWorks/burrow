//! Two checkouts with one target directory: real Cargo, with the repo's own
//! .cargo and `manifest_dir!`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const READS: &str = r#"
#[test]
fn reads() {
    let read = std::fs::read_to_string(burrow_test::manifest_dir!().join("../../fact")).unwrap();
    let bin = std::process::Command::new(burrow_test::program!("member-bin")).output().unwrap();
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
    let to = root.join(".cargo");
    fs::create_dir_all(&to).unwrap();
    burrow_test::copy_file(crates.join("../.cargo/config.toml"), to.join("config.toml"));
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
                // A program's name with a dash, which its crate's name can't have.
                "[[bin]]\nname = \"member-bin\"\npath = \"src/main.rs\"\n\n\
                 [dev-dependencies]\nburrow-test = { path = \"../burrow-test\" }\n",
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

/// `cargo` without the environment of this test's own build.
fn clean(cargo: &mut Command) -> &mut Command {
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
    cargo
}

fn said(out: std::process::Output) -> (bool, String) {
    let said = [out.stdout, out.stderr].concat();
    (
        out.status.success(),
        String::from_utf8_lossy(&said).into_owned(),
    )
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

    /// Cargo, run from `from` into the one target directory with nothing of
    /// how this test's own build was set up: whether it passed, and what it
    /// said.
    fn cargo(&self, from: &Path, args: &[&str]) -> (bool, String) {
        let mut cargo = Command::new(std::env::var_os("CARGO").expect("cargo test sets CARGO"));
        cargo.current_dir(from).args(args);
        let out = clean(&mut cargo)
            .env("CARGO_TARGET_DIR", self.dir.path().join("target"))
            .output()
            .unwrap();
        said(out)
    }

    /// `cargo test` of `side`'s workspace, run from `from`.
    fn test(&self, from: &Path, side: &str) -> (bool, String) {
        let manifest = self.crates(side).join("member/Cargo.toml");
        let manifest = manifest.to_str().unwrap();
        self.cargo(
            from,
            &[
                "test",
                "--offline",
                "--test",
                "reads",
                "--manifest-path",
                manifest,
                "--",
                "--nocapture",
            ],
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

/// Its config builds the workspace Cargo was given in that one's own
/// directory.
#[test]
fn a_run_from_another_checkout_builds_that_one() {
    let both = Checkouts::new();
    both.reads_itself(&both.crates("a"), "a");
    both.reads_itself(&both.crates("a"), "b");
}

#[test]
fn a_run_from_outside_refuses_another_checkouts_build() {
    let both = Checkouts::new();
    both.reads_itself(both.dir.path(), "a");
    both.refuses(both.dir.path(), "b");
}

/// The issue's check (burrow#1437): `cargo clippy` puts its own driver in
/// the workspace wrapper's place, so only a build directory of the
/// checkout's own keeps it from passing on the other's results.
#[test]
fn clippy_checks_each_checkout_itself() {
    let both = Checkouts::new();
    let lib = both.crates("b").join("member/src/lib.rs");
    let text = fs::read_to_string(&lib).unwrap() + "pub fn same(x: u8) -> bool {\n    x == x\n}\n";
    // Its own time stays older than the other's build.
    let old = fs::metadata(&lib).unwrap().modified().unwrap();
    fs::write(&lib, text).unwrap();
    fs::File::options()
        .write(true)
        .open(&lib)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let clippy = |side| both.cargo(&both.crates(side), &["clippy", "--offline", "--workspace"]);
    let (passed, said) = clippy("a");
    assert!(passed, "{said}");
    let (passed, said) = clippy("b");
    assert!(
        !passed && said.contains("clippy::eq_op"),
        "passed on a's results:\n{said}"
    );
}

/// A test that runs its program after another checkout's build put its own
/// in the shared target directory: as when the two run at once.
#[test]
fn a_test_never_runs_another_checkouts_program() {
    let both = Checkouts::new();
    let (built, said) = both.cargo(
        &both.crates("a"),
        &[
            "test",
            "--offline",
            "--no-run",
            "--test",
            "reads",
            "--message-format",
            "json",
        ],
    );
    assert!(built, "{said}");
    let test = said
        .lines()
        .find_map(|line| {
            let (_, rest) = line.split_once(r#""executable":""#)?;
            line.contains(r#""kind":["test"]"#)
                .then(|| rest.split('"').next().unwrap().to_owned())
        })
        .expect("the test executable");
    let (built, said) = both.cargo(&both.crates("b"), &["build", "--offline"]);
    assert!(built, "{said}");
    let (passed, said) = said_by(clean(&mut Command::new(test)).arg("--nocapture"));
    assert!(
        !passed
            && said.contains("isn't the program this test was built with")
            && !said.contains("bin=B"),
        "ran another checkout's program:\n{said}"
    );
}

fn said_by(command: &mut Command) -> (bool, String) {
    said(command.output().unwrap())
}

/// The files under crates/ that quote `name`.
fn quoting(name: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, name: &str, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "target" {
                    walk(&path, name, found);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && fs::read_to_string(&path)
                    .unwrap()
                    .contains(&format!("\"{name}"))
            {
                found.push(path);
            }
        }
    }
    let crates = burrow_test::manifest_dir!().parent().unwrap();
    let mut found = Vec::new();
    walk(crates, name, &mut found);
    found
}

fn checkout_rs() -> [PathBuf; 1] {
    [burrow_test::manifest_dir!().join("src/checkout.rs")]
}

/// `env!` of it anywhere else is a test that reads whichever checkout built it.
#[test]
fn tests_find_the_repo_through_manifest_dir() {
    assert_eq!(
        quoting(concat!("CARGO_", "MANIFEST_DIR\"")),
        checkout_rs(),
        "find the repo with burrow_test::manifest_dir!()"
    );
}

/// `env!` of one anywhere else is a test that can run another checkout's.
#[test]
fn tests_run_programs_through_program() {
    assert_eq!(
        quoting(concat!("CARGO_", "BIN_EXE_")),
        checkout_rs(),
        "run the crate's programs with burrow_test::program!()"
    );
}
