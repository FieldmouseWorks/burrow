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

#[test]
fn scanner_distinguishes_upstream_build_store_and_builder_errors() {
    let active = vec![
        "https://cache.nixos.org/".to_owned(),
        "https://builds.example/builds?priority=40".to_owned(),
    ];
    let builds = "https://builds.example/builds";

    // 1. Transient cache.nixos.org timeout -> fatal, attributes cache.nixos.org only.
    let mut s = FailureScan::new(&active);
    for line in [
        "disabling binary cache 'https://cache.nixos.org' for 60 seconds",
        "error: unable to download 'https://cache.nixos.org/nar/abc.nar.zst': Timeout was reached (28) Connection timed out after 15000 milliseconds",
        "       … substituter 'https://cache.nixos.org' is temporarily disabled because of this error we received before",
        "error: path '/nix/store/aaa-file' is required, but there is no substituter that can build it",
    ] {
        s.feed_line(line);
    }
    assert!(s.fatal);
    assert_eq!(s.failed, ["https://cache.nixos.org"]);

    // 2. Corrupt/truncated NAR from build store (no URL on error line) -> attributed via copying path.
    let mut s = FailureScan::new(&active);
    for line in [
        "copying path '/nix/store/bbb-cold' from 'https://builds.example/builds'...",
        "\u{1b}[31;1merror:\u{1b}[0m input compression not recognized",
        "error: path '/nix/store/bbb-cold' is required, but there is no substituter that can build it",
        "error: some substitutes for the outputs of derivation '/nix/store/bbb-cold.drv' failed (usually happens due to networking issues); try '--fallback' to build derivation from source",
    ] {
        s.feed_line(line);
    }
    assert!(s.fatal);
    assert_eq!(s.failed, [builds]);

    // 3. Harmless narinfo warning on dead build store + genuine check failure -> NOT fatal.
    let mut s = FailureScan::new(&active);
    for line in [
        "warning: unable to download 'https://builds.example/builds/nix-cache-info': Could not connect to server",
        "building '/nix/store/ccc-check.drv'...",
        "check> error: unable to download 'https://cache.nixos.org/fake'",
        "       >        error: unable to download 'https://git.savannah.gnu.org/cgit/config.git/plain/config.sub': Timeout was reached (28)",
        "error: Cannot build '/nix/store/ccc-check.drv'.",
    ] {
        s.feed_line(line);
    }
    assert!(!s.fatal);

    // 4. Lookalike host prefix is never attributed to configured substituters.
    let mut s = FailureScan::new(&active);
    s.feed_line("error: unable to download 'https://builds.example/builds.other/abc.narinfo': HTTP error 503");
    assert!(!s.fatal);
    assert!(s.failed.is_empty());
}

#[test]
fn retry_policy_drops_build_store_retries_upstream_and_breaks_on_outage() {
    let active = vec![
        "https://cache.nixos.org/".to_owned(),
        "https://builds.example/builds".to_owned(),
    ];
    let builds = "https://builds.example/builds";

    // A. Build store fails -> dropped immediately (no sleep), upstream kept, retry succeeds.
    let mut attempts = 0usize;
    let mut dropped = Vec::new();
    let mut sleeps = Vec::new();
    let ok = run_nix_with(
        "nix",
        builds,
        &[3, 10, 25],
        |d| sleeps.push(d),
        || {
            attempts += 1;
            if attempts == 1 {
                let mut scan = FailureScan::new(&active);
                scan.feed_line("copying path '/nix/store/x' from 'https://builds.example/builds'...");
                scan.feed_line("error: path '/nix/store/x' is required, but there is no substituter that can build it");
                Ok((1, active.clone(), scan))
            } else {
                Ok((0, vec!["https://cache.nixos.org/".to_owned()], FailureScan::default()))
            }
        },
        |u, a| {
            if !dropped.is_empty() {
                return false;
            }
            dropped.push(drop_override(u, a));
            true
        },
    )
    .unwrap();
    assert!(ok);
    assert_eq!(attempts, 2);
    assert!(sleeps.is_empty());
    assert_eq!(
        dropped,
        ["substituters = https://cache.nixos.org/\nextra-substituters ="]
    );

    // B. Upstream cache fails transiently once -> NOT dropped, sleeps 3s, retry succeeds.
    let mut attempts = 0usize;
    let mut dropped = false;
    let mut sleeps = Vec::new();
    let ok = run_nix_with(
        "nix",
        builds,
        &[3, 10, 25],
        |d| sleeps.push(d),
        || {
            attempts += 1;
            if attempts == 1 {
                let mut scan = FailureScan::new(&active);
                scan.feed_line("disabling binary cache 'https://cache.nixos.org' for 60 seconds");
                scan.feed_line("error: unable to download 'https://cache.nixos.org/nar/x.nar.zst': Timeout was reached (28)");
                Ok((1, active.clone(), scan))
            } else {
                Ok((0, active.clone(), FailureScan::default()))
            }
        },
        |_, _| {
            dropped = true;
            true
        },
    )
    .unwrap();
    assert!(ok);
    assert_eq!(attempts, 2);
    assert!(!dropped);
    assert_eq!(sleeps, [3]);

    // C. Upstream cache stays down across all retries -> returns Err (gate error, not check fail).
    let mut attempts = 0usize;
    let mut sleeps = Vec::new();
    let err = run_nix_with(
        "nix",
        builds,
        &[3, 10, 25],
        |d| sleeps.push(d),
        || {
            attempts += 1;
            let mut scan = FailureScan::new(&active);
            scan.feed_line(
                "error: unable to download 'https://cache.nixos.org/nar/x.nar.zst': HTTP error 503",
            );
            Ok((1, active.clone(), scan))
        },
        |_, _| panic!("must not drop upstream cache"),
    )
    .unwrap_err();
    assert_eq!(attempts, 4);
    assert_eq!(sleeps, [3, 10, 25]);
    assert!(err.to_string().contains("https://cache.nixos.org"), "{err}");
    assert_eq!(crate::verdict::worker(Err(err)), crate::verdict::BROKEN);

    // D. Normal build failure -> returns Ok(false) immediately with no retry.
    let mut attempts = 0usize;
    let ok = run_nix_with(
        "nix",
        builds,
        &[3, 10, 25],
        |_| panic!("must not sleep on normal failure"),
        || {
            attempts += 1;
            Ok((1, active.clone(), FailureScan::new(&active)))
        },
        |_, _| panic!("must not drop on normal failure"),
    )
    .unwrap();
    assert!(!ok);
    assert_eq!(attempts, 1);
}
