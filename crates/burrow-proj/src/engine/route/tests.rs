//! A route against stand-in meters (every selection, a third engine
//! included), then against the machine's own: Claude's reading and hold,
//! the credit's meter and key, and where the turn runs.
use super::*;
use crate::config::tests::{example, set};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Stand-in meters: each engine's use in percent (None: no fit reading),
/// and whether it's out of usage by a hold.
#[derive(Default)]
struct Stand(BTreeMap<&'static str, (Option<f64>, bool)>);

impl Stand {
    fn with(mut self, engine: &'static str, used: Option<f64>, out: bool) -> Self {
        self.0.insert(engine, (used, out));
        self
    }
}

impl Meters<&'static str> for Stand {
    fn short(&self, e: &'static str, pct: f64) -> bool {
        let (used, out) = self.0.get(e).copied().unwrap_or_default();
        out || used.is_some_and(|u| u >= pct)
    }
    fn room(&self, e: &'static str) -> bool {
        let (used, out) = self.0.get(e).copied().unwrap_or_default();
        !out && used.is_some_and(|u| u < 100.0)
    }
}

const CHAIN: [(f64, &str); 2] = [(95.0, "claude-api"), (100.0, "codex")];

fn three(m: &Stand) -> &'static str {
    pick("claude", &CHAIN, m)
}

#[test]
fn each_engine_along_the_route_is_taken_in_order() {
    // The subscription with room under its threshold.
    let m = Stand::default()
        .with("claude", Some(50.0), false)
        .with("claude-api", Some(0.0), false)
        .with("codex", Some(0.0), false);
    assert_eq!(three(&m), "claude");
    // Past it: the credit.
    let m = m.with("claude", Some(96.0), false);
    assert_eq!(three(&m), "claude-api");
    // The credit spent or paced out: Codex, which has room.
    let m = m.with("claude-api", Some(100.0), false);
    assert_eq!(three(&m), "codex");
    // Codex without room: back to the subscription, whose turn waits for it.
    let m = m.with("codex", Some(100.0), false);
    assert_eq!(three(&m), "claude");
    // Codex's meter unreadable: never moved onto.
    let m = m.with("codex", None, false);
    assert_eq!(three(&m), "claude");
}

#[test]
fn the_subscription_takes_its_place_back_as_soon_as_it_has_room() {
    let out =
        Stand::default()
            .with("claude", Some(10.0), true)
            .with("claude-api", Some(0.0), false);
    assert_eq!(three(&out), "claude-api");
    // The hold gone and a fresh reading under the threshold: nothing
    // remembered the move.
    let back = out.with("claude", Some(10.0), false);
    assert_eq!(three(&back), "claude");
}

#[test]
fn a_stale_reading_moves_nothing_but_a_refusal_does() {
    // No fit reading of the subscription: it runs as it stands.
    let stale = Stand::default()
        .with("claude", None, false)
        .with("claude-api", Some(0.0), false);
    assert_eq!(three(&stale), "claude");
    // Out of usage by its refusal's hold, whatever its reading.
    let refused = stale.with("claude", None, true);
    assert_eq!(three(&refused), "claude-api");
}

#[test]
fn past_every_threshold_the_first_that_can_run_takes_it() {
    // The subscription past its threshold but not out, the credit past
    // its own, Codex without room: the subscription, first.
    let m = Stand::default()
        .with("claude", Some(97.0), false)
        .with("claude-api", Some(40.0), false)
        .with("codex", Some(100.0), false);
    let at_30 = [(95.0, "claude-api"), (30.0, "codex")];
    assert_eq!(pick("claude", &at_30, &m), "claude");
    // The subscription out: the credit, past its threshold, still runs.
    let m = m.with("claude", Some(97.0), true);
    assert_eq!(pick("claude", &at_30, &m), "claude-api");
    // Codex with room: past the credit's threshold, Codex.
    let m = m.with("codex", Some(5.0), false);
    assert_eq!(pick("claude", &at_30, &m), "codex");
}

#[test]
fn a_single_hop_route_moves_as_it_always_did() {
    let hop = [(80.0, "claude-api")];
    let m = Stand::default().with("claude-api", Some(0.0), false);
    // No reading of the first: no move.
    assert_eq!(pick("claude", &hop, &m), "claude");
    for (used, expected) in [
        (79.0, "claude"),
        (80.0, "claude-api"),
        (100.0, "claude-api"),
    ] {
        let m =
            Stand::default()
                .with("claude", Some(used), false)
                .with("claude-api", Some(0.0), false);
        assert_eq!(pick("claude", &hop, &m), expected, "{used}");
    }
    // Nowhere to go: the first engine.
    let m = Stand::default().with("claude", Some(100.0), false);
    assert_eq!(pick("claude", &hop, &m), "claude");
    // A route of one engine is that engine.
    assert_eq!(pick("claude", &[], &m), "claude");
}

