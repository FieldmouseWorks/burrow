use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use burrow_github::Author;
use burrow_github::fake::{Fake, ok};
use serde_json::json;

use super::*;
use crate::config::Relay;
use crate::reason::{Family, WORDS};
use crate::relay::{GitHub, GitHubLabels};

/// What reached the world, in order: `sent KEY` (a request made on the
/// route), `ack KEY` (an acknowledgement seen), `label TARGET`.
type Log = Rc<RefCell<Vec<String>>>;

fn all_reasons(delivers: bool) -> Policy {
    Policy {
        owner_reasons: Kind::ALL.to_vec(),
        delivers,
    }
}

/// The route, with a request per key, the coordinator's acknowledgements
/// and failures to order.
#[derive(Default)]
struct FakeProject {
    log: Log,
    made: BTreeMap<String, u64>,
    acked: BTreeMap<u64, Receipt>,
    fail_send: bool,
    /// Makes the request, then loses the answer.
    lose_answer: bool,
    fail_ack: bool,
    /// The coordinator acknowledges each request as it's made.
    auto_ack: bool,
}

impl FakeProject {
    fn ack(&mut self, key: &str) {
        let n = self.made[key];
        self.acked.insert(
            n,
            Receipt::new(
                "owner".into(),
                7,
                "2026-11-14T05:00:00Z".into(),
                format!("relay#{n}"),
            ),
        );
    }

    fn sent(&self) -> usize {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with("sent"))
            .count()
    }
}

impl Project for FakeProject {
    fn send(&mut self, request: &Request) -> Result<u64, Error> {
        if self.fail_send {
            return Err("the route is down".into());
        }
        if let Some(n) = self.made.get(request.key) {
            return Ok(*n);
        }
        let n = 100 + self.made.len() as u64;
        self.made.insert(request.key.to_owned(), n);
        self.log.borrow_mut().push(format!("sent {}", request.key));
        if self.auto_ack {
            self.ack(request.key);
        }
        if self.lose_answer {
            return Err("the answer was lost".into());
        }
        Ok(n)
    }

    fn acknowledged(&mut self, key: &str, relay: u64) -> Result<Option<Receipt>, Error> {
        if self.fail_ack {
            return Err("can't read the route".into());
        }
        let r = self.acked.get(&relay).cloned();
        if r.is_some() {
            self.log.borrow_mut().push(format!("ack {key}"));
        }
        Ok(r)
    }
}

#[derive(Default)]
struct FakeLabels {
    log: Log,
    on: Vec<(Target, String)>,
    fail: bool,
    /// Puts it on, then fails as if the answer were lost.
    lose_answer: bool,
}

impl Labels for FakeLabels {
    fn add(&mut self, target: &Target, label: &str, _: &Receipt) -> Result<(), Error> {
        if self.fail {
            return Err("GitHub said no".into());
        }
        if !self.on.contains(&(target.clone(), label.to_owned())) {
            self.on.push((target.clone(), label.to_owned()));
        }
        self.log.borrow_mut().push(format!("label {target}"));
        if self.lose_answer {
            return Err("the answer was lost".into());
        }
        Ok(())
    }
}

/// Records on disk whose saves can fail, as a crash between an effect and
/// its record would leave them.
struct Flaky {
    dir: Dir,
    /// Fail this save (0 the first), counting from when it was set.
    fail_at: Option<usize>,
    saves: usize,
}

impl Records for Flaky {
    fn lock(&mut self) -> Result<Held, Error> {
        self.dir.lock()
    }
    fn load(&mut self, key: &str) -> Result<Option<Record>, Error> {
        self.dir.load(key)
    }
    fn save(&mut self, record: &Record) -> Result<(), Error> {
        let n = self.saves;
        self.saves += 1;
        if self.fail_at == Some(n) {
            return Err("crashed before it was kept".into());
        }
        self.dir.save(record)
    }
    fn keys(&mut self) -> Result<Vec<String>, Error> {
        self.dir.keys()
    }
}

