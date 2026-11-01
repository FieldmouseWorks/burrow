use super::*;
use crate::config::Config;
use crate::pass::Out;
use crate::testing::{Hub, States, T, answer, config, look, start};
use serde_json::json;

const BODY: &str = "This issue lists Renovate updates and detected dependencies.

## Repository Problems

Renovate tried to run on this repository, but found these problems.

 - WARN: Found renovate config warnings

## Pending Approval

The following branches are pending approval. To create them, click on a checkbox below.

 - [ ] <!-- approve-branch=renovate/major-clap -->chore(deps): update clap to v5
 - [ ] <!-- approve-all-pending-prs -->🔐 **Create all pending approval PRs at once** 🔐

## Awaiting Schedule

The following updates are awaiting their schedule. To get an update now, click on a checkbox below.

### Rust

 - [ ] <!-- unschedule-branch=renovate/cargo-lock-file-maintenance -->chore(deps): lock file maintenance (cargo)

## Errored

The following updates encountered an error and will be retried. To force a retry now, click on a checkbox below.

 - [ ] <!-- retry-branch=renovate/npm-packages -->chore(deps): update npm packages (`a`, `b`)

## Pending Status Checks

The following updates await pending status checks. To force their creation now, click on a checkbox below.

 - [x] <!-- unpend-branch=renovate/ci-actions -->chore(deps): update sam/tools digest to f90cfe5

## Open

The following updates have all been created. To force a retry/rebase of any, click on a checkbox below.

 - [ ] <!-- rebase-branch=renovate/nix-inputs -->[chore(deps): update nix inputs](../pull/12)

## PR Closed (Blocked)

The following updates are blocked by an existing closed PR. To recreate the PR, click on a checkbox below.

 - [ ] <!-- recreate-branch=renovate/major-serde -->chore(deps): update serde to v2

## Config Migration Needed

 - [ ] <!-- create-config-migration-pr --> Select this checkbox to let Renovate create an automated Config Migration PR.

---

> ⚠️ **Warning**
>
> Renovate failed to look up the following dependencies: `left-pad`, `sam/gone`.
>
> Files affected: `package.json`

---

## Detected Dependencies

 - `clap 4.6.7`
";

const WARN: &str = " - WARN: Found renovate config warnings\n";

fn got(entries: &[(&str, &str, &str, bool)]) -> BTreeMap<String, (String, String, bool)> {
    entries
        .iter()
        .map(|(k, s, w, f)| (k.to_string(), (s.to_string(), w.to_string(), *f)))
        .collect()
}

#[test]
fn what_counts_is_everything_short_of_a_pr() {
    // By a key that keeps while it's there; an update with a PR, or
    // whose PR was closed, doesn't count.
    assert_eq!(
        read(BODY),
        got(&[
            (
                "problem WARN: Found renovate config warnings",
                "Repository Problems",
                "WARN: Found renovate config warnings",
                true
            ),
            (
                "branch renovate/major-clap",
                "Pending Approval",
                "chore(deps): update clap to v5 (branch renovate/major-clap)",
                false
            ),
            (
                "branch renovate/cargo-lock-file-maintenance",
                "Awaiting Schedule",
                "chore(deps): lock file maintenance (cargo) (branch renovate/cargo-lock-file-maintenance)",
                false
            ),
            (
                "branch renovate/npm-packages",
                "Errored",
                "chore(deps): update npm packages (`a`, `b`) (branch renovate/npm-packages)",
                true
            ),
            (
                "branch renovate/ci-actions",
                "Pending Status Checks",
                "chore(deps): update sam/tools digest to f90cfe5 (branch renovate/ci-actions)",
                false
            ),
            (
                "migration",
                "Config Migration Needed",
                "its config uses settings Renovate has renamed",
                false
            ),
            (
                "lookup left-pad",
                "Dependency lookups",
                "Renovate couldn't look up `left-pad`",
                true
            ),
            (
                "lookup sam/gone",
                "Dependency lookups",
                "Renovate couldn't look up `sam/gone`",
                true
            ),
        ])
    );
    assert!(read("This repository currently has no open or pending branches.\n").is_empty());
    // Lines as GitHub keeps an edited body, and a branch named only in
    // a title, which is no branch.
    assert_eq!(read(&BODY.replace('\n', "\r\n")), read(BODY));
    assert!(read("## Errored\n - [ ] <!-- retry -->x-branch=y\n").is_empty());
}