/// The machine: a meters folder, a $20 credit expiring in two days with $5
/// in reserve ($10 a day may go), a key, and jobs.
struct Rig {
    tmp: tempfile::TempDir,
    cfg: Config,
}

fn rig(route: &str) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = tmp.path().join("jobs");
    cfg.meters = Some(tmp.path().join("meters"));
    fs::create_dir_all(&cfg.jobs).unwrap();
    fs::create_dir_all(tmp.path().join("meters")).unwrap();
    let renews = jiff::Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(2))
        .unwrap();
    set(
        &mut cfg,
        tmp.path(),
        &[
            &format!("credit 20 {renews} 200 5"),
            "offer claude models sub-model",
            "offer claude-api models api-model",
            "model * claude sub-model low",
            "model * claude-api api-model high",
            &format!("role * {route}"),
        ],
    );
    let key = tmp.path().join("api-key");
    fs::write(&key, "sk-ant-stand-in\n").unwrap();
    cfg.claude_api_key_file = Some(key);
    Rig { tmp, cfg }
}

impl Rig {
    /// Claude's reading: its week at `used`, read `age` seconds ago.
    fn claude(&self, used: f64, age: i64) {
        let now = crate::now();
        fs::write(
            self.cfg.meters.as_ref().unwrap().join("claude.json"),
            json!({
                "windows": {"seven_day": {"used": used, "resets_at": now + 86400}},
                "credits": false,
                "read_at": now - age,
            })
            .to_string(),
        )
        .unwrap();
    }

    /// Claude Code said it's out of usage: the hold its refusal records.
    fn refused(&self) {
        self.refused_at(crate::now());
    }

    fn refused_at(&self, at: i64) {
        crate::usage::record(
            &self.cfg,
            "refused",
            Some(at + 3600),
            "Claude reached its weekly allowance",
            at,
        )
        .unwrap();
    }

    /// Claude's reading at `used`, asked for `asked` seconds ago and read
    /// `age` ago: what proves a hold over once it's asked after the refusal.
    fn claude_asked(&self, used: f64, asked: i64, age: i64) {
        let now = crate::now();
        fs::write(
            self.cfg.meters.as_ref().unwrap().join("claude.json"),
            json!({
                "windows": {"seven_day": {"used": used, "resets_at": now + 86400}},
                "credits": false,
                "read_at": now - age,
                "requested_at": now - asked,
            })
            .to_string(),
        )
        .unwrap();
    }

    /// A credit run of $N recorded today.
    fn spent(&self, usd: i64) {
        let dir = self.cfg.jobs.join(format!("spent-{usd}"));
        fs::create_dir_all(&dir).unwrap();
        let now = crate::now();
        fs::write(dir.join("1.engine"), "claude-api").unwrap();
        fs::write(dir.join("1.started"), now.to_string()).unwrap();
        fs::write(
            dir.join("1.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"type": "burrow.run", "at": now, "budget": usd * 1_000_000}),
                json!({"type": "result", "session_id": "a", "total_cost_usd": usd}),
            ),
        )
        .unwrap();
    }

    fn choice(&self, sandboxed: bool) -> super::super::Choice {
        super::super::resolve(&self.cfg, "work", None, None, sandboxed).unwrap()
    }

    fn engine(&self) -> Engine {
        self.choice(false).engine
    }
}

const ROUTE: &str = "claude over 95 claude-api";

#[test]
fn the_subscription_short_by_its_fresh_meter_moves_to_the_credit_with_its_tuning() {
    let r = rig(ROUTE);
    r.claude(97.0, 60);
    let choice = r.choice(false);
    assert_eq!(choice.engine, Engine::ClaudeApi);
    assert_eq!(choice.model.as_deref(), Some("api-model"));
    assert_eq!(choice.effort.as_deref(), Some("high"));
    // Under the threshold, the subscription and its own tuning.
    r.claude(94.0, 60);
    let choice = r.choice(false);
    assert_eq!(choice.engine, Engine::Claude);
    assert_eq!(choice.model.as_deref(), Some("sub-model"));
    assert_eq!(choice.effort.as_deref(), Some("low"));
}

