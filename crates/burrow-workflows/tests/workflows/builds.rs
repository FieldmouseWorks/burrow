//! The build store in flake-check.yml (docs/gate-design.md, stage 1): with
//! the caller's Actions variables naming a store (`BURROW_BUILDS_URL` and
//! `BURROW_BUILDS_PUBLIC_KEY`), every job that installs Nix probes it
//! (`soft/builds`, raising `degraded/builds` and building cold if
//! unreachable) and names it as a substituter with its public key from its
//! first step, with no restore step; fork PRs substitute too (the store is
//! public) and write nothing; each same-repo leg uploads what it built in
//! `soft/builds-write`; and the attested predicate's `gate` names the store.
//! Kept builds (`actions/cache`, `flake-kept-`, `keep-builds`) are gone.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use yaml_serde::Value;

use crate::expr::{Ctx, V, value};
use crate::harness::{Scratch, read, repo_file, script, step, steps, text, workflow, write};

const URL: &str = "https://builds.example/builds";
const KEY: &str = "machine-1:K";
const MAIN: &str = "refs/heads/main";
const PR: &str = "refs/pull/7/merge";

fn wf() -> Value {
    workflow(".github/workflows/flake-check.yml")
}

fn label(s: &Value) -> String {
    s["id"]
        .as_str()
        .or_else(|| s["name"].as_str())
        .or_else(|| s["uses"].as_str().and_then(|u| u.split('@').next()))
        .or_else(|| s["run"].as_str())
        .unwrap_or("?")
        .to_owned()
}

/// The steps that run, as a runner works their `if`s out on `c`: each that
/// runs ends as `outcomes` says, or succeeds.
fn ran(job: &Value, c: &Ctx, outcomes: &BTreeMap<&str, &str>) -> Vec<String> {
    let mut c = c.clone();
    let mut out = Vec::new();
    for s in steps(job) {
        let runs = match &s["if"] {
            Value::Null => true,
            Value::String(cond) if cond.trim_start().starts_with("${{") => value(cond, &c).truthy(),
            Value::String(cond) => value(&format!("${{{{ {cond} }}}}"), &c).truthy(),
            other => panic!("{other:?}"),
        };
        if let Some(id) = s["id"].as_str() {
            let o = if runs {
                outcomes.get(id).copied().unwrap_or("success")
            } else {
                "skipped"
            };
            c.insert(format!("steps.{id}.outcome"), o.into());
        }
        if runs {
            out.push(label(s));
        }
    }
    out
}

/// Every output a step's `if` reads, empty: as when the step said nothing.
fn quiet(job: &Value, c: &mut Ctx) {
    for s in steps(job) {
        let t = serde_json::to_string(&s["if"]).unwrap();
        for part in t.split("steps.").skip(1) {
            let key: String = part
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(*ch))
                .collect();
            if key.contains(".outputs.") {
                c.entry(format!("steps.{key}")).or_insert(V::Null);
            }
        }
    }
}

/// A run's context for any job of flake-check.yml.
fn ctx_for(
    job: &Value,
    event: &str,
    git_ref: &str,
    head: Option<&str>,
    nix: bool,
    url: &str,
    key: &str,
) -> Ctx {
    let mut c: Ctx = [
        ("cancelled()", V::from(false)),
        ("always()", true.into()),
        ("inputs.install-nix", nix.into()),
        ("inputs.legs", V::from(1)),
        ("inputs.gate-reuse", false.into()),
        ("inputs.flaky", false.into()),
        ("inputs.docs-checks", "guidance".into()),
        ("github.event_name", event.into()),
        ("github.ref", git_ref.into()),
        ("github.repository", "owner/repo".into()),
        ("github.event.repository.default_branch", "main".into()),
        ("github.event.repository.private", false.into()),
        (
            "github.event.pull_request.head.repo.full_name",
            head.map_or(V::Null, V::from),
        ),
        ("needs.reuse.outputs.hit", V::Null),
        ("vars.BURROW_BUILDS_URL", url.into()),
        ("vars.BURROW_BUILDS_PUBLIC_KEY", key.into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    quiet(job, &mut c);
    c
}

/// Rendered non-empty lines of a job's `cachix/install-nix-action`'s
/// `extra_nix_config`.
fn nix_config_lines(job: &Value, c: &Ctx) -> Vec<String> {
    let install = steps(job)
        .iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|u| u.starts_with("cachix/install-nix-action@"))
        })
        .expect("install-nix-action step");
    value(text(&install["with"]["extra_nix_config"]), c)
        .text()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every job that installs Nix (`reuse`, `check`, `docs`, `negative`)
