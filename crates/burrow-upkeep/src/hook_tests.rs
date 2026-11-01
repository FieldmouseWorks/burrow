//! The webhooks: each repo's, kept with its settings; a full pass's, on the
//! projects' repos and off the ones no longer seen to, as each owner's
//! installation; and the App's own.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use burrow_github::Api;
use gh_token::error::Error;
use gh_token::fake;
use serde_json::{Value, json};

use super::*;
use crate::settle;
use crate::testing::{Hub, States, T, answer, config, look};

const SECRET: &str = "hook-secret";
const URL: &str = "https://example.org/burrow/hook";
/// What a Python pass kept for the test config's URL and events, and SECRET.
const PYTHONS: &str = "19ba51989bc6da0e";
/// And for the App's own: App 7's, to the URL with SECRET.
const PYTHONS_APP: &str = "2411815f2bfbc48e";

/// GitHub as each owner's installation: a Hub each, none for an owner the
/// App isn't installed on, and no token for one GitHub doesn't answer.
#[derive(Default)]
struct World {
    hubs: HashMap<String, Hub>,
    uninstalled: BTreeSet<String>,
    down: BTreeSet<String>,
    /// Whose installation each look asked for.
    asked: Vec<String>,
}

impl World {
    fn hub(&mut self, owner: &str) -> &mut Hub {
        self.hubs.entry(owner.into()).or_default()
    }

    /// Every owner's asks, as `METHOD path`, and none kept.
    fn calls(&mut self) -> Vec<String> {
        let mut all = vec![];
        for hub in self.hubs.values_mut() {
            all.extend(hub.asked.drain(..).map(|(m, p, _, _)| format!("{m} {p}")));
        }
        all.sort();
        self.asked.clear();
        all
    }
}

impl Owners for World {
    fn api(&mut self, owner: &str) -> Result<Option<&mut dyn Api>, Error> {
        let owner = owner.to_lowercase();
        self.asked.push(owner.clone());
        if self.down.contains(&owner) {
            return Err(Error::unavailable("/app/installations: http status 502"));
        }
        if self.uninstalled.contains(&owner) {
            return Ok(None);
        }
        Ok(Some(self.hub(&owner) as &mut dyn Api))
    }
}

/// An App no test of the repos' hooks asks anything of.
fn app() -> App {
    App::new("http://127.0.0.1:9", None, 7, &fake::key(true)).unwrap()
}

fn hooks<'a>(cfg: &'a Config, world: &'a mut World, app: &'a App, at: f64) -> Hooks<'a> {
    // As the steward has it (one test's isn't a dir).
    let _ = fs::create_dir_all(&cfg.state_dir);
    Hooks {
        cfg,
        now: T + at,
        secret: SECRET,
        owners: world,
        app,
        clock: 1_000_000,
        out: Out::default(),
        said: vec![],
    }
}

fn put(cfg: &Config, name: &str, v: Value) {
    let path = cfg.state_dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, v.to_string()).unwrap();
}

fn got(cfg: &Config, name: &str) -> Value {
    fs::read_to_string(cfg.state_dir.join(name))
        .map_or(Value::Null, |t| serde_json::from_str(&t).unwrap())
}

fn asked(hub: &Hub) -> Vec<String> {
    hub.asked
        .iter()
        .map(|(m, p, _, _)| format!("{m} {p}"))
        .collect()
}

fn sent(hub: &Hub) -> Value {
    hub.asked.last().unwrap().3.clone().unwrap()
}