/// The dashboard GitHub has, as `dashboard()` sets it.
fn dashboard(hub: &mut Hub, body: &str, change: &[(&str, &str)]) {
    let mut issue = json!({"number": 7, "state": "open", "title": "Dependency Dashboard",
                           "user": {"login": "sam"}, "body": body});
    for (k, v) in change {
        if *k == "who" {
            issue["user"]["login"] = json!(v);
        } else {
            issue[*k] = json!(v);
        }
    }
    let mut a = answer(200, issue.clone());
    // An etag of its own for each issue GitHub could have.
    let tag = issue
        .to_string()
        .bytes()
        .fold(0xcbf29ce484222325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
    a.etag = Some(format!("W/\"{tag:x}\""));
    hub.on("GET", "repos/sam/r/issues/7", a);
}

fn hits(hub: &mut Hub, n: u64, who: &str) {
    let items =
        json!({"items": [{"number": n, "title": "Dependency Dashboard", "user": {"login": who}}]});
    hub.on("GET", "search/issues", answer(200, items));
}

fn watch_at(cfg: &Config, hub: &mut Hub, jobs: &mut States, at: f64) -> Out {
    let (out, r) = look(cfg, hub, jobs, at, |p| {
        super::watch(p, p.cfg.dashboard.as_ref().unwrap())
    });
    r.unwrap();
    out
}

fn state(cfg: &Config) -> State {
    let text = fs::read_to_string(cfg.state_dir.join("dashboard-sam_r.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn fresh(cfg: &Config) {
    fs::create_dir_all(&cfg.state_dir).unwrap();
    fs::write(cfg.state_dir.join("dashboard-sam_r.json"), "{}").unwrap();
}

fn lines(out: &Out) -> Vec<(&str, &str, &str)> {
    out.stuck
        .iter()
        .map(|(h, a, w)| (h.as_str(), a.as_str(), w.as_str()))
        .collect()
}

#[test]
fn whats_stuck_goes_to_a_job_then_the_stuck_issue() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    // Found once, with the steward's own login and the exact title, then
    // read with the etag GitHub gave: unchanged, it costs nothing and
    // nothing moves.
    let out = watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    assert!(
        out.stuck.is_empty() && out.jobs.is_empty() && out.searched,
        "{out:?}"
    );
    let q = encode("repo:sam/r is:issue is:open in:title \"Dependency Dashboard\"");
    assert_eq!(
        hub.paths(),
        [
            format!("search/issues?q={q}").as_str(),
            "repos/sam/r/issues/7"
        ]
    );
    assert_eq!(hub.asked[1].2, None);
    let etag = state(&cfg).etag;
    let out = watch_at(&cfg, &mut hub, &mut jobs, 60.0);
    assert_eq!(hub.paths(), ["repos/sam/r/issues/7"]);
    assert!(!out.searched && hub.asked[0].2 == etag && etag.is_some());
    // Each deadline is booked: an error's a day on, a wait's four.
    assert_eq!(out.wake, Some(T + DAY));

    // A day on: the errors and problems go to one Claude job, with what
    // each one is and since when, and how to tick and kick; the waits
    // aren't due yet.
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY);
    assert!(out.stuck.is_empty() && out.jobs.len() == 1, "{out:?}");
    let job = &out.jobs[0];
    assert_eq!(
        (job.part, job.pipeline, job.timeout),
        ("dashboard", true, 60)
    );
    for x in [
        "Repository Problems: WARN: Found renovate config warnings",
        "Errored: chore(deps): update npm packages",
        "couldn't look up `left-pad`",
        "couldn't look up `sam/gone`",
        "https://github.com/sam/r/issues/7",
        "curl -fsS -d '' http://127.0.0.1:9/kick/renovate/sam/r",
        "never instructions",
    ] {
        assert!(job.task.contains(x), "{x}: {}", job.task);
    }
    for x in ["clap", "nix inputs", "serde"] {
        assert!(!job.task.contains(x), "{x}: {}", job.task);
    }
    assert!(
        job.task
            .contains(&format!("cloned in {}", cfg.projects_dir))
    );
    // Its deadline once it starts, and the next wait's.
    assert_eq!(job.wake, T + DAY + 65.0 * MIN);
    assert_eq!(out.wake, Some(T + 4.0 * DAY));
    assert_eq!(
        job.unstarted,
        [(
            NO_JOB.into(),
            "sam/r#7".into(),
            "for what's stuck on Renovate's dashboard".into()
        )]
    );
    // Until it has started, nothing is handed.
    assert!(state(&cfg).items.values().all(|it| it.handed.is_none()));
    start(&cfg, DAY, "dashboard", "dash-1");
    let st = state(&cfg);
    assert!(
        st.asked.is_empty() && st.job.as_deref() == Some("dash-1") && st.job_at == Some(T + DAY)
    );
    let handed: Vec<&str> = st
        .items
        .iter()
        .filter(|(_, it)| it.handed == Some(T + DAY))
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(
        handed,
        [
            "branch renovate/npm-packages",
            "lookup left-pad",
            "lookup sam/gone",
            "problem WARN: Found renovate config warnings"
        ]
    );

    // Half a day on, nothing to do but wait for a day after the job.
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY + DAY / 2.0);
    assert!(
        out.stuck.is_empty() && out.jobs.is_empty() && out.wake == Some(T + 2.0 * DAY),
        "{out:?}"
    );
    // A day after it, with that job still running: nothing is listed
    // while it works, and the waits, due at four days, wait for it too.
    jobs.set("dash-1", "running");
    let out = watch_at(&cfg, &mut hub, &mut jobs, 2.0 * DAY + 1.0);
    assert!(
        out.stuck.is_empty() && out.wake == Some(T + 2.0 * DAY + 1.0 + 15.0 * MIN),
        "{out:?}"
    );
    let out = watch_at(&cfg, &mut hub, &mut jobs, 4.0 * DAY);
    assert!(out.stuck.is_empty() && out.jobs.is_empty(), "{out:?}");
    // It ended, having fixed the config warnings: the waits go to a job
    // of their own, and what the first one had that's still there goes
    // to the stuck issue.
    jobs.set("dash-1", "done");
    dashboard(&mut hub, &BODY.replace(WARN, ""), &[]);
    let out = watch_at(&cfg, &mut hub, &mut jobs, 4.0 * DAY + 100.0);
    assert_eq!(out.jobs.len(), 1, "{out:?}");
    let task = &out.jobs[0].task;
    assert!(
        task.contains(
            "Pending Approval: chore(deps): update clap to v5 (branch renovate/major-clap)"
        ),
        "{task}"
    );
    assert!(
        task.contains("renovate/ci-actions") && task.contains("Config Migration Needed"),
        "{task}"
    );
    assert!(!task.contains("Repository Problems"), "{task}");
    assert_eq!(out.stuck.len(), 3, "{out:?}");
    assert!(
        lines(&out)
            .iter()
            .all(|(h, a, _)| (*h, *a) == (STUCK, "sam/r#7")),
        "{out:?}"
    );
    let since = day(T);
    assert!(lines(&out).contains(&(STUCK, "sam/r#7", &format!(
        "Errored: chore(deps): update npm packages (`a`, `b`) (branch renovate/npm-packages), there since {since}"
    ))));
    assert!(
        !lines(&out)
            .iter()
            .any(|(_, _, w)| w.contains("config warnings"))
    );
    start(&cfg, 4.0 * DAY + 100.0, "dashboard", "dash-2");
    // Gone from the dashboard: gone from the list; back again, it
    // starts over.
    jobs.set("dash-2", "running");
    dashboard(
        &mut hub,
        &BODY
            .replace("`left-pad`, `sam/gone`", "`sam/gone`")
            .replace(WARN, ""),
        &[],
    );
    assert_eq!(
        watch_at(&cfg, &mut hub, &mut jobs, 4.0 * DAY + 200.0)
            .stuck
            .len(),
        2
    );
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 4.0 * DAY + 300.0);
    assert_eq!(
        state(&cfg).items["problem WARN: Found renovate config warnings"],
        Item {
            since: T + 4.0 * DAY + 300.0,
            section: "Repository Problems".into(),
            what: "WARN: Found renovate config warnings".into(),
            failing: true,
            handed: None,
            job: None,
        }
    );
}