/// probes the store before Nix starts and configures it as a substituter
/// with its public key and `fallback = true`, with no restore step; and
/// with either variable unset (or `install-nix` off), none of it runs.
#[test]
fn every_job_that_installs_nix_substitutes_from_the_store_with_no_restore_step() {
    let wf = wf();
    let nix_jobs: Vec<&str> = wf["jobs"]
        .as_mapping()
        .unwrap()
        .iter()
        .filter(|(_, body)| {
            !body["steps"].is_null()
                && steps(body).iter().any(|s| {
                    s["uses"]
                        .as_str()
                        .is_some_and(|u| u.starts_with("cachix/install-nix-action@"))
                })
        })
        .map(|(name, _)| text(name))
        .collect();
    assert_eq!(nix_jobs, ["reuse", "check", "docs", "negative"]);

    for name in nix_jobs {
        let job = &wf["jobs"][name];
        let all = steps(job);
        let at_probe = all.iter().position(|s| s["id"] == "builds").expect(name);
        let at_degraded = all
            .iter()
            .position(|s| s["name"] == "degraded/builds")
            .expect(name);
        let at_install = all
            .iter()
            .position(|s| {
                s["uses"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("cachix/install-nix-action@"))
            })
            .expect(name);
        assert!(at_probe < at_degraded && at_degraded < at_install, "{name}");

        let mut c = ctx_for(job, "pull_request", PR, Some("owner/repo"), true, URL, KEY);
        let did = ran(job, &c, &BTreeMap::new());
        assert!(did.iter().any(|l| l == "builds"), "{name}: {did:?}");
        assert!(
            !did.iter().any(|l| l == "degraded/builds"),
            "{name}: {did:?}"
        );
        c.insert("steps.builds.outcome".into(), "success".into());
        assert_eq!(
            nix_config_lines(job, &c),
            [
                format!("extra-substituters = {URL}"),
                format!("extra-trusted-public-keys = {KEY}"),
                "fallback = true".to_owned(),
            ],
            "{name}"
        );

        // Either variable empty: no probe, no substituter, no upload.
        for (u, k) in [("", ""), (URL, ""), ("", KEY)] {
            let mut off = ctx_for(job, "pull_request", PR, Some("owner/repo"), true, u, k);
            let did = ran(job, &off, &BTreeMap::new());
            assert!(!did.iter().any(|l| l == "builds"), "{name} {u} {k}");
            assert!(!did.iter().any(|l| l == "builds-write"), "{name} {u} {k}");
            off.insert("steps.builds.outcome".into(), "skipped".into());
            assert!(nix_config_lines(job, &off).is_empty(), "{name} {u} {k}");
        }

        // install-nix off (self-hosted runner): neither probe nor Nix install.
        let no_nix = ctx_for(job, "push", MAIN, None, false, URL, KEY);
        let did = ran(job, &no_nix, &BTreeMap::new());
        assert!(
            !did.iter()
                .any(|l| l == "builds" || l == "cachix/install-nix-action"),
            "{name}: {did:?}"
        );
    }
}