fn repos(seen: &[&str], projects: &[&str]) -> Repos {
    Repos {
        seen: seen.iter().map(|s| s.to_string()).collect(),
        projects: projects.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_hook_is_known_by_the_hash_a_python_pass_kept() {
    assert_eq!(
        hash(&json!([URL, ["check_suite", "push"], SECRET])),
        PYTHONS
    );
    assert_eq!(hash(&json!([7, URL, SECRET])), PYTHONS_APP);
    // Python's json.dumps escapes past printable ASCII, as UTF-16.
    let odd = "s\u{e9}cr\u{ea}t \u{1f511} \"q\" \\\\ \u{7f}\u{1}\n/";
    assert_eq!(
        dumps(&json!([odd])),
        r#"["s\u00e9cr\u00eat \ud83d\udd11 \"q\" \\\\ \u007f\u0001\n/"]"#
    );
    assert_eq!(
        hash(&json!([URL, ["check_suite"], odd])),
        "a28df63dfd4cbbf8"
    );
}

#[test]
fn a_repos_hook_is_added_moved_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    put(&cfg, "repo-sam_r.json", json!({"settings": 5}));
    let mut hub = Hub::default();
    let mut jobs = States::new("running");
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(200, json!([])),
    );
    hub.on("POST", "repos/sam/r/hooks", answer(201, json!({"id": 9})));
    let mut keep_now = |cfg: &Config, hub: &mut Hub, secret: &str, due: bool| {
        let mut st: State = pass::load(&cfg.state_dir, "repo-sam_r.json");
        let (_, r) = look(cfg, hub, &mut jobs, 0.0, |p| keep(p, secret, &mut st, due));
        r
    };
    // Missing: added, sending what burrow-events' handlers take.
    keep_now(&cfg, &mut hub, SECRET, false).unwrap();
    assert_eq!(
        asked(&hub),
        [
            "GET repos/sam/r/hooks?per_page=100",
            "POST repos/sam/r/hooks"
        ]
    );
    assert_eq!(
        sent(&hub),
        json!({"name": "web", "active": true, "events": ["check_suite", "push"],
               "config": {"url": URL, "content_type": "json", "secret": SECRET, "insecure_ssl": "0"}})
    );
    assert_eq!(
        got(&cfg, "repo-sam_r.json"),
        json!({"settings": 5, "hook": PYTHONS, "hookId": 9})
    );
    // As it's wanted and not due: nothing asked.
    keep_now(&cfg, &mut hub, SECRET, false).unwrap();
    assert!(hub.asked.is_empty(), "{:?}", hub.asked);
    // Due with the daily settings: set again, the one with its id.
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(
            200,
            json!([{"id": 3, "config": {"url": URL}}, {"id": 9, "config": {"url": "x"}}]),
        ),
    );
    hub.on("PATCH", "repos/sam/r/hooks/9", answer(200, json!({})));
    keep_now(&cfg, &mut hub, SECRET, true).unwrap();
    assert_eq!(
        asked(&hub),
        [
            "GET repos/sam/r/hooks?per_page=100",
            "PATCH repos/sam/r/hooks/9"
        ]
    );
    assert_eq!(sent(&hub)["config"]["url"], URL);
    // A new secret: set again, at once.
    keep_now(&cfg, &mut hub, "new-secret", false).unwrap();
    assert_eq!(sent(&hub)["config"]["secret"], "new-secret");
    let st = got(&cfg, "repo-sam_r.json");
    assert!(st["hook"] != PYTHONS && st["hookId"] == 9, "{st}");
    // A new URL moves the hook it made, by its id, rather than adding one.
    cfg.webhook.url = "https://new.example.org/burrow/hook".into();
    hub.asked.clear();
    keep_now(&cfg, &mut hub, "new-secret", false).unwrap();
    assert_eq!(
        asked(&hub),
        [
            "GET repos/sam/r/hooks?per_page=100",
            "PATCH repos/sam/r/hooks/9"
        ]
    );
    assert_eq!(
        sent(&hub)["config"]["url"],
        "https://new.example.org/burrow/hook"
    );
    // Without the id it was given, it's the one sending to its URL.
    put(&cfg, "repo-sam_r.json", json!({}));
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(
            200,
            json!([{"id": 4, "config": {"url": "https://new.example.org/burrow/hook"}}]),
        ),
    );
    hub.on("PATCH", "repos/sam/r/hooks/4", answer(200, json!({})));
    keep_now(&cfg, &mut hub, "new-secret", false).unwrap();
    assert_eq!(got(&cfg, "repo-sam_r.json")["hookId"], 4);
    // GitHub not giving the hooks, or not taking one: nothing kept, so the
    // next pass tries again.
    put(&cfg, "repo-sam_r.json", json!({"settings": 5}));
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(403, json!({"message": "Must have admin rights"})),
    );
    let e = keep_now(&cfg, &mut hub, SECRET, false).unwrap_err();
    assert!(e.contains("admin rights"), "{e}");
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(200, json!([])),
    );
    hub.on(
        "POST",
        "repos/sam/r/hooks",
        answer(422, json!({"message": "no"})),
    );
    keep_now(&cfg, &mut hub, SECRET, false).unwrap_err();
    assert_eq!(got(&cfg, "repo-sam_r.json"), json!({"settings": 5}));
}