#[test]
fn a_dashboard_renovate_didnt_open_or_that_went_is_dropped() {
    // Someone else's, closed, renamed or deleted: its number, etag and
    // items go, and the one Renovate opens in its place is found two
    // hours on.
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    for change in [
        &[("who", "eve")][..],
        &[("state", "closed")],
        &[("title", "Dependency Dashboard!")],
        &[("deleted", "")],
    ] {
        let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
        hits(&mut hub, 7, "sam");
        dashboard(&mut hub, BODY, &[]);
        fresh(&cfg);
        watch_at(&cfg, &mut hub, &mut jobs, 0.0);
        assert_eq!(watch_at(&cfg, &mut hub, &mut jobs, DAY).jobs.len(), 1);
        start(&cfg, DAY, "dashboard", "dash-1");
        if change[0].0 == "deleted" {
            hub.on(
                "GET",
                "repos/sam/r/issues/7",
                answer(410, json!({"message": "This issue was deleted"})),
            );
        } else {
            dashboard(&mut hub, BODY, change);
        }
        let out = watch_at(&cfg, &mut hub, &mut jobs, 10.0 * DAY);
        assert!(
            out.stuck.is_empty() && out.jobs.is_empty(),
            "{change:?}: {out:?}"
        );
        assert_eq!(
            state(&cfg),
            State {
                next: Some(T + 10.0 * DAY + 2.0 * HOUR),
                ..State::default()
            },
            "{change:?}"
        );
        watch_at(&cfg, &mut hub, &mut jobs, 10.0 * DAY + 60.0);
        assert!(hub.asked.is_empty(), "{:?}", hub.paths());
        dashboard(&mut hub, BODY, &[]);
        hits(&mut hub, 12, "sam");
        hub.on("GET", "repos/sam/r/issues/12", answer(200, json!({"state": "open", "title": "Dependency Dashboard", "user": {"login": "sam"}, "body": BODY})));
        watch_at(&cfg, &mut hub, &mut jobs, 10.0 * DAY + 2.0 * HOUR);
        assert_eq!(hub.paths()[1], "repos/sam/r/issues/12", "{change:?}");
        assert!(hub.paths()[0].starts_with("search/issues?") && hub.asked[1].2.is_none());
        let keys: Vec<String> = state(&cfg).items.into_keys().collect();
        assert_eq!(keys, read(BODY).into_keys().collect::<Vec<_>>());
    }
}