/// A PR from a fork substitutes from the public store (both a leg and the
/// negative) and writes nothing; a same-repo PR, default-branch push, or
/// train candidate uploads after its gate ran (passed or failed) on a
/// runner whose store is its own, never when cancelled or on a self-hosted
/// runner.
#[test]
fn a_pr_from_a_fork_substitutes_too_and_writes_nothing() {
    let wf = wf();
    let check = &wf["jobs"]["check"];
    let negative = &wf["jobs"]["negative"];

    for head in [Some("someone/repo"), None] {
        for (name, job) in [("check", check), ("negative", negative)] {
            let mut c = ctx_for(job, "pull_request", PR, head, true, URL, KEY);
            let did = ran(job, &c, &BTreeMap::new());
            assert!(
                did.iter().any(|l| l == "builds"),
                "{name} {head:?}: {did:?}"
            );
            c.insert("steps.builds.outcome".into(), "success".into());
            assert_eq!(
                nix_config_lines(job, &c),
                [
                    format!("extra-substituters = {URL}"),
                    format!("extra-trusted-public-keys = {KEY}"),
                    "fallback = true".to_owned(),
                ],
                "{name} {head:?}"
            );
            assert!(
                !did.iter().any(|l| l == "builds-write"),
                "{name} {head:?} wrote to the store: {did:?}"
            );
        }
    }

    // Same-repo runs upload once the gate ran (passed or failed) with install-nix on.
    for (event, git_ref, head) in [
        ("push", MAIN, None),
        ("push", "refs/heads/conary/train/1", None),
        ("pull_request", PR, Some("owner/repo")),
    ] {
        for nix in [true, false] {
            let c = ctx_for(check, event, git_ref, head, nix, URL, KEY);
            for gate in ["success", "failure", "cancelled", "skipped"] {
                let ran_gate = gate == "success" || gate == "failure";
                let outcomes = BTreeMap::from([("gate", gate)]);
                let did = ran(check, &c, &outcomes);
                assert_eq!(
                    did.iter().any(|l| l == "builds-write"),
                    nix && ran_gate,
                    "{event} {git_ref} nix={nix} gate={gate}: {did:?}"
                );
            }
            let mut cancelled = c.clone();
            cancelled.insert("cancelled()".into(), true.into());
            let did = ran(check, &cancelled, &BTreeMap::new());
            assert!(
                !did.iter().any(|l| l == "builds-write"),
                "{event} cancelled"
            );
        }
    }

    let write = step(check, "builds-write");
    assert_eq!(write["working-directory"], ".flake-check/crates");
    assert_eq!(write["env"]["BUILDS"], "${{ vars.BURROW_BUILDS_URL }}");
    assert!(
        write["continue-on-error"] == true
            && write["timeout-minutes"].as_u64().is_some_and(|m| m <= 60),
        "{write:?}"
    );
    assert_eq!(
        script(write),
        "cargo build --release --locked --quiet -p burrow-keep --bin burrow-keep-upload\n\
         \"$CARGO_TARGET_DIR/release/burrow-keep-upload\" \"$BUILDS\"\n"
    );
    assert!(repo_file("crates/burrow-keep/src/bin/burrow-keep-upload.rs").is_file());
}