/// The state settle keeps when its labels and settings aren't due: it asks
/// GitHub nothing.
fn settled(cfg: &Config, hook: Option<&str>) {
    let labels: Vec<String> = settle::labels(&cfg.settle)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    let mut st = json!({"settings": T, "app": 7, "train": false, "labels": labels});
    if let Some(h) = hook {
        st["hook"] = h.into();
        st["hookId"] = 9.into();
    }
    // A status run's own too.
    put(cfg, "dry/repo-sam_r.json", st.clone());
    put(cfg, "repo-sam_r.json", st);
}

#[test]
fn settle_keeps_the_hook_and_a_status_run_leaves_it_alone() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let repo = settle::Repo {
        private: false,
        empty: false,
    };
    let mut hub = Hub::default();
    let mut jobs = States::new("running");
    hub.on(
        "GET",
        "repos/sam/r/hooks?per_page=100",
        answer(200, json!([])),
    );
    hub.on("POST", "repos/sam/r/hooks", answer(201, json!({"id": 9})));
    settled(&cfg, None);
    let (out, _) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        crate::settle(p, &repo, || Ok(SECRET.into()));
        Ok(())
    });
    assert_eq!(
        asked(&hub),
        [
            "GET repos/sam/r/hooks?per_page=100",
            "POST repos/sam/r/hooks"
        ]
    );
    assert_eq!(got(&cfg, "repo-sam_r.json")["hook"], PYTHONS);
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    // Kept, and not due as the settings aren't: nothing asked.
    let (_, _) = look(&cfg, &mut hub, &mut jobs, 120.0, |p| {
        crate::settle(p, &repo, || Ok(SECRET.into()));
        Ok(())
    });
    assert!(hub.asked.is_empty(), "{:?}", hub.asked);
    // A status run has no secret, and doesn't ask for one.
    settled(&cfg, None);
    let (out, _) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        p.dry = true;
        crate::settle(p, &repo, || panic!("a status run asked for the secret"));
        Ok(())
    });
    assert!(
        hub.asked.is_empty() && out.failed.is_empty(),
        "{:?}",
        hub.asked
    );
    // A pass not given it fails the repo's look.
    let (out, _) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        crate::settle(p, &repo, || Err("no webhook credential".into()));
        Ok(())
    });
    assert_eq!(out.failed, ["its webhook: no webhook credential"]);
    // GitHub not taking it is only logged: tried again next pass.
    hub.on(
        "POST",
        "repos/sam/r/hooks",
        answer(422, json!({"message": "no"})),
    );
    let (out, _) = look(&cfg, &mut hub, &mut jobs, 60.0, |p| {
        crate::settle(p, &repo, || Ok(SECRET.into()));
        Ok(())
    });
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert!(got(&cfg, "repo-sam_r.json").get("hook").is_none());
}