#[test]
fn a_stale_reading_moves_nothing_and_a_refusal_moves_it_anyway() {
    let r = rig(ROUTE);
    // An hour old, and from the future.
    r.claude(97.0, 3600);
    assert_eq!(r.engine(), Engine::Claude);
    r.claude(97.0, -60);
    assert_eq!(r.engine(), Engine::Claude);
    // Claude Code said it's out of usage: the credit, though the cached
    // reading is stale (and says it has room).
    r.claude(10.0, 7200);
    r.refused();
    assert_eq!(r.engine(), Engine::ClaudeApi);
    // With no reading at all, too.
    fs::remove_file(r.cfg.meters.as_ref().unwrap().join("claude.json")).unwrap();
    assert_eq!(r.engine(), Engine::ClaudeApi);
}

#[test]
fn the_subscription_takes_the_next_turn_back_once_its_hold_is_gone() {
    let mut r = rig(ROUTE);
    r.refused();
    assert_eq!(r.engine(), Engine::ClaudeApi);
    // The hold over, the reading fresh and under the threshold.
    fs::remove_file(r.cfg.jobs.join(crate::usage::FILE)).unwrap();
    r.claude(20.0, 10);
    assert_eq!(r.engine(), Engine::Claude);
    // The same for a route set over a chain that started on the credit.
    set(
        &mut r.cfg,
        r.tmp.path(),
        &["role * claude-api over 50 claude"],
    );
    assert_eq!(r.engine(), Engine::ClaudeApi);
}

/// A fresh meter asked for after the refusal, with room, proves the hold
/// over before any pass releases it: the subscription has the next turn. One
/// asked for before the refusal proves nothing.
#[test]
fn a_fresh_meter_after_the_refusal_gives_the_subscription_its_turn_back() {
    let r = rig(ROUTE);
    let now = crate::now();
    r.refused_at(now - 100);
    r.claude_asked(20.0, 200, 150);
    assert_eq!(r.engine(), Engine::ClaudeApi, "asked before the refusal");
    r.claude_asked(20.0, 50, 40);
    assert_eq!(r.engine(), Engine::Claude);
    assert!(crate::usage::out(&r.cfg).is_some(), "nothing was released");
}

#[test]
fn a_credit_without_room_or_state_keeps_the_turn_on_the_subscription() {
    type Short = fn(&mut Rig);
    let cases: [(&str, Short); 8] = [
        ("no credit set", |r| {
            set(&mut r.cfg, Path::new("/"), &["credit off"])
        }),
        ("an unreadable credit", |r| {
            let file = r.cfg.settings.as_ref().unwrap().credit_file();
            fs::write(file, "{not json").unwrap();
        }),
        ("no settings", |r| r.cfg.settings = None),
        // $15 spent and $5 kept: nothing above the reserve.
        ("spent to its reserve", |r| r.spent(15)),
        // Today's $10 spent: paced till midnight.
        ("today's allowance spent", |r| r.spent(10)),
        // Another turn's run on the credit, going with all of today's room:
        // a second turn routed now stays where it is, rather than be
        // refused at its launch.
        ("a run going holds its budget", |r| {
            let dir = r.cfg.jobs.join("going");
            fs::create_dir_all(&dir).unwrap();
            let now = crate::now();
            fs::write(dir.join("1.engine"), "claude-api").unwrap();
            fs::write(dir.join("1.started"), now.to_string()).unwrap();
            fs::write(
                dir.join("1.jsonl"),
                format!(
                    "{}\n",
                    json!({"type": "burrow.run", "at": now, "budget": 10_000_000})
                ),
            )
            .unwrap();
        }),
        ("no key", |r| r.cfg.claude_api_key_file = None),
        ("an unreadable key", |r| {
            r.cfg.claude_api_key_file = Some(r.tmp.path().join("absent"))
        }),
    ];
    for (name, short) in cases {
        let mut r = rig(ROUTE);
        r.refused();
        assert_eq!(r.engine(), Engine::ClaudeApi, "{name}: before");
        short(&mut r);
        assert_eq!(r.engine(), Engine::Claude, "{name}");
    }
    // Some spent, room left: still the credit.
    let r = rig(ROUTE);
    r.refused();
    r.spent(3);
    assert_eq!(r.engine(), Engine::ClaudeApi);
}