struct World {
    _tmp: tempfile::TempDir,
    records: Flaky,
    project: FakeProject,
    labels: FakeLabels,
    policy: Policy,
    log: Log,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let log = Log::default();
        World {
            records: Flaky {
                dir: Dir(tmp.path().join("escalations")),
                fail_at: None,
                saves: 0,
            },
            _tmp: tmp,
            project: FakeProject {
                log: log.clone(),
                ..FakeProject::default()
            },
            labels: FakeLabels {
                log: log.clone(),
                ..FakeLabels::default()
            },
            policy: all_reasons(true),
            log,
        }
    }

    fn run<T>(&mut self, f: impl FnOnce(&mut Escalations) -> T) -> T {
        let mut e = Escalations {
            policy: &self.policy,
            owner_label: "needs-owner",
            records: &mut self.records,
            project: &mut self.project,
            labels: &mut self.labels,
            now: 1_800_000_000,
        };
        f(&mut e)
    }

    fn escalate(&mut self, key: &str) -> Result<Outcome, Error> {
        self.run(|e| {
            e.escalate(
                key,
                &target(),
                "confirm-access",
                "GitHub settings for the Renovate App",
                "Clear the Confirm access prompt in remi's browser.",
            )
        })
    }

    fn pass(&mut self) -> Vec<Passed> {
        self.run(|e| e.pass()).unwrap()
    }

    fn labelled(&self) -> bool {
        !self.labels.on.is_empty()
    }

    /// Never a label before an acknowledgement.
    fn assert_order(&self) {
        let log = self.log.borrow();
        if let Some(first_label) = log.iter().position(|l| l.starts_with("label")) {
            assert!(
                log[..first_label].iter().any(|l| l.starts_with("ack")),
                "a label before any acknowledgement: {log:?}"
            );
        }
    }
}

fn target() -> Target {
    Target::parse("sam/homelab#1239").unwrap()
}

#[test]
fn every_one_way_door_reaches_the_owner() {
    let policy = all_reasons(true);
    for kind in Kind::ALL {
        assert_eq!(
            policy.route(kind.word(), "what exactly"),
            Route::Owner(Reason {
                kind,
                detail: "what exactly".into()
            }),
            "{}",
            kind.word()
        );
    }
    let families: Vec<_> = Kind::ALL.iter().map(|k| k.family()).collect();
    assert_eq!(families.iter().filter(|f| **f == Family::Secret).count(), 3);
    assert_eq!(
        families
            .iter()
            .filter(|f| **f == Family::HumanPrompt)
            .count(),
        3
    );
    assert_eq!(
        families
            .iter()
            .filter(|f| **f == Family::Irreversible)
            .count(),
        6
    );
    assert_eq!(WORDS.to_vec(), Kind::ALL.map(Kind::word).to_vec());
}

#[test]
fn anything_else_goes_to_the_operator() {
    let policy = all_reasons(true);
    // The cases the rule was written for, and the like.
    for word in [
        "hold",
        "why is this on hold",
        "paused",
        "restate-authorization",
        "judgment",
        "owner",
        "approval",
        "merge",
        "",
        "Confirm-Access",
    ] {
        assert!(
            matches!(policy.route(word, "wend#218's hold"), Route::Operator(_)),
            "{word:?}"
        );
    }
    // A door with nothing named, or a document for a detail.
    assert!(matches!(
        policy.route("secret-rotate", "  "),
        Route::Operator(_)
    ));
    assert!(matches!(
        policy.route("secret-rotate", &"x".repeat(501)),
        Route::Operator(_)
    ));
    // A door the machine took off its list.
    let narrow = Policy {
        owner_reasons: vec![Kind::FirmwareFlash],
        delivers: true,
    };
    assert!(matches!(
        narrow.route("secret-delete", "the backup key"),
        Route::Operator(_)
    ));
    assert!(matches!(
        narrow.route("firmware-flash", "the NVMe drives"),
        Route::Owner(_)
    ));
    // With no route to the project, nothing waits on the owner unheard.
    assert!(matches!(
        all_reasons(false).route("firmware-flash", "the NVMe drives"),
        Route::Operator(_)
    ));
}

#[test]
fn the_operators_requests_touch_nothing() {
    let mut w = World::new();
    let out = w
        .run(|e| {
            e.escalate(
                "remi-1008",
                &target(),
                "hold",
                "what is wend#218's hold waiting for",
                "Is this paused?",
            )
        })
        .unwrap();
    assert!(matches!(out, Outcome::Operator { .. }), "{out:?}");
    assert!(w.log.borrow().is_empty());
    assert!(w.records.keys().unwrap().is_empty());
}