#[test]
fn a_project_it_doesnt_see_to_gets_its_hook_as_its_owners_installation() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    world.hub("eve").on(
        "GET",
        "repos/eve/p/hooks?per_page=100",
        answer(200, json!([])),
    );
    world
        .hub("eve")
        .on("POST", "repos/eve/p/hooks", answer(201, json!({"id": 5})));
    // sam/r is seen to, under any spelling: only eve's is the projects'.
    let projects = ["sam/r", "Sam/R", "eve/p"].map(String::from);
    let seen = BTreeSet::from(["sam/r".to_string()]);
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    let more = h.projects(&seen, &projects);
    assert_eq!(more, BTreeSet::from(["eve/p".to_string()]));
    assert_eq!(h.said, Vec::<String>::new());
    assert_eq!(world.asked, ["eve"]);
    assert_eq!(
        world.calls(),
        [
            "GET repos/eve/p/hooks?per_page=100",
            "POST repos/eve/p/hooks"
        ]
    );
    assert_eq!(
        got(&cfg, "repo-eve_p.json"),
        json!({"hook": PYTHONS, "hookId": 5, "hookAt": T})
    );
    // Kept and not due: no installation asked for, nothing asked of GitHub.
    let mut h = hooks(&cfg, &mut world, &app, 600.0);
    h.projects(&seen, &projects);
    assert!(world.asked.is_empty() && world.calls().is_empty());
    // A day on, it's set again, in case it was edited.
    world.hub("eve").on(
        "GET",
        "repos/eve/p/hooks?per_page=100",
        answer(200, json!([{"id": 5, "config": {"url": URL}}])),
    );
    world
        .hub("eve")
        .on("PATCH", "repos/eve/p/hooks/5", answer(200, json!({})));
    let mut h = hooks(&cfg, &mut world, &app, DAY);
    h.projects(&seen, &projects);
    assert_eq!(
        world.calls(),
        [
            "GET repos/eve/p/hooks?per_page=100",
            "PATCH repos/eve/p/hooks/5"
        ]
    );
    assert_eq!(got(&cfg, "repo-eve_p.json")["hookAt"], T + DAY);
}

#[test]
fn an_owner_the_app_isnt_on_is_said_once_and_tried_again_a_day_on() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    world.uninstalled.insert("mallory".into());
    world.hub("eve").on(
        "GET",
        "repos/eve/p/hooks?per_page=100",
        answer(200, json!([])),
    );
    world
        .hub("eve")
        .on("POST", "repos/eve/p/hooks", answer(201, json!({"id": 5})));
    let projects = ["mallory/x", "eve/p"].map(String::from);
    let seen = BTreeSet::from(["sam/r".to_string()]);
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    // Kept all the same, for unhook: what its hook is now can't be asked.
    let more = h.projects(&seen, &projects);
    assert_eq!(more.len(), 2);
    let why = "the App steward isn't installed on mallory";
    assert_eq!(
        h.said,
        [format!("mallory/x: its webhook: {why}; tried again daily")]
    );
    assert_eq!(
        world.calls(),
        [
            "GET repos/eve/p/hooks?per_page=100",
            "POST repos/eve/p/hooks"
        ]
    );
    assert_eq!(
        got(&cfg, "repo-mallory_x.json"),
        json!({"hookAt": T, "unreached": why})
    );
    // Not tried again on every pass, and not said again a day on.
    let mut h = hooks(&cfg, &mut world, &app, 3600.0);
    h.projects(&seen, &projects);
    let said = h.said;
    assert!(world.asked.is_empty() && said.is_empty());
    let mut h = hooks(&cfg, &mut world, &app, DAY);
    h.projects(&seen, &projects);
    let said = h.said;
    assert_eq!(world.asked.iter().filter(|o| *o == "mallory").count(), 1);
    assert!(said.is_empty(), "{said:?}");
    world.calls();
    // Installed there now: it gets its hook, and that's said.
    world.uninstalled.clear();
    world.hub("mallory").on(
        "GET",
        "repos/mallory/x/hooks?per_page=100",
        answer(200, json!([])),
    );
    world.hub("mallory").on(
        "POST",
        "repos/mallory/x/hooks",
        answer(201, json!({"id": 6})),
    );
    let mut h = hooks(&cfg, &mut world, &app, 2.0 * DAY);
    h.projects(&seen, &projects);
    assert_eq!(h.said, ["mallory/x: reached again"]);
    assert_eq!(
        got(&cfg, "repo-mallory_x.json"),
        json!({"hook": PYTHONS, "hookId": 6, "hookAt": T + 2.0 * DAY})
    );
}