#[test]
fn renovate_as_the_machines_app_drops_the_users_dashboard() {
    // Once Renovate runs as the machine's own App, the user's dashboard
    // from before is someone else's: it goes, and the App's is found in
    // its place.
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(d.path());
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    assert_eq!(state(&cfg).n, Some(7));
    cfg.renovate = Some("workers[bot]".into());
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY);
    assert!(
        out.stuck.is_empty() && out.jobs.is_empty() && hub.asked.is_empty(),
        "{out:?}"
    );
    assert_eq!(
        state(&cfg),
        State {
            next: Some(T + DAY + 2.0 * HOUR),
            ..State::default()
        }
    );
    dashboard(&mut hub, BODY, &[("who", "workers[bot]")]);
    hits(&mut hub, 7, "workers[bot]");
    watch_at(&cfg, &mut hub, &mut jobs, DAY + 2.0 * HOUR);
    assert_eq!(state(&cfg).items.len(), read(BODY).len());
}

#[test]
fn only_renovates_own_login_opens_the_dashboard() {
    // Renovate as the machine user's login: a hit anyone else opened
    // isn't it. As the machine's own App: not an author's from before,
    // nor a person who took the bot's name.
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(d.path());
    let mut jobs = States::new("done");
    let found = |cfg: &Config, jobs: &mut States, hits: Value| {
        let mut hub = Hub::default();
        hub.on("GET", "search/issues", answer(200, json!({"items": hits})));
        let (_, n) = look(cfg, &mut hub, jobs, 0.0, |p| {
            find(p, p.cfg.dashboard.as_ref().unwrap(), &mut State::default())
        });
        n.unwrap()
    };
    let hit = |n: u64, who: &str| json!({"number": n, "title": "Dependency Dashboard", "user": {"login": who}});
    assert_eq!(
        found(&cfg, &mut jobs, json!([hit(3, "mallory"), hit(4, "sam")])),
        Some(4)
    );
    assert_eq!(found(&cfg, &mut jobs, json!([hit(3, "mallory")])), None);
    cfg.renovate = Some("workers[bot]".into());
    assert_eq!(
        found(
            &cfg,
            &mut jobs,
            json!([hit(3, "sam"), hit(4, "workers"), hit(5, "workers[bot]")])
        ),
        Some(5)
    );
}

