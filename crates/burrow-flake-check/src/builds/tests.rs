//! With real nix, on a chroot store of the test's own and a binary cache in
//! a directory, served over HTTP as the build store is. Needs nix, jq, curl
//! and coreutils on PATH, as the flake's check has them.
use super::*;
use std::net::TcpListener;

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

    /// The cache, served over HTTP as the build store is.
    fn store(&self) -> String {
        burrow_test::build_store(self.join("cache"))
    }

    /// The derivation this expression is, instantiated in the store.
    fn drv(&self, expr: &str) -> String {
        self.ok("nix-instantiate", &["--expr", expr])
    }
}

fn fixed(name: &str, hash: &str) -> String {
    format!(
        r#"derivation {{ name = "{name}"; system = "x86_64-linux"; builder = "/bin/sh"; outputHashMode = "flat"; outputHashAlgo = "sha256"; outputHash = "{hash}"; }}"#
    )
}

fn built(name: &str) -> String {
    format!(
        r#"derivation {{ name = "{name}"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo {name} > $out" ]; }}"#
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
}

/// Three cold derivations, by name: one whose output the cache holds (a
/// fixed output, which needs no signature), one whose output it holds
/// unsigned, and one it hasn't; and the unsigned one's output.
fn three(h: &Here) -> ([String; 3], String) {
    let (hash, _) = h.cached("x", "built elsewhere\n");
    let served = h.drv(&fixed("x", &hash));
    let unsigned = h.drv(&built("unsigned"));
    let missing = h.drv(&built("missing"));
    // The cache has the unsigned one's output, signed by no one.
    let out = h.ok("nix", &["derivation", "show", &unsigned]);
    let out = jq(
        out.as_bytes(),
        r#"(.derivations // .) | to_entries[0].value.outputs.out.path | sub(".*/"; "")"#,
    )
    .unwrap();
    let hash_part = &out[..32];
    fs::write(
        h.join(&format!("cache/{hash_part}.narinfo")),
        format!(
            "StorePath: /nix/store/{out}\nURL: nar/none.nar\nCompression: none\nNarHash: sha256:{}\nNarSize: 1\nReferences: \n",
            "0".repeat(52)
        ),
    )
    .unwrap();
    ([served, unsigned, missing], out)
}

/// What the store has of the cold derivations, as a store that trusts only
/// its key would take them: one it holds (a fixed output, which needs no
/// signature), one it holds unsigned (which doesn't count), and one it
/// hasn't; how long fetching takes, timed; and the kept builds' restore
/// beside it. The report's last line is JSON.
#[test]
fn a_leg_says_what_the_store_has_of_what_it_builds_cold() {
    let h = Here::new();
    let ([served, unsigned, missing], out) = three(&h);
    let (sk, pk) = (h.join("sk"), h.join("pk"));
    h.ok(
        "nix-store",
        &["--generate-binary-cache-key", "machine-1", s(&sk), s(&pk)],
    );
    // The kept builds' restore, as the workflow's clock left it.
    fs::create_dir_all(h.join("kept")).unwrap();
    fs::write(h.join("kept/one.nar"), vec![0; 4096]).unwrap();
    fs::write(h.join("builds-restore-start"), "1000000000000\n").unwrap();
    fs::write(h.join("builds-restore-end"), "1002500000000\n").unwrap();
    let store = Store {
        url: h.store(),
        key: fs::read_to_string(&pk).unwrap().trim().to_owned(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: DEADLINE,
    };
    let r = store.ask(&[served, unsigned, missing]).unwrap();
    assert_eq!(r.served, ["x"]);
    assert_eq!(r.unserved, ["missing", "unsigned"]);
    assert_eq!(r.untrusted, 1);
    assert!(r.bytes > 0);
    let (b, secs) = r.sample.expect("a timed fetch");
    assert!(b == r.bytes && secs >= 0.0, "{:?}", r.sample);
    let (t, kept) = r.restore.unwrap();
    assert!((t - 2.5).abs() < 1e-9 && kept >= 4096, "{:?}", r.restore);
    let lines = r.lines();
    assert!(
        lines[0].contains("it has 1 of the 3 derivations this leg builds cold")
            && lines[0]
                .ends_with(": x; 1 more paths it has aren't signed by its key, and don't count"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("fetching them would take about")
            && lines[1].contains("the kept builds' restore took 2s"),
        "{lines:?}"
    );
    let json = lines[2].strip_prefix("builds-shadow ").unwrap();
    assert_eq!(
        jq(
            json.as_bytes(),
            "[.cold, .served, .untrusted, .served_names, .unserved_names] | tostring"
        )
        .unwrap(),
        r#"[3,1,1,["x"],["missing","unsigned"]]"#
    );
    // Nothing went into the run's own store.
    assert!(!h.join(&format!("root/nix/store/{out}")).exists());
}

/// With no public key named, a store trusts no signature: what it holds
/// unsigned doesn't count, and what needs none (a fixed output) does.
#[test]
fn with_no_key_named_only_what_needs_no_signature_counts() {
    let h = Here::new();
    let (cold, _) = three(&h);
    let store = Store {
        url: h.store(),
        key: String::new(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: DEADLINE,
    };
    let r = store.ask(&cold).unwrap();
    assert_eq!(r.served, ["x"]);
    assert_eq!(r.unserved, ["missing", "unsigned"]);
    assert_eq!(r.untrusted, 1);
}

/// An HTTP store that has none of the outputs of what a leg builds cold,
/// which the run's own store hasn't either, says so: none, not an error.
/// (Asked with `nix path-info`, nix 2.35 over HTTP said "don't know how to
/// build these paths" and failed: burrow#1223.)
#[test]
fn an_http_store_with_none_of_the_outputs_says_none() {
    let h = Here::new();
    fs::create_dir_all(h.join("cache")).unwrap();
    fs::write(h.join("cache/nix-cache-info"), "StoreDir: /nix/store\n").unwrap();
    let cold = [h.drv(&built("one")), h.drv(&built("two"))];
    let store = Store {
        url: h.store(),
        key: String::new(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: DEADLINE,
    };
    let r = store.ask(&cold).unwrap();
    assert!(r.served.is_empty() && r.bytes == 0, "{:?}", r.served);
    assert_eq!(r.unserved, ["one", "two"]);
    assert!(
        r.lines()[0].contains("it has 0 of the 2 derivations this leg builds cold"),
        "{:?}",
        r.lines()
    );
}

/// A store that isn't there is said, never a failure: the leg builds as
/// it would have.
#[test]
fn a_store_that_doesnt_answer_is_an_error_to_say() {
    let h = Here::new();
    let drv = h.drv(&built("missing"));
    let closed = burrow_test::ClosedPort::new();
    let url = format!("http://{}/builds", closed.addr());
    let store = Store {
        url,
        key: String::new(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: DEADLINE,
    };
    assert!(store.ask(&[drv]).is_err());
}

/// A store that takes its connections and never answers holds a leg for
/// one deadline, however many outputs it's asked about: curl's own
/// `--max-time` is each transfer's, so the batch has a deadline of its own.
#[test]
fn a_store_that_stalls_holds_a_leg_one_deadline_not_one_per_output() {
    let h = Here::new();
    let cold = [
        h.drv(&built("one")),
        h.drv(&built("two")),
        h.drv(&built("three")),
    ];
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/builds", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for c in l.incoming() {
            held.extend(c.ok());
        }
    });
    let store = Store {
        url,
        key: String::new(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: "2",
    };
    let start = std::time::Instant::now();
    let e = store.ask(&cold).unwrap_err().to_string();
    let took = start.elapsed();
    assert!(
        took < std::time::Duration::from_secs(4),
        "three stalled outputs took {took:?}"
    );
    assert!(e.contains("past the 2s deadline"), "{e}");
}

/// A slow HTTP store answers each request after `delay`, tracking peak
/// in-flight concurrency and every path requested.
fn slow_store(
    dir: PathBuf,
    delay: std::time::Duration,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use std::io::{BufRead, BufReader};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/builds", l.local_addr().unwrap());
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (peak_t, seen_t) = (Arc::clone(&peak), Arc::clone(&seen));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let (dir, active, peak, seen) = (
                dir.clone(),
                Arc::clone(&active),
                Arc::clone(&peak_t),
                Arc::clone(&seen_t),
            );
            std::thread::spawn(move || {
                let Ok(read) = c.try_clone() else { return };
                let mut r = BufReader::new(read);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut w = line.split_whitespace();
                let (method, target) = (w.next().unwrap_or(""), w.next().unwrap_or(""));
                let mut h = String::new();
                while r.read_line(&mut h).is_ok_and(|n| n > 0) && h != "\r\n" {
                    h.clear();
                }
                seen.lock().unwrap().push(target.to_owned());
                let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);
                std::thread::sleep(delay);
                active.fetch_sub(1, Ordering::SeqCst);
                let file = target
                    .strip_prefix("/builds/")
                    .filter(|f| !f.split('/').any(|p| p == ".." || p.is_empty()))
                    .and_then(|f| fs::read(dir.join(f)).ok());
                let (status, body) = match file {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", Vec::new()),
                };
                let _ = write!(
                    c,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if method != "HEAD" {
                    let _ = c.write_all(&body);
                }
            });
        }
    });
    (url, peak, seen)
}

/// Many cold paths against a store with per-request latency finish well
/// inside the deadline because narinfos are fetched concurrently (bounded)
/// and verified offline from the narinfos already fetched: signed outputs
/// and fixed outputs count, unsigned or differently signed ones don't.
#[test]
fn a_slow_store_fetches_narinfos_concurrently_within_its_deadline() {
    let h = Here::new();
    let (sk, pk) = (h.join("sk"), h.join("pk"));
    let (other_sk, other_pk) = (h.join("other-sk"), h.join("other-pk"));
    h.ok(
        "nix-store",
        &["--generate-binary-cache-key", "machine-1", s(&sk), s(&pk)],
    );
    h.ok(
        "nix-store",
        &[
            "--generate-binary-cache-key",
            "other-1",
            s(&other_sk),
            s(&other_pk),
        ],
    );
    let to = format!("file://{}", h.join("cache").display());
    let cache_signed = |name: &str, key: &Path| {
        let drv = h.drv(&built(name));
        let out = h.ok("nix-store", &["--realise", &drv]);
        h.ok("nix", &["store", "sign", "--key-file", s(key), &out]);
        h.ok("nix", &["copy", "--to", &to, &out]);
        h.ok("nix", &["store", "delete", &out]);
        drv
    };
    let signed = cache_signed("signed", &sk);
    let wrong_key = cache_signed("wrong-key", &other_sk);
    let (hash, _) = h.cached("fixed", "fixed output\n");
    let fixed_drv = h.drv(&fixed("fixed", &hash));
    let unsigned = h.drv(&built("unsigned"));
    let unsigned_out = h.ok("nix-store", &["--query", "--outputs", &unsigned]);
    let unsigned_base = unsigned_out.rsplit('/').next().unwrap();
    fs::write(
        h.join(&format!("cache/{}.narinfo", &unsigned_base[..32])),
        format!(
            "StorePath: {unsigned_out}\nURL: nar/none.nar\nCompression: none\nNarHash: sha256:{}\nNarSize: 1\nReferences: \n",
            "0".repeat(52)
        ),
    )
    .unwrap();
    let missing_expr = r#"builtins.genList (i: derivation { name = "miss-${toString i}"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo ${toString i} > $out" ]; }) 36"#;
    let missing: Vec<String> = h
        .ok("nix-instantiate", &["--expr", missing_expr])
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(missing.len(), 36);
    let mut cold = vec![signed, wrong_key, fixed_drv, unsigned];
    cold.extend(missing);
    assert_eq!(cold.len(), 40);

    // 40 requests * 200ms = 8s serially, well past the 4s deadline.
    let (url, peak, seen) = slow_store(h.join("cache"), std::time::Duration::from_millis(200));
    let store = Store {
        url,
        key: fs::read_to_string(&pk).unwrap().trim().to_owned(),
        temp: h.dir.clone(),
        nix_config: Some(h.config()),
        deadline: "4",
    };
    let r = store.ask(&cold).unwrap();
    assert_eq!(r.served, ["fixed", "signed"]);
    assert_eq!(r.untrusted, 2);
    assert_eq!(r.unserved.len(), 38);
    assert!(r.bytes > 0 && r.sample.is_some());

    let max_in_flight = peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        max_in_flight > 1 && max_in_flight <= 32,
        "expected bounded concurrent fetches (2..=32), saw {max_in_flight}"
    );
    let narinfo_reqs: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.ends_with(".narinfo"))
        .cloned()
        .collect();
    assert_eq!(
        narinfo_reqs.len(),
        40,
        "expected each narinfo fetched once (no second fetch during verify), got {}",
        narinfo_reqs.len()
    );
}

#[test]
fn the_report_says_none_and_unknowns_plainly() {
    let r = Report {
        unserved: vec!["a".into()],
        ..Report::default()
    };
    let lines = r.lines();
    assert!(
        lines[0].contains("it has 0 of the 1 derivations"),
        "{lines:?}"
    );
    assert_eq!(
        lines[1],
        "build store: nothing to fetch; no kept builds were restored"
    );
    let r = Report {
        served: vec!["a".into()],
        bytes: 100 << 20,
        sample: Some((50 << 20, 2.0)),
        ..Report::default()
    };
    assert_eq!(r.fetch_seconds(), Some(4.0));
    assert!(r.lines()[1].contains("about 4s (estimated from 50 MiB fetched in 2.0s)"));
    assert_eq!(name("/nix/store/aaaa-codex-1.0.drv"), "codex-1.0");
}