#[test]
fn a_clone_whose_repo_is_gone_is_looked_up_daily_and_keeps_its_hook() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    let gone = || answer(404, json!({"message": "Not Found"}));
    world
        .hub("sam")
        .on("GET", "repos/sam/gone/hooks?per_page=100", gone());
    world.hub("sam").on("GET", "repos/sam/gone$", gone());
    put(
        &cfg,
        "repo-sam_gone.json",
        json!({"hook": "h", "hookId": 4}),
    );
    let projects = ["sam/gone".to_string()];
    let seen = BTreeSet::from(["sam/r".to_string()]);
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    assert_eq!(h.projects(&seen, &projects).len(), 1);
    let said = format!(
        "sam/gone: GitHub has no such repo, so its clone in {} gets no webhook; looked up again daily",
        cfg.projects_dir
    );
    assert_eq!(
        h.said,
        [
            "sam/gone: its webhook: GET repos/sam/gone/hooks?per_page=100: HTTP 404: Not Found"
                .to_string(),
            said
        ]
    );
    assert_eq!(
        world.calls(),
        [
            "GET repos/sam/gone",
            "GET repos/sam/gone/hooks?per_page=100"
        ]
    );
    assert_eq!(
        got(&cfg, "repo-sam_gone.json"),
        json!({"hook": "h", "hookId": 4, "hookAt": T, "unreached": GONE})
    );
    // Nothing asked within the day; a day on, only the repo, quietly.
    let mut calls = vec![];
    for at in [600.0, 3600.0, DAY + 1.0, DAY + 600.0] {
        let mut h = hooks(&cfg, &mut world, &app, at);
        h.projects(&seen, &projects);
        assert!(h.said.is_empty(), "{:?}", h.said);
        calls.extend(world.calls());
    }
    assert_eq!(calls, ["GET repos/sam/gone"]);
    // Back: the hook it had is the one it keeps, moved to the URL by its id.
    world.hub("sam").on(
        "GET",
        "repos/sam/gone$",
        answer(200, json!({"full_name": "sam/gone"})),
    );
    world.hub("sam").on(
        "GET",
        "repos/sam/gone/hooks?per_page=100",
        answer(
            200,
            json!([{"id": 4, "config": {"url": "https://old.example.org/hook"}}]),
        ),
    );
    world
        .hub("sam")
        .on("PATCH", "repos/sam/gone/hooks/4", answer(200, json!({})));
    let mut h = hooks(&cfg, &mut world, &app, 2.0 * (DAY + 1.0));
    h.projects(&seen, &projects);
    assert_eq!(h.said, ["sam/gone: on GitHub again"]);
    assert_eq!(
        world.calls(),
        [
            "GET repos/sam/gone",
            "GET repos/sam/gone/hooks?per_page=100",
            "PATCH repos/sam/gone/hooks/4"
        ]
    );
    let back = got(&cfg, "repo-sam_gone.json");
    assert!(
        back.get("unreached").is_none() && back["hookId"] == 4 && back["hook"] == PYTHONS,
        "{back}"
    );
}