#[test]
fn the_label_waits_for_the_acknowledgement() {
    let mut w = World::new();
    assert_eq!(
        w.escalate("renovate-confirm").unwrap(),
        Outcome::Pending { relay: Some(100) }
    );
    assert!(!w.labelled());
    // Asked again before it's acknowledged: still one request, no label.
    assert_eq!(
        w.escalate("renovate-confirm").unwrap(),
        Outcome::Pending { relay: Some(100) }
    );
    assert!(
        w.pass()[0]
            .1
            .as_ref()
            .is_ok_and(|o| matches!(o, Outcome::Pending { .. }))
    );
    assert!(!w.labelled());
    w.project.ack("renovate-confirm");
    let passed = w.pass();
    assert!(
        matches!(passed[0].1, Ok(Outcome::Escalated { .. })),
        "{passed:?}"
    );
    assert_eq!(w.labels.on, vec![(target(), "needs-owner".to_owned())]);
    // Done is done: nothing sent or labelled again.
    assert!(w.pass().is_empty());
    assert!(matches!(
        w.escalate("renovate-confirm").unwrap(),
        Outcome::Escalated { .. }
    ));
    assert_eq!(w.project.sent(), 1);
    assert_eq!(
        w.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with("label"))
            .count(),
        1
    );
    w.assert_order();
}

#[test]
fn a_failed_delivery_leaves_no_label_and_is_carried_on() {
    let mut w = World::new();
    w.project.fail_send = true;
    assert!(w.escalate("k").is_err());
    assert!(!w.labelled());
    assert_eq!(w.project.sent(), 0);
    // Kept: a pass after the route comes back carries it on.
    w.project.fail_send = false;
    assert!(matches!(w.pass()[0].1, Ok(Outcome::Pending { .. })));
    w.project.fail_ack = true;
    assert!(w.pass()[0].1.is_err());
    assert!(!w.labelled());
    w.project.fail_ack = false;
    w.project.ack("k");
    assert!(matches!(w.pass()[0].1, Ok(Outcome::Escalated { .. })));
    assert_eq!(w.project.sent(), 1);
    w.assert_order();
}

#[test]
fn a_failed_label_is_tried_again_without_a_second_notice() {
    let mut w = World::new();
    w.escalate("k").unwrap();
    w.project.ack("k");
    w.labels.fail = true;
    assert!(w.pass()[0].1.is_err());
    assert!(!w.labelled());
    w.labels.fail = false;
    assert!(matches!(w.pass()[0].1, Ok(Outcome::Escalated { .. })));
    assert!(w.labelled());
    assert_eq!(w.project.sent(), 1);
    w.assert_order();
}

#[test]
fn a_lost_answer_is_found_again_not_sent_again() {
    let mut w = World::new();
    w.project.lose_answer = true;
    assert!(w.escalate("k").is_err());
    w.project.lose_answer = false;
    assert!(matches!(
        w.pass()[0].1,
        Ok(Outcome::Pending { relay: Some(100) })
    ));
    w.project.ack("k");
    w.labels.lose_answer = true;
    assert!(w.pass()[0].1.is_err());
    w.labels.lose_answer = false;
    assert!(matches!(w.pass()[0].1, Ok(Outcome::Escalated { .. })));
    assert_eq!(w.project.sent(), 1);
    assert_eq!(w.labels.on.len(), 1);
    w.assert_order();
}

/// A crash after each effect, before its record: whichever, the next pass
/// ends with one request, the label on, and no label before its
/// acknowledgement.
#[test]
fn a_crash_between_any_two_effects_recovers_without_a_second_notice() {
    // Saves: 0 the first record, 1 relayed, 2 delivered, 3 labelled.
    for crash in 0..4 {
        let mut w = World::new();
        // Acknowledged as soon as it's made, so one call goes all the way.
        w.project.auto_ack = true;
        w.records.fail_at = Some(crash);
        assert!(w.escalate("k").is_err(), "crash {crash}");
        if crash == 0 {
            // Not even kept: nothing was sent, and the producer was told.
            assert!(w.log.borrow().is_empty());
            assert!(w.records.keys().unwrap().is_empty());
            continue;
        }
        assert_eq!(w.labelled(), crash == 3, "crash {crash}");
        // The next process, over the same records.
        w.records.fail_at = None;
        let passed = w.pass();
        assert!(
            matches!(passed[..], [(_, Ok(Outcome::Escalated { .. }))]),
            "crash {crash}: {passed:?}"
        );
        assert!(w.labelled(), "crash {crash}");
        assert_eq!(w.project.sent(), 1, "crash {crash}");
        assert_eq!(w.labels.on.len(), 1, "crash {crash}");
        w.assert_order();
        assert!(w.pass().is_empty());
    }
}