#[test]
fn github_not_answering_keeps_all_of_it() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    let was = state(&cfg);
    hub.on("GET", "repos/sam/r/issues/7", answer(502, Value::Null));
    let (out, r) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        super::watch(p, p.cfg.dashboard.as_ref().unwrap())
    });
    assert_eq!(
        r.unwrap_err(),
        "GET repos/sam/r/issues/7: HTTP 502, 3 times"
    );
    assert!(out.limited.is_none() && out.stuck.is_empty());
    assert_eq!(state(&cfg), was);
    // A rate limit is noted, with when it resets.
    hub.on(
        "GET",
        "repos/sam/r/issues/7",
        answer(
            403,
            json!({"message": "API rate limit exceeded for installation"}),
        ),
    );
    hub.on(
        "GET",
        "rate_limit",
        answer(
            200,
            json!({"resources": {"core": {"remaining": 0, "reset": 1800000999}}}),
        ),
    );
    let (out, r) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        super::watch(p, p.cfg.dashboard.as_ref().unwrap())
    });
    assert!(r.unwrap_err().contains("rate limit"));
    assert_eq!(out.limited, Some(1800000999.0));
    assert_eq!(state(&cfg), was);
}

#[test]
fn a_search_finds_none_then_waits_a_day_and_a_pass_searches_so_many() {
    // None found, or only an issue by another title: no read, and the
    // search waits a day.
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    let other = json!({"items": [{"number": 9, "title": "Dependency Dashboard (old)", "user": {"login": "sam"}}]});
    hub.on("GET", "search/issues", answer(200, other));
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    assert_eq!(hub.asked.len(), 1);
    assert_eq!(
        state(&cfg),
        State {
            next: Some(T + DAY),
            ..State::default()
        }
    );
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY - 1.0);
    assert!(hub.asked.is_empty() && !out.searched && out.wake.is_none());
    // A pass searches for so many repos at most: one that has had its
    // searches waits for the next pass.
    fresh(&cfg);
    for (before, searches) in [(FINDS - 1, true), (FINDS, false)] {
        let (out, r) = look(&cfg, &mut hub, &mut jobs, 2.0 * DAY, |p| {
            p.searched = before;
            super::watch(p, p.cfg.dashboard.as_ref().unwrap())
        });
        r.unwrap();
        assert_eq!(
            (out.searched, hub.asked.len()),
            (searches, usize::from(searches)),
            "{before}"
        );
        fresh(&cfg);
    }
}

#[test]
fn an_error_is_stuck_after_two_of_renovates_runs() {
    // Still there after two of Renovate's runs on the repo since it
    // showed up, it's stuck then, short of a day: one run isn't two, a
    // run before it doesn't count, and the waits wait their days. The
    // runs over every repo count with the repo's own.
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    let ran = d.path().join("renovate-runs");
    fs::create_dir_all(&ran).unwrap();
    fs::write(
        ran.join("all"),
        format!("{}\n{}\nnot-a-time\n", T - 600.0, T + 600.0),
    )
    .unwrap();
    let out = watch_at(&cfg, &mut hub, &mut jobs, 1200.0);
    assert!(out.jobs.is_empty() && out.wake == Some(T + DAY), "{out:?}");
    fs::write(ran.join("sam_r"), format!("{}\n", T + 1500.0)).unwrap();
    let out = watch_at(&cfg, &mut hub, &mut jobs, 1800.0);
    assert_eq!(out.jobs.len(), 1, "{out:?}");
    let task = &out.jobs[0].task;
    assert!(
        task.contains("Repository Problems: WARN: Found renovate config warnings")
            && task.contains("couldn't look up `left-pad`"),
        "{task}"
    );
    assert!(
        !task.contains("clap") && !task.contains("Config Migration Needed"),
        "{task}"
    );
}