/// When the store is unreachable (or does not answer as a Nix binary cache),
/// `soft/builds` fails, `degraded/builds` reports the degradation in the
/// step summary, and `install-nix-action` omits the dead substituter so the
/// job builds cold.
#[test]
fn the_store_down_raises_degraded_builds_and_omits_the_substituter() {
    let wf = wf();
    let check = &wf["jobs"]["check"];
    let probe = step(check, "builds");
    assert_eq!(probe["continue-on-error"], true);
    assert_eq!(probe["name"], "soft/builds");

    let s = Scratch::default();
    // 1. Healthy binary cache answers nix-cache-info with StoreDir: -> probe succeeds.
    let cache = s.join("cache");
    fs::create_dir_all(&cache).unwrap();
    write(
        &cache.join("nix-cache-info"),
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n",
    );
    let healthy_url = burrow_test::build_store(&cache);
    let ok = s.bash(script(probe), s.path(), &[("BUILDS", &healthy_url)]);
    assert_eq!(ok.code, 0, "{}", ok.said());

    // 2. Closed port -> probe fails.
    let closed = burrow_test::ClosedPort::new();
    let down_url = format!("http://{}/builds", closed.addr());
    let down = s.bash(script(probe), s.path(), &[("BUILDS", &down_url)]);
    assert_ne!(down.code, 0, "closed port passed probe: {}", down.said());

    // 3. HTTP 200 error page without StoreDir: -> probe fails.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bogus_url = format!("http://{}/builds", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut r = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let body = "<html>not a cache</html>\n";
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let bogus = s.bash(script(probe), s.path(), &[("BUILDS", &bogus_url)]);
    assert_ne!(
        bogus.code,
        0,
        "non-cache 200 passed probe: {}",
        bogus.said()
    );

    // When `builds` fails, every Nix-installing job runs `degraded/builds`
    // and omits the dead store from `extra_nix_config`.
    let failed = BTreeMap::from([("builds", "failure")]);
    for name in ["reuse", "check", "docs", "negative"] {
        let job = &wf["jobs"][name];
        let mut c = ctx_for(job, "pull_request", PR, Some("owner/repo"), true, URL, KEY);
        let did = ran(job, &c, &failed);
        assert!(
            did.iter().any(|l| l == "degraded/builds"),
            "{name}: {did:?}"
        );
        c.insert("steps.builds.outcome".into(), "failure".into());
        assert!(
            nix_config_lines(job, &c).is_empty(),
            "{name} kept dead substituter"
        );
    }

    let reporter = steps(check)
        .iter()
        .find(|s| s["name"] == "degraded/builds")
        .unwrap();
    let summary = s.join("summary");
    let reported = s.bash(
        script(reporter),
        s.path(),
        &[("GITHUB_STEP_SUMMARY", summary.to_str().unwrap())],
    );
    assert_eq!(reported.code, 0, "{}", reported.said());
    assert_eq!(read(&summary), "degraded/builds\n");
}

/// With a build store configured (even with `gate-reuse` off), `identity`
/// runs in `check` and `gate` (with `tools` in `gate`), and the attested
/// predicate's `gate` field names the store's URL and public key.
#[test]
fn the_predicates_gate_names_the_store() {
    let wf = wf();
    let (check, gate) = (&wf["jobs"]["check"], &wf["jobs"]["gate"]);

    // With store variables set and install-nix on (gate-reuse off):
    // identity runs in both check and gate.
    for (name, job) in [("check", check), ("gate", gate)] {
        let mut c = ctx_for(job, "push", MAIN, None, true, URL, KEY);
        c.insert(
            "inputs.legs".into(),
            V::from(if name == "gate" { 4 } else { 1 }),
        );
        let did = ran(job, &c, &BTreeMap::new());
        assert!(did.iter().any(|l| l == "identity"), "{name}: {did:?}");
        if name == "gate" {
            assert!(did.iter().any(|l| l == "tools"), "{name}: {did:?}");
        }

        // With install-nix off or store unset (and gate-reuse off): no identity.
        for (nix, u, k) in [(false, URL, KEY), (true, "", ""), (true, URL, "")] {
            let mut off = ctx_for(job, "push", MAIN, None, nix, u, k);
            off.insert(
                "inputs.legs".into(),
                V::from(if name == "gate" { 4 } else { 1 }),
            );
            let did = ran(job, &off, &BTreeMap::new());
            assert!(
                !did.iter().any(|l| l == "identity"),
                "{name} nix={nix} {u} {k}: {did:?}"
            );
        }
    }

    // Run the workflow's own `identity` then `subject` steps and verify the
    // attested predicate's `gate.builds` names the store.
    let s = Scratch::default();
    s.stub(
        "curl",
        "echo '{\"count\": 1, \"value\": \"eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln\"}'\n",
    );
    let repo = s.join("repo");
    let tools = repo.join(".flake-check");
    fs::create_dir_all(&tools).unwrap();
    s.git(&repo, &["init", "-q"]);
    let sha = s.commit(&repo, "a", "a\n");
    s.git(&tools, &["init", "-q"]);
    let wf_sha = s.commit(&tools, "flake-check.yml", "v1\n");

    let temp = s.mkdtemp();
    write(&temp.join("flake.log"), "checks passed\n");
    let id = s.bash(
        script(step(check, "identity")),
        &repo,
        &[
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("WORKFLOW_REPOSITORY", "FieldmouseWorks/burrow"),
            ("WORKFLOW_FILE", ".github/workflows/flake-check.yml"),
            ("WORKFLOW_SHA", &wf_sha),
            ("RUNNER_LABEL", "ubuntu-latest"),
            ("INSTALL_NIX", "true"),
            ("APART", "true"),
            ("LEGS", "1"),
            ("SKIP", "agents"),
            ("FLAKY", "false"),
            ("REUSE", "false"),
            ("BUILDS", URL),
            ("BUILDS_KEY", KEY),
        ],
    );
    assert_eq!(id.code, 0, "{}", id.said());
    let sub = s.bash(
        script(step(check, "subject")),
        &repo,
        &[
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("GITHUB_EVENT_NAME", "push"),
            ("GITHUB_SHA", &sha),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("OUTCOME", "success"),
            ("FILED", ""),
            ("SKIP", "agents"),
            ("PR_HEAD", ""),
            ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://token.example/?a=1"),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "t"),
        ],
    );
    assert_eq!(sub.code, 0, "{}", sub.said());
    let attested = read(&temp.join("conary/conary-attestation"));
    let predicate: serde_json::Value =
        serde_json::from_str(attested.lines().nth(2).unwrap()).unwrap();
    assert_eq!(
        predicate["gate"]["builds"],
        serde_json::json!({"url": URL, "public-key": KEY}),
        "{predicate}"
    );
    assert_eq!(predicate["gate"]["source"], "commit", "{predicate}");
    assert!(predicate["gate"]["inputs"].get("keep-builds").is_none());
}