#[test]
fn github_not_answering_changes_nothing_known() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    world.down.insert("kim".into());
    // GitHub's rate limit on the lookup of one that's gone, noted for the
    // pass.
    put(&cfg, "repo-sam_gone.json", json!({"unreached": GONE}));
    world.hub("sam").on(
        "GET",
        "repos/sam/gone$",
        answer(403, json!({"message": "API rate limit exceeded"})),
    );
    world.hub("sam").on(
        "GET",
        "rate_limit",
        answer(
            200,
            json!({"resources": {"core": {"remaining": 0, "reset": T + 900.0}}}),
        ),
    );
    let projects = ["kim/p", "sam/gone"].map(String::from);
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    h.projects(&BTreeSet::new(), &projects);
    assert_eq!(h.out.limited, Some(T + 900.0));
    assert_eq!(h.said.len(), 2, "{:?}", h.said);
    assert!(h.said[0].starts_with("kim/p: its webhook: /app/installations"));
    assert!(h.said[1].contains("rate limit"), "{:?}", h.said);
    assert_eq!(got(&cfg, "repo-kim_p.json"), json!({"hookAt": T}));
    assert_eq!(
        got(&cfg, "repo-sam_gone.json"),
        json!({"unreached": GONE, "hookAt": T})
    );
}

#[test]
fn a_repo_no_longer_seen_to_has_its_hook_taken_off() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    let h = |id: u64| json!({"hook": "h", "hookId": id});
    put(
        &cfg,
        "repo-sam_old.json",
        json!({"hook": "h", "hookId": 7, "other": 1}),
    );
    put(&cfg, "repo-sam_stuck.json", h(8));
    put(&cfg, "repo-sam_r.json", h(9));
    // A project's repo, whose hook is the projects'.
    put(&cfg, "repo-eve_p.json", h(5));
    // Both renamed to sam/r: GitHub answers the old names with it, and its
    // hook is the one sam/r's adopted.
    put(&cfg, "repo-sam_R.json", h(9));
    put(
        &cfg,
        "repo-sam_was.json",
        json!({"hook": "h", "hookId": 9, "other": 2}),
    );
    // Renamed too, maybe: GitHub doesn't answer, so nothing comes off.
    put(&cfg, "repo-sam_once.json", h(9));
    // No such repo the App can see: deleted, or out of its reach.
    put(
        &cfg,
        "repo-sam_hidden.json",
        json!({"hook": "h", "hookId": 6, "other": 3}),
    );
    // No hook, and not a repo's state.
    put(&cfg, "repo-sam_plain.json", json!({"settings": 1}));
    put(&cfg, "dashboard-sam_old.json", h(1));
    let hub = world.hub("sam");
    for (repo, now) in [
        ("old", "sam/old"),
        ("stuck", "sam/stuck"),
        ("R", "sam/r"),
        ("was", "Sam/r"),
    ] {
        let path = format!("repos/sam/{repo}$");
        hub.on("GET", &path, answer(200, json!({"full_name": now})));
    }
    hub.on(
        "GET",
        "repos/sam/once$",
        answer(502, json!({"message": "Bad Gateway"})),
    );
    hub.on(
        "GET",
        "repos/sam/hidden$",
        answer(404, json!({"message": "Not Found"})),
    );
    hub.on("DELETE", "repos/sam/old/hooks/7", answer(204, Value::Null));
    hub.on(
        "DELETE",
        "repos/sam/stuck/hooks/8",
        answer(403, json!({"message": "Must have admin rights"})),
    );
    let live = ["sam/r", "eve/p"].map(String::from);
    let run = |at: f64, world: &mut World| {
        let mut h = hooks(&cfg, world, &app, at);
        h.unhook(live.iter());
        assert!(h.out.failed.is_empty(), "{:?}", h.out.failed);
        h.said
    };
    let said = run(0.0, &mut world);
    let deletes: Vec<String> = world
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("DELETE"))
        .collect();
    assert_eq!(
        deletes,
        [
            "DELETE repos/sam/old/hooks/7",
            "DELETE repos/sam/stuck/hooks/8"
        ]
    );
    assert_eq!(got(&cfg, "repo-sam_old.json"), json!({"other": 1}));
    assert_eq!(
        got(&cfg, "repo-sam_stuck.json"),
        json!({"hook": "h", "hookId": 8, "unhookTried": T})
    );
    assert_eq!(got(&cfg, "repo-sam_r.json"), h(9));
    assert_eq!(got(&cfg, "repo-eve_p.json"), h(5));
    assert_eq!(got(&cfg, "repo-sam_R.json"), json!({}));
    assert_eq!(got(&cfg, "repo-sam_was.json"), json!({"other": 2}));
    assert_eq!(
        got(&cfg, "repo-sam_once.json"),
        json!({"hook": "h", "hookId": 9, "unhookTried": T})
    );
    let hidden = json!({"hook": "h", "hookId": 6, "other": 3, "unhookTried": T, "unreached": GONE});
    assert_eq!(got(&cfg, "repo-sam_hidden.json"), hidden);
    assert_eq!(got(&cfg, "repo-sam_plain.json"), json!({"settings": 1}));
    assert_eq!(got(&cfg, "dashboard-sam_old.json"), h(1));
    let hidden_said = "sam/hidden: GitHub has no such repo the App can see, so its webhook stays \
                       on record; looked up again daily";
    assert!(said.contains(&hidden_said.to_string()), "{said:?}");
    assert!(
        said.contains(&"sam/R: renamed to sam/r, which keeps the webhook".to_string()),
        "{said:?}"
    );
    // Within the day, nothing's tried again.
    run(600.0, &mut world);
    assert!(world.calls().is_empty());
    // A day on: the one that wouldn't come off, the one GitHub didn't
    // answer for and the hidden one, which isn't said again.
    let said = run(DAY + 1.0, &mut world);
    assert_eq!(
        world.calls(),
        [
            "DELETE repos/sam/stuck/hooks/8",
            "GET repos/sam/hidden",
            "GET repos/sam/once",
            "GET repos/sam/once",
            "GET repos/sam/once",
            "GET repos/sam/stuck",
        ]
    );
    assert!(!said.iter().any(|l| l.contains("sam/hidden")), "{said:?}");
    assert_eq!(
        got(&cfg, "repo-sam_hidden.json"),
        json!({"hook": "h", "hookId": 6, "other": 3, "unhookTried": T + DAY + 1.0, "unreached": GONE})
    );
    // Once answers with sam/r, and the App sees sam/hidden again.
    let hub = world.hub("sam");
    hub.on(
        "GET",
        "repos/sam/once$",
        answer(200, json!({"full_name": "sam/r"})),
    );
    hub.on(
        "GET",
        "repos/sam/hidden$",
        answer(200, json!({"full_name": "sam/hidden"})),
    );
    hub.on(
        "DELETE",
        "repos/sam/hidden/hooks/6",
        answer(204, Value::Null),
    );
    run(2.0 * (DAY + 1.0), &mut world);
    let calls = world.calls();
    assert!(
        !calls.iter().any(|c| c.starts_with("DELETE repos/sam/once")),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"DELETE repos/sam/hidden/hooks/6".to_string()),
        "{calls:?}"
    );
    assert_eq!(got(&cfg, "repo-sam_once.json"), json!({}));
    assert_eq!(got(&cfg, "repo-sam_hidden.json"), json!({"other": 3}));
}