#[test]
fn a_sandbox_never_moves_to_the_credit() {
    let r = rig(ROUTE);
    r.refused();
    assert_eq!(r.choice(true).engine, Engine::Claude);
}

#[test]
fn the_credits_threshold_is_its_cycles_spend() {
    // The credit first, the subscription past half the credit's budget.
    let r = rig("claude-api over 50 claude");
    r.claude(10.0, 60);
    assert_eq!(r.engine(), Engine::ClaudeApi);
    r.spent(9);
    assert_eq!(r.engine(), Engine::ClaudeApi, "$9 of $20");
    r.spent(1);
    assert_eq!(r.engine(), Engine::Claude, "$10 of $20");
}

#[test]
fn a_pin_starts_the_route_where_it_names_the_engine() {
    let r = rig(ROUTE);
    r.refused();
    let pinned = |pin| {
        super::super::resolve(&r.cfg, "work", None, Some(pin), false)
            .unwrap()
            .engine
    };
    assert_eq!(pinned("claude"), Engine::ClaudeApi);
    assert_eq!(pinned("claude-api"), Engine::ClaudeApi);
    // Pinned to an engine the route doesn't name: that engine alone.
    let r = rig("claude");
    r.refused();
    assert_eq!(
        super::super::resolve(&r.cfg, "work", None, Some("claude-api"), false)
            .unwrap()
            .engine,
        Engine::ClaudeApi
    );
    let r = rig("claude-api");
    assert_eq!(
        super::super::resolve(&r.cfg, "work", None, Some("claude"), false)
            .unwrap()
            .engine,
        Engine::Claude
    );
}

#[test]
fn a_route_of_one_engine_never_moves() {
    let r = rig("claude");
    r.refused();
    r.claude(100.0, 10);
    assert_eq!(r.engine(), Engine::Claude);
}

/// A job whose protected request names no role or engine (or a sandbox),
/// at turn 1, its identity captured as it started.
fn job_at<'a>(r: &'a Rig, agent: Option<&str>) -> crate::job::Job<'a> {
    let job = crate::job::Job::new(&r.cfg, if agent.is_some() { "vm" } else { "here" });
    fs::create_dir_all(&job.dir).unwrap();
    crate::ask::ensure(&job).unwrap();
    fs::write(
        crate::ask::host_path(&job, "request"),
        json!({"project": "p", "engine": null, "role": null, "app": null, "agent": agent, "owned": false})
            .to_string(),
    )
    .unwrap();
    job.set_turns(1).unwrap();
    let started = super::super::choice(&r.cfg, &job).unwrap();
    crate::retry::Identity::capture_choice(&job, "1", &started).unwrap();
    job
}

/// A run refused for usage is routed again: the turn moves to the credit,
/// kept as its identity (what its next run and a retained retry start
/// on), and back once the credit is short and the subscription isn't.
#[test]
fn a_refused_run_moves_its_turn_and_the_move_is_its_identity() {
    use super::super::{choice, reroute};
    let r = rig(ROUTE);
    let job = job_at(&r, None);
    let started = choice(&r.cfg, &job).unwrap();
    assert_eq!(started.engine, Engine::Claude);
    assert_eq!(
        reroute(&r.cfg, &job, &started).unwrap(),
        None,
        "nothing short"
    );
    r.refused();
    let next = reroute(&r.cfg, &job, &started).unwrap().unwrap();
    assert_eq!(
        (next.engine, next.model.as_deref(), next.effort.as_deref()),
        (Engine::ClaudeApi, Some("api-model"), Some("high"))
    );
    crate::retry::Identity::reroute(&job, "1", &next).unwrap();
    assert_eq!(choice(&r.cfg, &job).unwrap(), next);
    assert_eq!(reroute(&r.cfg, &job, &next).unwrap(), None, "already there");
    // The credit spent while the subscription is back: the turn goes back.
    fs::remove_file(r.cfg.jobs.join(crate::usage::FILE)).unwrap();
    r.spent(15);
    let back = reroute(&r.cfg, &job, &next).unwrap().unwrap();
    assert_eq!(back.engine, Engine::Claude);
    assert_eq!(back.model.as_deref(), Some("sub-model"));
    // A sandbox's turn stays: no key goes into a VM.
    let r = rig(ROUTE);
    let vm = job_at(&r, Some("web-ab12"));
    let started = choice(&r.cfg, &vm).unwrap();
    r.refused();
    assert_eq!(reroute(&r.cfg, &vm, &started).unwrap(), None);
}