/// Guard test: kept builds (`actions/cache`, `flake-kept-`, `keep-builds`)
/// must never return to the shared workflow or its callers.
#[test]
fn no_actions_cache_or_kept_builds_in_workflows() {
    let wf = wf();
    assert!(
        wf["on"]["workflow_call"]["inputs"]
            .get("keep-builds")
            .is_none(),
        "keep-builds input returned to flake-check.yml"
    );
    let dir = repo_file(".github/workflows");
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let body = read(&path);
        for forbidden in ["actions/cache", "flake-kept-", "keep-builds"] {
            assert!(
                !body.contains(forbidden),
                "{} mentions forbidden {forbidden:?}",
                path.display()
            );
        }
    }
}

fn assert_builds_fallback_passes(s: &Scratch, fallback: &Value, log: &std::path::Path) {
    for (case, line) in [
        (
            "clean-build",
            "building '/nix/store/abc-foo.drv'...\n".to_owned(),
        ),
        (
            "unrelated-falling-back",
            "warning: error: unable to download 'https://cache.nixos.org/abc.narinfo': HTTP error 503\n\
             warning: disabling binary cache 'https://cache.nixos.org' for 60 seconds\n\
             warning: falling back to building '/nix/store/abc-foo'\n"
                .to_owned(),
        ),
        (
            "unrelated-substitution-failed",
            "warning: substitution of '/nix/store/abc-foo' failed\n\
             warning: falling back to building '/nix/store/abc-foo'\n"
                .to_owned(),
        ),
        (
            "healthy-store-with-other-cache-untrusted-and-fallback",
            format!(
                "copying path '/nix/store/aaa-good' from '{URL}'\n\
                 warning: ignoring substitute for '/nix/store/bbb-other' from 'https://cache.nixos.org', as it's not signed\n\
                 warning: falling back to building '/nix/store/bbb-other'\n"
            ),
        ),
        (
            "lookalike-host-prefix",
            format!(
                "error: unable to download '{URL}.other/abc.narinfo': HTTP error 503\n\
                 disabling binary cache '{URL}.other' for 60 seconds\n"
            ),
        ),
    ] {
        write(log, &line);
        let ok = s.bash(
            script(fallback),
            s.path(),
            &[("BUILDS", URL), ("LOG", log.to_str().unwrap())],
        );
        assert_eq!(ok.code, 0, "{case} failed builds-fallback: {}", ok.said());
    }
}