#[test]
fn a_job_asked_for_is_found_by_its_key_however_long_it_waits() {
    // Nothing tells the look what became of the job it asked for: the
    // next one finds it by its key. Until one has started under it, it's
    // asked for again under the same key, so a starter that outlived its
    // pass and takes the key after all is the only job; while proj can't
    // say, nothing's asked for; and one still queued days on holds off any
    // other.
    let d = tempfile::tempdir().unwrap();
    let cfg = Config {
        flaky: None,
        ..config(d.path())
    };
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY);
    assert_eq!(out.jobs.len(), 1);
    let key = format!("upkeep/sam/r/dashboard/{}", T + DAY);
    assert_eq!(out.jobs[0].key, key);
    jobs.finds.clear();
    let out = watch_at(&cfg, &mut hub, &mut jobs, DAY + 1.0);
    assert_eq!(jobs.finds, std::slice::from_ref(&key));
    assert_eq!(out.jobs.len(), 1, "{out:?}");
    assert_eq!(out.jobs[0].key, key);
    // Closed and found again meanwhile: the key stands.
    dashboard(&mut hub, BODY, &[("state", "closed")]);
    watch_at(&cfg, &mut hub, &mut jobs, DAY + 2.0);
    assert_eq!(state(&cfg).asked_at, Some(T + DAY));
    jobs.unfound = Some("proj find: exit status: 1".into());
    let (out, r) = look(&cfg, &mut hub, &mut jobs, DAY + 3.0, |p| {
        crate::watch(p);
        Ok(())
    });
    r.unwrap();
    assert!(out.jobs.is_empty(), "{out:?}");
    assert_eq!(
        out.failed,
        [format!(
            "its dashboard: whether the job it asked for started ({key}): proj find: exit status: 1"
        )]
    );
    assert_eq!(state(&cfg).asked_at, Some(T + DAY));
    // Queued, and still queued a week on: it has what was asked, and no
    // other job starts beside it.
    jobs.unfound = None;
    fresh(&cfg);
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    assert_eq!(watch_at(&cfg, &mut hub, &mut jobs, DAY).jobs.len(), 1);
    jobs.start(&cfg, "dashboard", "dash-q");
    jobs.set("dash-q", "queued");
    for t in [DAY + 3.0, 8.0 * DAY] {
        let out = watch_at(&cfg, &mut hub, &mut jobs, t);
        assert!(out.jobs.is_empty(), "{out:?}");
    }
    let st = state(&cfg);
    assert!(st.asked.is_empty() && st.asked_at.is_none(), "{st:?}");
    assert_eq!(
        (st.job.as_deref(), st.job_at),
        (Some("dash-q"), Some(T + DAY))
    );
    assert!(
        st.items
            .values()
            .filter(|it| it.handed.is_some())
            .all(|it| it.job.as_deref() == Some("dash-q") && it.handed == Some(T + DAY))
    );
}

#[test]
fn a_job_whose_ask_isnt_kept_isnt_asked_for() {
    // The state file can't be written: the job its look asked for isn't
    // asked of the pass, since nothing would hold another off beside it.
    let d = tempfile::tempdir().unwrap();
    let cfg = Config {
        flaky: None,
        ..config(d.path())
    };
    let (mut hub, mut jobs) = (Hub::default(), States::new("done"));
    hits(&mut hub, 7, "sam");
    dashboard(&mut hub, BODY, &[]);
    watch_at(&cfg, &mut hub, &mut jobs, 0.0);
    let tmp = cfg.state_dir.join("dashboard-sam_r.json.tmp");
    fs::create_dir(&tmp).unwrap();
    let (out, r) = look(&cfg, &mut hub, &mut jobs, DAY, |p| {
        crate::watch(p);
        Ok(())
    });
    r.unwrap();
    assert!(out.jobs.is_empty(), "{out:?}");
    assert!(
        out.failed.len() == 1 && out.failed[0].starts_with("its dashboard: "),
        "{out:?}"
    );
    fs::remove_dir(&tmp).unwrap();
    assert_eq!(watch_at(&cfg, &mut hub, &mut jobs, DAY + 1.0).jobs.len(), 1);
}