#[test]
fn a_key_is_one_escalation() {
    let mut w = World::new();
    w.escalate("k").unwrap();
    let other = Target::parse("sam/homelab#7").unwrap();
    let again = w.run(|e| e.escalate("k", &other, "confirm-access", "x", "y"));
    assert!(again.unwrap_err().contains("already"));
    let wrong = w.run(|e| e.escalate("../k", &target(), "confirm-access", "x", "y"));
    assert!(wrong.is_err());
    assert!(
        w.run(|e| e.escalate("k2", &target(), "confirm-access", "x", " "))
            .is_err()
    );
}

#[test]
fn targets_are_owner_name_number() {
    assert_eq!(
        Target::parse("FieldmouseWorks/remi#1239").unwrap(),
        Target {
            repo: "FieldmouseWorks/remi".into(),
            number: 1239
        }
    );
    for bad in [
        "remi#1", "a/b/c#1", "a/b#0", "a/b#x", "a/b", "/b#1", "a b/c#1",
    ] {
        assert!(Target::parse(bad).is_err(), "{bad}");
    }
}

fn relay() -> Relay {
    Relay {
        repo: "sam/homelab".into(),
        label: "escalation".into(),
        acknowledgers: vec![Author {
            login: "sam".into(),
            id: 42,
        }],
    }
}

fn request_parts() -> (Target, Reason) {
    (
        target(),
        Reason {
            kind: Kind::ConfirmAccess,
            detail: "the Renovate App's settings".into(),
        },
    )
}

#[test]
fn the_relay_finds_its_request_before_it_makes_one() {
    let relay = relay();
    let (target, reason) = request_parts();
    let request = Request {
        key: "k",
        target: &target,
        reason: &reason,
        ask: "Clear it.",
        since: 1_800_000_000,
    };
    // One made already, its answer lost.
    let mut api = Fake::default();
    api.on(
        "GET",
        "repos/sam/homelab/issues?state=all",
        vec![ok(
            200,
            json!([
                {"number": 5, "title": "[escalation k2] other", "pull_request": null},
                {"number": 9, "title": "[escalation k] GitHub's Confirm access prompt: sam/homelab#1239"},
                {"number": 3, "title": "[escalation k] a PR", "pull_request": {}},
            ]),
            &[],
        )],
    );
    let mut gh = GitHub {
        api: &mut api,
        relay: &relay,
        owner: "Sam",
        owner_label: "needs-owner",
    };
    assert_eq!(gh.send(&request).unwrap(), 9);
    assert!(api.asked("POST").is_empty());
    // None yet: it's made, under its label, saying how it's acknowledged.
    let mut api = Fake::default();
    api.on(
        "GET",
        "repos/sam/homelab/issues?state=all",
        vec![ok(200, json!([]), &[])],
    )
    .on(
        "POST",
        "repos/sam/homelab/labels",
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "POST",
        "repos/sam/homelab/issues",
        vec![ok(201, json!({"number": 12}), &[])],
    );
    let mut gh = GitHub {
        api: &mut api,
        relay: &relay,
        owner: "Sam",
        owner_label: "needs-owner",
    };
    assert_eq!(gh.send(&request).unwrap(), 12);
    let since = &api.asked("GET")[0].1;
    assert!(since.contains("since=2027-01-15T07%3A59%3A00Z"), "{since}");
    let made = api.asked("POST")[1].2.clone().unwrap();
    assert!(
        made["title"]
            .as_str()
            .unwrap()
            .starts_with("[escalation k] ")
    );
    assert_eq!(made["labels"], json!(["escalation"]));
    let body = made["body"].as_str().unwrap();
    assert!(body.contains("\ndelivered k\n"), "{body}");
    assert!(
        body.contains("https://github.com/sam/homelab/issues/1239"),
        "{body}"
    );
}

