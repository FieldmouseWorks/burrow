//! With real nix, on a chroot store of the test's own and a binary cache in
//! a directory. Needs nix on PATH, as the flake's check has it.
use super::*;

/// A chroot store, a cache beside it, and nix's config for them.
struct Here {
    dir: PathBuf,
    _keep: tempfile::TempDir,
}

impl Here {
    fn new() -> Here {
        let keep = tempfile::tempdir().unwrap();
        Here {
            dir: keep.path().to_path_buf(),
            _keep: keep,
        }
    }

    fn join(&self, p: &str) -> PathBuf {
        self.dir.join(p)
    }

    /// A machine's own config: no substituter, no registry to fetch, and
    /// no sandbox (a test's builder may already be in one). Substituting
    /// stays on, as a runner's does: offline (a check's sandbox), nix turns
    /// it off unless it's set.
    fn config(&self) -> String {
        format!(
            "experimental-features = nix-command flakes\nstore = local?root={}\nsubstituters =\nsubstitute = true\nflake-registry =\nsandbox = false",
            self.join("root").display()
        )
    }

    fn nix(&self, config: &str, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .env("NIX_CONFIG", config)
            .env("HOME", &self.dir)
            .env("XDG_CACHE_HOME", self.join("cache-home"))
            .env_remove("NIX_USER_CONF_FILES")
            .env_remove("NIX_REMOTE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, program: &str, args: &[&str]) -> String {
        let o = self.nix(&self.config(), program, args);
        assert!(
            o.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    }

    /// A file's flat SHA-256, and its path once added: what a fixed-output
    /// derivation of it makes, which the cache then holds, and the store
    /// doesn't.
    fn cached(&self, name: &str, text: &str) -> (String, String) {
        let file = self.join(name);
        fs::write(&file, text).unwrap();
        let hash = self.ok(
            "nix",
            &["hash", "file", "--type", "sha256", "--base32", s(&file)],
        );
        let path = self.ok("nix", &["store", "add-file", s(&file)]);
        let to = format!("file://{}", self.join("cache").display());
        self.ok("nix", &["copy", "--to", &to, &path]);
        self.ok("nix", &["store", "delete", &path]);
        (hash, path)
    }
}

fn fixed(name: &str, hash: &str) -> String {
    format!(
        r#"derivation {{ name = "{name}"; system = "x86_64-linux"; builder = "/bin/sh"; outputHashMode = "flat"; outputHashAlgo = "sha256"; outputHash = "{hash}"; }}"#
    )
}

/// A run that writes the store builds as the machine's config says, never
/// as the flake's: a flake's `nixConfig` that adds a substituter (one
/// nothing signs for, trusted) and turns the sandbox off is refused, even
/// on a runner whose own config takes a flake's. A run that doesn't write
/// is as it was.
#[test]
fn a_flakes_own_config_cant_add_a_substituter_or_drop_the_sandbox_in_a_run_that_writes() {
    let h = Here::new();
    let (hash, path) = h.cached("x", "built elsewhere\n");
    let flake = h.join("flake");
    fs::create_dir_all(&flake).unwrap();
    fs::write(
        flake.join("flake.nix"),
        format!(
            r#"{{
  nixConfig = {{ sandbox = false; extra-substituters = [ "file://{}?trusted=true" ]; }};
  outputs = {{ self }}: {{ a = {}; }};
}}"#,
            h.join("cache").display(),
            fixed("x", &hash)
        ),
    )
    .unwrap();
    let flake = format!("path:{}#a", flake.display());
    let runner = format!("{}\naccept-flake-config = true", h.config());
    let dry = |writes: bool| {
        let o = h.nix(
            &crate::worker_config(&runner, writes),
            "nix",
            &["build", "--dry-run", "--no-link", &flake],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).into_owned()
    };
    let writing = dry(true);
    assert!(
        writing.contains("will be built") && !writing.contains(&path),
        "a writing run took the flake's substituter:\n{writing}"
    );
    for setting in ["sandbox", "extra-substituters"] {
        assert!(
            writing.contains(&format!(
                "ignoring untrusted flake configuration setting '{setting}'"
            )),
            "{writing}"
        );
    }
    // Without the store, the runner's own config stands, as before.
    let reading = dry(false);
    assert!(
        reading.contains("will be fetched") && reading.contains(&path),
        "{reading}"
    );
}

/// With no store named, the worker's config is what it always was.
#[test]
fn a_run_with_no_store_has_the_config_it_had() {
    assert_eq!(
        crate::worker_config("a = b", false),
        format!("a = b\nmax-silent-time = {}", crate::SILENT_SECONDS)
    );
    assert!(crate::worker_config("a = b", true).ends_with(NIX_CONFIG));
    assert_eq!(name("/nix/store/aaaa-codex-1.0.drv"), "codex-1.0");
}