#[test]
fn a_repo_no_longer_seen_to_whose_owner_cant_be_asked_keeps_its_hook() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let app = app();
    let mut world = World::default();
    world.uninstalled.insert("mallory".into());
    world.down.insert("kim".into());
    put(
        &cfg,
        "repo-mallory_x.json",
        json!({"hook": "h", "hookId": 1}),
    );
    put(&cfg, "repo-kim_y.json", json!({"hook": "h", "hookId": 2}));
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    h.unhook([].iter());
    assert_eq!(
        h.said,
        [
            "kim/y: /app/installations: http status 502",
            "mallory/x: the App steward isn't installed on mallory"
        ]
    );
    for (name, id) in [("repo-mallory_x.json", 1), ("repo-kim_y.json", 2)] {
        assert_eq!(
            got(&cfg, name),
            json!({"hook": "h", "hookId": id, "unhookTried": T})
        );
    }
}

#[test]
fn the_apps_own_webhook_is_set_as_the_app_when_it_changes_and_daily() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    let status = Arc::new(AtomicU16::new(200));
    let bodies = Arc::new(Mutex::new(vec![]));
    let (s, b) = (status.clone(), bodies.clone());
    let github = fake::serve(move |r| {
        b.lock().unwrap().push(r.body.clone());
        (
            s.load(Ordering::SeqCst),
            vec![],
            r#"{"message":"x"}"#.into(),
        )
    });
    let mut world = World::default();
    let run = |cfg: &Config, at: f64, secret: &str, world: &mut World| {
        let app = App::new(&github.url, None, cfg.app.id, &fake::key(true)).unwrap();
        let mut h = Hooks {
            secret,
            ..hooks(cfg, world, &app, at)
        };
        h.run(&repos(&[], &[]));
        assert!(h.out.failed.is_empty(), "{:?}", h.out.failed);
        h.said
    };
    assert_eq!(
        run(&cfg, 0.0, SECRET, &mut world),
        ["the App's webhook: set"]
    );
    assert_eq!(github.asked(), ["PATCH /app/hook/config"]);
    let body: Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(
        body,
        json!({"url": URL, "content_type": "json", "secret": SECRET, "insecure_ssl": "0"})
    );
    assert_eq!(
        got(&cfg, "app-hook.json"),
        json!({"hook": PYTHONS_APP, "at": T})
    );
    // The same, within the day: nothing asked.
    assert!(run(&cfg, 600.0, SECRET, &mut world).is_empty());
    assert_eq!(github.asked().len(), 1);
    // A day on, in case it was edited.
    run(&cfg, DAY, SECRET, &mut world);
    assert_eq!(github.asked().len(), 2);
    // A new secret, at once; GitHub not taking it, tried again next pass.
    status.store(502, Ordering::SeqCst);
    let said = run(&cfg, DAY + 60.0, "new-secret", &mut world);
    assert!(
        said[0].starts_with("the App's webhook: ") && said[0].contains("502"),
        "{said:?}"
    );
    status.store(200, Ordering::SeqCst);
    run(&cfg, DAY + 120.0, "new-secret", &mut world);
    assert_eq!(github.asked().len(), 4);
    assert!(bodies.lock().unwrap()[3].contains("new-secret"));
    // Another App, with the same URL and secret: its own, at once.
    cfg.app.id = 8;
    run(&cfg, DAY + 180.0, "new-secret", &mut world);
    assert_eq!(github.asked().len(), 5);
}