#[test]
fn only_an_acknowledgers_word_is_delivery() {
    let relay = relay();
    let ask = |comments: serde_json::Value| {
        let mut api = Fake::default();
        api.on(
            "GET",
            "repos/sam/homelab/issues/9/comments",
            vec![ok(200, comments, &[])],
        );
        let mut gh = GitHub {
            api: &mut api,
            relay: &relay,
            owner: "Sam",
            owner_label: "needs-owner",
        };
        gh.acknowledged("k", 9).unwrap()
    };
    let by = |login: &str, id: u64, body: &str| {
        json!({"user": {"login": login, "id": id}, "body": body,
               "created_at": "2026-11-14T05:00:00Z", "html_url": "https://github.com/c/1"})
    };
    // The issue alone, or anyone else saying it, isn't delivery.
    assert_eq!(ask(json!([])), None);
    assert_eq!(ask(json!([by("sam", 99, "delivered k")])), None);
    assert_eq!(ask(json!([by("mallory", 7, "delivered k")])), None);
    assert_eq!(
        ask(json!([{"user": {"login": "sam"}, "body": "delivered k"}])),
        None
    );
    // Nor the acknowledger saying something else.
    for body in [
        "delivered k2",
        "delivered",
        "delivered k please",
        "I'll look",
        "seen k",
    ] {
        assert_eq!(ask(json!([by("sam", 42, body)])), None, "{body}");
    }
    // The acknowledger, by id, even renamed, and in a code span.
    for body in [
        "delivered k",
        "\n `delivered k` \nRaised in the project.",
        "```\ndelivered k\n```",
    ] {
        let r = ask(json!([
            by("sam", 99, "delivered k"),
            by("sam-renamed", 42, body)
        ]));
        let r = r.unwrap_or_else(|| panic!("{body:?}"));
        assert_eq!((r.by(), r.by_id()), ("sam-renamed", 42));
        assert_eq!(r.url(), "https://github.com/c/1");
    }
}

#[test]
fn the_label_counts_once_github_kept_it() {
    let receipt = Receipt::new("sam".into(), 42, "t".into(), "u".into());
    let mut api = Fake::default();
    api.on(
        "POST",
        "repos/sam/homelab/issues/1239/labels",
        vec![ok(200, json!([{"name": "bug"}]), &[])],
    );
    let mut labels = GitHubLabels { api: &mut api };
    assert!(labels.add(&target(), "needs-owner", &receipt).is_err());
    let mut api = Fake::default();
    api.on(
        "POST",
        "repos/sam/homelab/issues/1239/labels",
        vec![ok(200, json!([{"name": "Needs-Owner"}]), &[])],
    );
    let mut labels = GitHubLabels { api: &mut api };
    labels.add(&target(), "needs-owner", &receipt).unwrap();
    assert_eq!(
        api.asked("POST")[0].2,
        Some(json!({"labels": ["needs-owner"]}))
    );
}

/// The whole operation over the GitHub route, from a request to its label,
/// with GitHub scripted: the label is asked for only after the
/// acknowledgement.
#[test]
fn over_github_the_label_follows_the_acknowledgement() {
    let tmp = tempfile::tempdir().unwrap();
    let relay = relay();
    let policy = all_reasons(true);
    let mut records = Dir(tmp.path().to_owned());
    let mut api = Fake::default();
    api.on("GET", "repos/sam/homelab/issues?state=all", vec![ok(200, json!([]), &[])])
        .on("POST", "repos/sam/homelab/labels", vec![ok(201, json!({}), &[])])
        .on(
            "POST",
            "repos/sam/homelab/issues",
            vec![ok(201, json!({"number": 12}), &[])],
        )
        .on(
            "GET",
            "repos/sam/homelab/issues/12/comments",
            vec![
                ok(200, json!([]), &[]),
                ok(
                    200,
                    json!([{"user": {"login": "sam", "id": 42}, "body": "delivered k",
                            "created_at": "2026-11-14T05:00:00Z", "html_url": "https://github.com/c/2"}]),
                    &[],
                ),
            ],
        );
    let mut label_api = Fake::default();
    label_api.on(
        "POST",
        "repos/sam/homelab/issues/1239/labels",
        vec![ok(200, json!([{"name": "needs-owner"}]), &[])],
    );
    let mut project = GitHub {
        api: &mut api,
        relay: &relay,
        owner: "Sam",
        owner_label: "needs-owner",
    };
    let mut labels = GitHubLabels {
        api: &mut label_api,
    };
    let mut e = Escalations {
        policy: &policy,
        owner_label: "needs-owner",
        records: &mut records,
        project: &mut project,
        labels: &mut labels,
        now: 1_800_000_000,
    };
    let first = e
        .escalate(
            "k",
            &target(),
            "confirm-access",
            "the Renovate App",
            "Clear it.",
        )
        .unwrap();
    assert_eq!(first, Outcome::Pending { relay: Some(12) });
    let passed = e.pass().unwrap();
    let Ok(Outcome::Escalated { receipt }) = &passed[0].1 else {
        panic!("{passed:?}")
    };
    assert_eq!(receipt.url(), "https://github.com/c/2");
    assert_eq!(label_api.asked("POST").len(), 1);
    // One request made, ever.
    assert_eq!(
        api.asked("POST")
            .iter()
            .filter(|a| a.1 == "repos/sam/homelab/issues")
            .count(),
        1
    );
}