fn assert_builds_fallback_fails(s: &Scratch, fallback: &Value, log: &std::path::Path) {
    for (case, line) in [
        (
            "disabled-cache",
            format!("disabling binary cache '{URL}' for 60 seconds\n"),
        ),
        (
            "disabled-cache-trailing-slash",
            format!("disabling binary cache '{URL}/' for 60 seconds\n"),
        ),
        (
            "substituter-disabled",
            format!("substituter '{URL}' is disabled\n"),
        ),
        (
            "not-a-binary-cache",
            format!("warning: '{URL}' does not appear to be a binary cache\n"),
        ),
        (
            "unable-to-download",
            format!("error: unable to download '{URL}/abc.narinfo': HTTP error 503\n"),
        ),
        (
            "incomplete-nar",
            format!("error: NAR for '/nix/store/abc-foo' fetched from '{URL}' is incomplete\n"),
        ),
        (
            "untrusted-substitute",
            format!(
                "warning: ignoring substitute for '/nix/store/abc-foo' from '{URL}', as it's not signed\n"
            ),
        ),
        (
            "untrusted-substituter",
            format!(
                "warning: ignoring untrusted substituter '{URL}', you are not a trusted user.\n"
            ),
        ),
    ] {
        write(log, &line);
        let failed = s.bash(
            script(fallback),
            s.path(),
            &[("BUILDS", URL), ("LOG", log.to_str().unwrap())],
        );
        assert_ne!(failed.code, 0, "{case} passed builds-fallback");
    }

    // Trailing slash on BURROW_BUILDS_URL still matches Nix's normalized URL.
    write(
        log,
        &format!("disabling binary cache '{URL}' for 60 seconds\n"),
    );
    let trailing = s.bash(
        script(fallback),
        s.path(),
        &[
            ("BUILDS", &format!("{URL}/")),
            ("LOG", log.to_str().unwrap()),
        ],
    );
    assert_ne!(
        trailing.code, 0,
        "trailing-slash BUILDS passed builds-fallback"
    );
}

/// When `soft/builds` passed so Nix configured `fallback = true`, a mid-run
/// substituter failure recorded in the job's log fails `soft/builds-fallback`
/// and runs `degraded/builds-fallback`.
#[test]
fn mid_run_substituter_fallback_raises_degraded_builds_fallback() {
    let wf = wf();
    let check = &wf["jobs"]["check"];
    let fallback = step(check, "builds-fallback");
    assert_eq!(fallback["continue-on-error"], true);
    assert_eq!(fallback["name"], "soft/builds-fallback");

    let s = Scratch::default();
    let log = s.join("flake.log");
    assert_builds_fallback_passes(&s, fallback, &log);
    assert_builds_fallback_fails(&s, fallback, &log);

    // Runs in check, docs, and negative only when soft/builds succeeded and
    // the build step ran; degraded/builds-fallback runs on failure.
    for name in ["check", "docs", "negative"] {
        let job = &wf["jobs"][name];
        let mut c = ctx_for(job, "pull_request", PR, Some("owner/repo"), true, URL, KEY);
        if name == "negative" {
            c.insert("steps.paths.outputs.needs".into(), "true".into());
        }
        let did = ran(job, &c, &BTreeMap::from([("builds-fallback", "failure")]));
        assert!(
            did.iter().any(|l| l == "builds-fallback"),
            "{name}: {did:?}"
        );
        assert!(
            did.iter().any(|l| l == "degraded/builds-fallback"),
            "{name}: {did:?}"
        );

        let skipped = ran(job, &c, &BTreeMap::from([("builds", "failure")]));
        assert!(
            !skipped.iter().any(|l| l == "builds-fallback"),
            "{name} ran builds-fallback after probe failed: {skipped:?}"
        );
    }

    let reporter = steps(check)
        .iter()
        .find(|s| s["name"] == "degraded/builds-fallback")
        .unwrap();
    let summary = s.join("summary");
    let reported = s.bash(
        script(reporter),
        s.path(),
        &[("GITHUB_STEP_SUMMARY", summary.to_str().unwrap())],
    );
    assert_eq!(reported.code, 0, "{}", reported.said());
    assert_eq!(read(&summary), "degraded/builds-fallback\n");
}