#[test]
fn a_full_pass_hooks_the_projects_then_unhooks_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let github = fake::serve(|_| (200, vec![], "{}".into()));
    let app = App::new(&github.url, None, 7, &fake::key(true)).unwrap();
    let mut world = World::default();
    let eve = world.hub("eve");
    eve.on(
        "GET",
        "repos/eve/p/hooks?per_page=100",
        answer(200, json!([])),
    );
    eve.on("POST", "repos/eve/p/hooks", answer(201, json!({"id": 5})));
    // Its project's hook stays on, as the hooks of the repos it sees to do.
    put(&cfg, "repo-sam_r.json", json!({"hook": "h", "hookId": 9}));
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    h.run(&repos(&["sam/r"], &["eve/p"]));
    assert_eq!(
        world.calls(),
        [
            "GET repos/eve/p/hooks?per_page=100",
            "POST repos/eve/p/hooks"
        ]
    );
    assert_eq!(github.asked(), ["PATCH /app/hook/config"]);
    // What won't save is what fails: a state dir that isn't one. (Its hook
    // not saving, eve/p's looked up.)
    world.hub("eve").on(
        "GET",
        "repos/eve/p$",
        answer(200, json!({"full_name": "eve/p"})),
    );
    let file = dir.path().join("file");
    fs::write(&file, "").unwrap();
    let mut cfg = config(dir.path());
    cfg.state_dir = file;
    let mut h = hooks(&cfg, &mut world, &app, 0.0);
    h.run(&repos(&[], &["eve/p"]));
    assert_eq!(h.out.failed.len(), 2, "{:?}", h.out.failed);
}
