use super::*;
use crate::config::{Config, Overrides};
use crate::pass::Out;
use crate::testing::{Hub, States, T, answer, config, look};
use serde_json::json;

const LABELS: [&str; 6] = [
    "hold",
    "needs-owner",
    "first",
    "waiting: release age",
    "waiting: proof slot",
    "waiting: reviewer",
];
const LISTING: &str = "repos/sam/r/rulesets?per_page=100";
const BRANCHES: &str = "repos/sam/r/branches?protected=true&per_page=100";
const PROTECTION: &str = "repos/sam/r/branches/main/protection/required_status_checks";
const SETTLED: [(&str, u64); 1] = [("review-settled", 7)];

fn public() -> Repo {
    Repo {
        private: false,
        empty: false,
    }
}

fn checks(of: &[(&str, u64)]) -> Value {
    of.iter()
        .map(|(c, app)| json!({"context": c, "integration_id": app}))
        .collect()
}

fn owner_bypass(mode: &str) -> Value {
    json!([{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": mode}])
}

/// A branch ruleset of the repo's own, with these rules.
fn ruleset(name: &str, rules: Value, bypass: Value) -> Value {
    json!({"name": name, "target": "branch", "source_type": "Repository", "enforcement": "active",
           "bypass_actors": bypass,
           "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
           "rules": rules})
}

fn required_rule(strict: bool, required: Value) -> Value {
    json!({"type": "required_status_checks", "parameters": {
        "strict_required_status_checks_policy": strict, "required_status_checks": required}})
}

/// The baseline as the steward makes it, required checks and all.
fn full_baseline(cfg: &Config) -> Value {
    let mut b = baseline(&cfg.settle, cfg.settle.defaults);
    b["source_type"] = "Repository".into();
    b
}

/// sam/r as GitHub has it when there's nothing to change: its settings on,
/// every label there, its own baseline as the steward makes it, and no
/// protected branch.
fn github(cfg: &Config) -> Hub {
    let mut hub = Hub::default();
    let info = json!({"allow_auto_merge": true, "delete_branch_on_merge": true, "allow_squash_merge": true,
                      "security_and_analysis": {"secret_scanning": {"status": "enabled"},
                                                "secret_scanning_push_protection": {"status": "enabled"}}});
    hub.on("GET", "repos/sam/r$", answer(200, info.clone()));
    hub.on("PATCH", "repos/sam/r$", answer(200, info));
    hub.on("PATCH", "repos/sam/r/labels/", answer(200, json!({})));
    hub.on("POST", "repos/sam/r/labels$", answer(201, json!({})));
    hub.on(
        "PUT",
        "repos/sam/r/vulnerability-alerts$",
        answer(204, Value::Null),
    );
    let listed =
        json!([{"id": 1, "name": "baseline", "target": "branch", "source_type": "Repository"}]);
    hub.on("GET", &format!("{LISTING}$"), answer(200, listed));
    hub.on(
        "GET",
        "repos/sam/r/rulesets/1$",
        answer(200, full_baseline(cfg)),
    );
    hub.on("POST", "repos/sam/r/rulesets$", answer(201, json!({})));
    hub.on("PUT", "repos/sam/r/rulesets/", answer(200, json!({})));
    hub.on("GET", &format!("{BRANCHES}$"), answer(200, json!([])));
    hub.on("PATCH", "repos/sam/r/branches/", answer(200, json!({})));
    hub
}

/// A protected branch whose classic protection requires these checks.
fn protected(hub: &mut Hub, name: &str, got: Value) {
    let enforced =
        json!({"protection": {"required_status_checks": {"enforcement_level": "everyone"}}});
    let path = format!("repos/sam/r/branches/{}", encode(name));
    hub.on("GET", &format!("{path}$"), answer(200, enforced));
    let checks = format!("{path}/protection/required_status_checks$");
    hub.on("GET", &checks, answer(200, got));
}

/// Settle sam/r `at` seconds after T: whether it was due, and what it
/// says back.
fn settle_at(cfg: &Config, hub: &mut Hub, repo: &Repo, at: f64) -> (bool, Out) {
    let mut jobs = States::new("running");
    let (out, due) = look(cfg, hub, &mut jobs, at, |p| {
        Ok::<_, String>(settle(p, &cfg.settle, repo))
    });
    (due.unwrap(), out)
}

fn kept(cfg: &Config) -> Value {
    let text = std::fs::read_to_string(cfg.state_dir.join("repo-sam_r.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn keep(cfg: &Config, st: Value) {
    std::fs::create_dir_all(&cfg.state_dir).unwrap();
    std::fs::write(cfg.state_dir.join("repo-sam_r.json"), st.to_string()).unwrap();
}

/// What it changed: each call but a GET, with its body.
fn writes(hub: &Hub) -> Vec<(&str, &str, Option<&Value>)> {
    hub.asked
        .iter()
        .filter(|a| a.0 != "GET")
        .map(|a| (a.0.as_str(), a.1.as_str(), a.3.as_ref()))
        .collect()
}

/// The labels it made or brought in line, with what each says.
fn made(hub: &Hub) -> Vec<(String, String)> {
    let mut got: Vec<(String, String)> = hub
        .asked
        .iter()
        .filter(|a| a.0 == "PATCH" && a.1.starts_with("repos/sam/r/labels/"))
        .map(|a| {
            let name = a.1.trim_start_matches("repos/sam/r/labels/");
            let said = a.3.as_ref().unwrap()["description"].as_str().unwrap();
            (
                name.replace("%3A", ":").replace("%20", " "),
                said.to_string(),
            )
        })
        .collect();
    got.sort();
    got
}

fn names(made: &[(String, String)]) -> Vec<&str> {
    made.iter().map(|(n, _)| n.as_str()).collect()
}

fn sorted_labels() -> Vec<&'static str> {
    let mut l = LABELS.to_vec();
    l.sort();
    l
}

/// The writes to its rules: rulesets and classic protection.
fn ruled(hub: &Hub) -> Vec<(&str, &str, Option<&Value>)> {
    writes(hub)
        .into_iter()
        .filter(|w| w.1.contains("rulesets") || w.1.contains("protection"))
        .collect()
}

#[test]
fn labels_are_made_daily_and_at_once_when_one_is_new() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    keep(&cfg, json!({"settings": T, "app": 7}));
    let (due, out) = settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(!due && out.failed.is_empty(), "{out:?}");
    let got = made(&hub);
    assert_eq!(names(&got), sorted_labels());
    // A waiting label says what the PR waits on, and that its status says
    // more, within the 100 characters GitHub takes.
    assert!(got.iter().all(|(_, d)| d.chars().count() <= 100), "{got:?}");
    let waiting = got.iter().filter(|(n, _)| n.starts_with("waiting: "));
    assert!(
        waiting.clone().count() == 3
            && waiting
                .clone()
                .all(|(_, d)| d.contains("review-settled says"))
    );
    assert!(got.contains(&(
        "needs-owner".into(),
        "Waits for sam; take the label off to let the steward carry on".into()
    )));
    assert_eq!(kept(&cfg)["labels"], json!(LABELS));
    // Made, they're not made again until the day's settings.
    let (_, _) = settle_at(&cfg, &mut hub, &public(), 60.0);
    assert!(hub.asked.is_empty(), "{:?}", hub.asked);
    let (due, _) = settle_at(&cfg, &mut hub, &public(), DAY);
    assert!(due);
    assert_eq!(names(&made(&hub)), sorted_labels());
    // A repo that made the labels before there were waiting ones makes them
    // at once, not a day on.
    let mut st = kept(&cfg);
    st["labels"] = json!(["hold", "needs-owner", "first"]);
    keep(&cfg, st);
    let (due, _) = settle_at(&cfg, &mut hub, &public(), DAY + 60.0);
    assert!(!due);
    assert_eq!(names(&made(&hub)), sorted_labels());
}

#[test]
fn a_label_says_no_more_than_github_takes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.settle.owner = "o".repeat(80);
    let mut hub = github(&cfg);
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let said = made(&hub)
        .into_iter()
        .find(|(n, _)| n == "needs-owner")
        .unwrap()
        .1;
    let want: String = format!("Waits for {}; take the label off", cfg.settle.owner)
        .chars()
        .take(100)
        .collect();
    assert_eq!(said, want);
}

#[test]
fn a_label_github_lacks_is_made_and_one_it_wont_take_is_tried_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    keep(&cfg, json!({"settings": T, "app": 7}));
    hub.on(
        "PATCH",
        "repos/sam/r/labels/first$",
        answer(404, json!({"message": "Not Found"})),
    );
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let posted: Vec<_> = writes(&hub).into_iter().filter(|w| w.0 == "POST").collect();
    assert_eq!(
        posted,
        [(
            "POST",
            "repos/sam/r/labels",
            Some(&json!({"name": "first", "color": "0e8a16",
                         "description": "Goes ahead of the PRs without it in the line for proofs that take the machine"}))
        )]
    );
    assert_eq!(kept(&cfg)["labels"], json!(LABELS));
    // One GitHub won't take: none are kept as made, so all are made again
    // on the next look, while the others still were this time.
    keep(&cfg, json!({"settings": T, "app": 7}));
    hub.on(
        "POST",
        "repos/sam/r/labels$",
        answer(422, json!({"message": "Validation Failed"})),
    );
    settle_at(&cfg, &mut hub, &public(), 0.0);
    assert_eq!(made(&hub).len(), 6);
    assert_eq!(kept(&cfg).get("labels"), None);
    settle_at(&cfg, &mut hub, &public(), 60.0);
    assert_eq!(made(&hub).len(), 6);
}

#[test]
fn a_repos_settings_are_turned_on_and_auto_merge_github_leaves_off_isnt_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on(
        "GET",
        "repos/sam/r$",
        answer(200, json!({"delete_branch_on_merge": true})),
    );
    // GitHub takes it, and leaves it off.
    hub.on(
        "PATCH",
        "repos/sam/r$",
        answer(200, json!({"allow_auto_merge": false})),
    );
    let (due, out) = settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(due && out.failed.is_empty(), "{out:?}");
    let patched: Vec<_> = writes(&hub)
        .into_iter()
        .filter(|w| w.1 == "repos/sam/r")
        .collect();
    let on = json!({"status": "enabled"});
    assert_eq!(
        patched,
        [(
            "PATCH",
            "repos/sam/r",
            Some(
                &json!({"allow_auto_merge": true, "allow_squash_merge": true,
                         "security_and_analysis": {"secret_scanning": on, "secret_scanning_push_protection": on}})
            )
        )]
    );
    assert!(writes(&hub).contains(&("PUT", "repos/sam/r/vulnerability-alerts", None)));
    assert_eq!(kept(&cfg)["autoMergeRefused"], true);
    // A day on, it's not asked for again; a private repo has no secret
    // scanning to turn on.
    let private = Repo {
        private: true,
        empty: false,
    };
    settle_at(&cfg, &mut hub, &private, DAY);
    let patched: Vec<_> = writes(&hub)
        .into_iter()
        .filter(|w| w.1 == "repos/sam/r")
        .collect();
    assert_eq!(
        patched,
        [(
            "PATCH",
            "repos/sam/r",
            Some(&json!({"allow_squash_merge": true}))
        )]
    );
    // All on, nothing's patched, and the vulnerability alerts are still put.
    hub.on(
        "GET",
        "repos/sam/r$",
        answer(200, json!({"allow_auto_merge": true, "delete_branch_on_merge": true, "allow_squash_merge": true})),
    );
    settle_at(&cfg, &mut hub, &private, 2.0 * DAY);
    assert!(!writes(&hub).iter().any(|w| w.1 == "repos/sam/r"));
    assert!(writes(&hub).contains(&("PUT", "repos/sam/r/vulnerability-alerts", None)));
}

/// gates: a ruleset of the repo's own besides the baseline, which requires
/// up-to-date branches and another check.
fn gates(strict: bool, required: Value) -> Value {
    ruleset(
        "gates",
        json!([{"type": "deletion"}, required_rule(strict, required)]),
        owner_bypass("always"),
    )
}

/// The listing, in two pages: the baseline, then gates, a tag ruleset and
/// the org's.
fn listed(hub: &mut Hub) {
    let mut first = answer(
        200,
        json!([{"id": 1, "name": "baseline", "target": "branch", "source_type": "Repository"}]),
    );
    first.next = Some(format!("{LISTING}&page=2"));
    hub.on("GET", &format!("{LISTING}$"), first);
    let rest = json!([{"id": 2, "name": "gates", "target": "branch", "source_type": "Repository"},
                      {"id": 3, "name": "tags", "target": "tag", "source_type": "Repository"},
                      {"id": 4, "name": "org", "target": "branch", "source_type": "Organization"}]);
    hub.on("GET", &format!("{LISTING}&page=2$"), answer(200, rest));
    let main = json!([{"name": "main"}]);
    hub.on("GET", &format!("{BRANCHES}$"), answer(200, main));
}

/// What a ruleset goes back as: the whole of it but where GitHub lists it.
fn back(rs: &Value) -> Value {
    let mut b = rs.clone();
    b.as_object_mut().unwrap().remove("source_type");
    b
}

#[test]
fn up_to_date_branches_go_off_wherever_the_repo_has_rules() {
    // Its own rulesets and every protected branch's classic protection.
    // Tag rulesets, the org's, and ones that ask for neither are left as
    // they are, and so are other checks.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    listed(&mut hub);
    let rs = gates(true, json!([{"context": "gate"}]));
    hub.on("GET", "repos/sam/r/rulesets/2$", answer(200, rs.clone()));
    protected(&mut hub, "main", json!({"strict": true}));
    let (_, out) = settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(out.failed.is_empty(), "{out:?}");
    // The whole ruleset goes back with only the one setting changed: its
    // name, branches and bypass are what they were.
    let mut want = back(&rs);
    want["rules"][1]["parameters"]["strict_required_status_checks_policy"] = false.into();
    assert_eq!(
        ruled(&hub),
        [
            ("PUT", "repos/sam/r/rulesets/2", Some(&want)),
            ("PATCH", PROTECTION, Some(&json!({"strict": false}))),
        ]
    );
    assert_eq!(kept(&cfg)["app"], 7);
}

#[test]
fn the_stewards_status_is_required_from_its_app_only() {
    // In a ruleset (integration_id) and in classic protection (app_id). The
    // other checks keep their sources: one any App may set stays so
    // (GitHub lists it with no app_id, and takes -1 for it).
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    listed(&mut hub);
    let required = json!([{"context": "gate"}, {"context": "review-settled"}, {"context": "ci", "integration_id": 15368}]);
    let rs = gates(false, required);
    hub.on("GET", "repos/sam/r/rulesets/2$", answer(200, rs.clone()));
    let classic = json!({"strict": false, "checks": [{"context": "gate", "app_id": null},
                                                     {"context": "review-settled", "app_id": null},
                                                     {"context": "ci", "app_id": 15368}]});
    protected(&mut hub, "main", classic);
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let mut want = back(&rs);
    want["rules"][1]["parameters"]["required_status_checks"][1]["integration_id"] = 7.into();
    let pinned = json!({"checks": [{"context": "gate", "app_id": -1}, {"context": "review-settled", "app_id": 7},
                                   {"context": "ci", "app_id": 15368}]});
    assert_eq!(
        ruled(&hub),
        [
            ("PUT", "repos/sam/r/rulesets/2", Some(&want)),
            ("PATCH", PROTECTION, Some(&pinned)),
        ]
    );
    assert_eq!(kept(&cfg)["app"], 7);
    // Once pinned, nothing to change.
    let mut rs = back(&want);
    rs["source_type"] = "Repository".into();
    hub.on("GET", "repos/sam/r/rulesets/2$", answer(200, rs));
    let classic = json!({"strict": false, "checks": [{"context": "gate", "app_id": -1},
                                                     {"context": "review-settled", "app_id": 7}]});
    protected(&mut hub, "main", classic);
    settle_at(&cfg, &mut hub, &public(), DAY);
    assert_eq!(ruled(&hub), []);
    // Pinned to another App (the steward's old one, say), it's moved to
    // this one.
    let classic = json!({"strict": false, "checks": [{"context": "gate", "app_id": -1},
                                                     {"context": "review-settled", "app_id": 99}]});
    protected(&mut hub, "main", classic);
    settle_at(&cfg, &mut hub, &public(), 2.0 * DAY);
    let pinned = json!({"checks": [{"context": "gate", "app_id": -1}, {"context": "review-settled", "app_id": 7}]});
    assert_eq!(ruled(&hub), [("PATCH", PROTECTION, Some(&pinned))]);
}

#[test]
fn another_app_is_due_at_once_and_settled_only_once_its_checks_are_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    listed(&mut hub);
    hub.on(
        "GET",
        "repos/sam/r/rulesets/2$",
        answer(200, gates(false, checks(&SETTLED))),
    );
    protected(&mut hub, "main", json!({"strict": false}));
    keep(&cfg, json!({"settings": T, "app": 6, "labels": LABELS}));
    // GitHub not saying what the protection's checks are: settled for no
    // App, and still due.
    let path = format!("{PROTECTION}$");
    hub.on(
        "GET",
        &path,
        answer(500, json!({"message": "Server Error"})),
    );
    let (due, _) = settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(due);
    assert!(writes(&hub).contains(&("PUT", "repos/sam/r/vulnerability-alerts", None)));
    assert_eq!(kept(&cfg).get("app"), None);
    hub.on("GET", &path, answer(200, json!({"strict": false})));
    let (due, _) = settle_at(&cfg, &mut hub, &public(), 60.0);
    assert!(due);
    assert_eq!(kept(&cfg)["app"], 7);
    let (due, _) = settle_at(&cfg, &mut hub, &public(), 120.0);
    assert!(!due && hub.asked.is_empty(), "{:?}", hub.asked);
}

#[test]
fn a_plan_with_no_rules_has_none_to_pin_and_rules_github_wont_list_arent_pinned() {
    let free = answer(
        403,
        json!({"message": "Upgrade to GitHub Pro or make this repository public to enable this feature."}),
    );
    let down = answer(502, json!({"message": "Bad Gateway"}));
    for (private, said, pinned) in [
        (true, free, true),
        (true, down.clone(), false),
        (false, down, false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(tmp.path());
        let mut hub = github(&cfg);
        hub.on("GET", &format!("{LISTING}$"), said);
        keep(&cfg, json!({"settings": 0, "app": 7}));
        let repo = Repo {
            private,
            empty: false,
        };
        let (due, out) = settle_at(&cfg, &mut hub, &repo, 0.0);
        assert!(due && out.failed.is_empty(), "{out:?}");
        assert_eq!(kept(&cfg).get("app").is_some(), pinned, "{private}");
    }
}

#[test]
fn a_baseline_github_didnt_take_isnt_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on("GET", &format!("{LISTING}$"), answer(200, json!([])));
    hub.on(
        "POST",
        "repos/sam/r/rulesets$",
        answer(422, json!({"message": "Validation Failed"})),
    );
    keep(&cfg, json!({"settings": 0, "app": 7}));
    settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(writes(&hub).iter().any(|w| w.1 == "repos/sam/r/rulesets"));
    assert_eq!(kept(&cfg).get("app"), None);
}

#[test]
fn a_repo_set_to_have_no_baseline_still_has_its_own_rules_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    cfg.settle.repos.insert(
        "sam/r".into(),
        Overrides {
            ruleset: Some(false),
            ..Overrides::default()
        },
    );
    let mut hub = github(&cfg);
    let listed =
        json!([{"id": 2, "name": "gates", "target": "branch", "source_type": "Repository"}]);
    hub.on("GET", &format!("{LISTING}$"), answer(200, listed));
    hub.on(
        "GET",
        "repos/sam/r/rulesets/2$",
        answer(200, gates(true, checks(&SETTLED))),
    );
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let ruled: Vec<_> = ruled(&hub).into_iter().map(|w| (w.0, w.1)).collect();
    assert_eq!(ruled, [("PUT", "repos/sam/r/rulesets/2")]);
}

#[test]
fn every_protected_branch_is_kept_by_its_encoded_name() {
    // Not just the default one's: a PR can be merged into any of them.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    let mut first = answer(200, json!([{"name": "main"}, {"name": "release/1.0"}]));
    first.next = Some(format!("{BRANCHES}&page=2"));
    hub.on("GET", &format!("{BRANCHES}$"), first);
    hub.on(
        "GET",
        &format!("{BRANCHES}&page=2$"),
        answer(200, json!([{"name": "old"}])),
    );
    protected(&mut hub, "main", json!({"strict": true}));
    let anyone =
        json!({"strict": false, "checks": [{"context": "review-settled", "app_id": null}]});
    protected(&mut hub, "release/1.0", anyone.clone());
    protected(&mut hub, "old", anyone);
    let mut jobs = States::new("running");
    let (_, kept) = look(&cfg, &mut hub, &mut jobs, 0.0, |p| {
        Ok::<_, String>(keep_protection(p, &cfg.settle))
    });
    assert!(kept.unwrap());
    let pinned = json!({"checks": [{"context": "review-settled", "app_id": 7}]});
    assert_eq!(
        writes(&hub),
        [
            ("PATCH", PROTECTION, Some(&json!({"strict": false}))),
            (
                "PATCH",
                "repos/sam/r/branches/release%2F1.0/protection/required_status_checks",
                Some(&pinned)
            ),
            (
                "PATCH",
                "repos/sam/r/branches/old/protection/required_status_checks",
                Some(&pinned)
            ),
        ]
    );
    // The protected branches not listed: nothing's known.
    hub.on("GET", &format!("{BRANCHES}$"), answer(502, json!({})));
    let (_, kept) = look(&cfg, &mut hub, &mut jobs, 0.0, |p| {
        Ok::<_, String>(keep_protection(p, &cfg.settle))
    });
    assert!(!kept.unwrap());
}

#[test]
fn a_rule_github_wont_give_or_take_leaves_the_repo_unpinned() {
    // Its other rules are still kept, and its settings are due again on
    // the next pass.
    type Break = fn(&mut Hub);
    let breaks: [(&str, Break); 4] = [
        ("a ruleset it won't give", |hub| {
            hub.on(
                "GET",
                "repos/sam/r/rulesets/2$",
                answer(404, json!({"message": "Not Found"})),
            );
        }),
        ("a ruleset it won't take", |hub| {
            hub.on(
                "PUT",
                "repos/sam/r/rulesets/2$",
                answer(422, json!({"message": "Validation Failed"})),
            );
        }),
        ("protection it won't take", |hub| {
            let path = "repos/sam/r/branches/main/protection/required_status_checks$";
            hub.on(
                "PATCH",
                path,
                answer(422, json!({"message": "Validation Failed"})),
            );
        }),
        ("the first branch's protection it won't give", |hub| {
            hub.on(
                "GET",
                "repos/sam/r/branches/main$",
                answer(404, json!({"message": "Not Found"})),
            );
        }),
    ];
    for (what, broken) in breaks {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = config(tmp.path());
        let mut hub = github(&cfg);
        listed(&mut hub);
        hub.on(
            "GET",
            "repos/sam/r/rulesets/2$",
            answer(200, gates(true, checks(&SETTLED))),
        );
        let main_and_old = json!([{"name": "main"}, {"name": "old"}]);
        hub.on("GET", &format!("{BRANCHES}$"), answer(200, main_and_old));
        protected(&mut hub, "main", json!({"strict": true}));
        protected(&mut hub, "old", json!({"strict": true}));
        broken(&mut hub);
        keep(
            &cfg,
            json!({"settings": T, "app": 7, "labels": LABELS, "train": false}),
        );
        let (due, out) = settle_at(&cfg, &mut hub, &public(), DAY);
        assert!(due && out.failed.is_empty(), "{what}: {out:?}");
        let old = "repos/sam/r/branches/old/protection/required_status_checks";
        assert!(writes(&hub).iter().any(|w| w.1 == old), "{what}");
        assert_eq!(kept(&cfg).get("app"), None, "{what}");
        let (due, _) = settle_at(&cfg, &mut hub, &public(), DAY + 60.0);
        assert!(due, "{what}");
    }
}

#[test]
fn protection_that_requires_no_checks_has_none_to_change() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on(
        "GET",
        &format!("{BRANCHES}$"),
        answer(200, json!([{"name": "main"}])),
    );
    let off = json!({"protection": {"required_status_checks": {"enforcement_level": "off"}}});
    hub.on("GET", "repos/sam/r/branches/main$", answer(200, off));
    settle_at(&cfg, &mut hub, &public(), 0.0);
    assert_eq!(ruled(&hub), []);
    assert!(!hub.paths().contains(&PROTECTION));
    assert_eq!(kept(&cfg)["app"], 7);
}

#[test]
fn an_orgs_baseline_isnt_the_repos_and_one_with_none_gets_it() {
    // The steward can't pin what the org keeps, so the repo gets its own
    // over it (GitHub enforces both), and the org's is left as it is.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    let org =
        json!([{"id": 9, "name": "baseline", "target": "branch", "source_type": "Organization"}]);
    hub.on("GET", &format!("{LISTING}$"), answer(200, org));
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let made = baseline(&cfg.settle, cfg.settle.defaults);
    assert_eq!(ruled(&hub), [("POST", "repos/sam/r/rulesets", Some(&made))]);
    // With none at all, it's added, its required status from the App only.
    hub.on("GET", &format!("{LISTING}$"), answer(200, json!([])));
    settle_at(&cfg, &mut hub, &public(), DAY);
    assert_eq!(ruled(&hub), [("POST", "repos/sam/r/rulesets", Some(&made))]);
    let required = made["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|y| y["type"] == "required_status_checks");
    assert_eq!(
        required.unwrap()["parameters"]["required_status_checks"],
        checks(&SETTLED)
    );
    assert_eq!(made["bypass_actors"], owner_bypass("pull_request"));
    // A repo whose automation pushes to it gets only what stops deleting
    // and force pushes, which that automation bypasses.
    let pushed = Rules {
        direct_push: true,
        ..cfg.settle.defaults
    };
    let made = baseline(&cfg.settle, pushed);
    assert_eq!(
        made["rules"],
        json!([{"type": "deletion"}, {"type": "non_fast_forward"}])
    );
    assert_eq!(made["bypass_actors"], owner_bypass("always"));
}

#[test]
fn an_empty_repo_gets_its_ruleset_with_its_first_commit() {
    // Not before: it would turn away the first push (the owner's too: they
    // pass it only by PR). Its labels and other settings are set, and its
    // first push's pass adds the ruleset at once, not a day on.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on("GET", &format!("{LISTING}$"), answer(200, json!([])));
    let empty = Repo {
        private: false,
        empty: true,
    };
    settle_at(&cfg, &mut hub, &empty, 0.0);
    assert!(!hub.paths().contains(&"repos/sam/r/rulesets") && made(&hub).len() == 6);
    assert!(writes(&hub).contains(&("PUT", "repos/sam/r/vulnerability-alerts", None)));
    assert_eq!(kept(&cfg)["empty"], true);
    // Still empty, it's settled once a day like any other.
    let (due, _) = settle_at(&cfg, &mut hub, &empty, 30.0);
    assert!(!due);
    let (due, _) = settle_at(&cfg, &mut hub, &public(), 60.0);
    assert!(due);
    assert!(
        writes(&hub)
            .iter()
            .any(|w| (w.0, w.1) == ("POST", "repos/sam/r/rulesets"))
    );
    assert_eq!(kept(&cfg).get("empty"), None);
}

#[test]
fn the_baseline_requires_the_trains_status_exactly_while_its_train_is_on() {
    // Its own required checks the App's alone; one already there is brought
    // in line when the train goes on or off, and another ruleset only has
    // it pinned to the App.
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    let off = cfg.settle.defaults;
    let on = Rules { train: true, ..off };
    let both = checks(&[("review-settled", 7), ("conary/train", 7)]);
    assert_eq!(json!(required(&cfg.settle, false)), checks(&SETTLED));
    assert_eq!(json!(required(&cfg.settle, true)), both);
    let made = baseline(&cfg.settle, on);
    assert_eq!(
        made["rules"][3]["parameters"]["required_status_checks"],
        both
    );
    assert_eq!(made["bypass_actors"], json!([]));
    let mut hub = github(&cfg);
    let rules = |required: Value| {
        json!([required_rule(false, required), {"type": "deletion"}, {"type": "non_fast_forward"},
               {"type": "pull_request", "parameters": {}}])
    };
    for (train, had, want) in [
        (on, checks(&SETTLED), Some(both.clone())),
        (off, both.clone(), Some(checks(&SETTLED))),
        (on, both.clone(), None),
        (off, checks(&SETTLED), None),
    ] {
        let bypass = if train.train {
            json!([])
        } else {
            owner_bypass("pull_request")
        };
        let rs = ruleset("baseline", rules(had), bypass);
        hub.on("GET", "repos/sam/r/rulesets/1$", answer(200, rs.clone()));
        let mut jobs = States::new("running");
        let (_, ok) = look(&cfg, &mut hub, &mut jobs, 0.0, |p| {
            Ok::<_, String>(keep_rules(p, &cfg.settle, train, &[&json!({"id": 1})]))
        });
        assert!(ok.unwrap());
        let put: Vec<_> = writes(&hub)
            .into_iter()
            .map(|w| w.2.unwrap().clone())
            .collect();
        match want {
            None => assert_eq!(put, Vec::<Value>::new(), "{train:?}"),
            Some(want) => {
                let mut sent = back(&rs);
                sent["rules"][0]["parameters"]["required_status_checks"] = want;
                assert_eq!(put, [sent], "{train:?}");
            }
        }
    }
    // Another ruleset than the baseline only has the train's pinned.
    let rs = gates(false, json!([{"context": "conary/train"}]));
    hub.on("GET", "repos/sam/r/rulesets/2$", answer(200, rs));
    let mut jobs = States::new("running");
    let (_, ok) = look(&cfg, &mut hub, &mut jobs, 0.0, |p| {
        Ok::<_, String>(keep_rules(p, &cfg.settle, off, &[&json!({"id": 2})]))
    });
    assert!(ok.unwrap());
    let put = writes(&hub)[0].2.unwrap();
    assert_eq!(
        put["rules"][1]["parameters"]["required_status_checks"],
        checks(&[("conary/train", 7)])
    );
    // And a whole repo's settings are due as soon as its train goes on or
    // off, not a day later.
    cfg.settle.repos.insert(
        "sam/r".into(),
        Overrides {
            train: Some(true),
            ..Overrides::default()
        },
    );
    for (was, due) in [(true, false), (false, true)] {
        keep(
            &cfg,
            json!({"settings": T, "app": 7, "train": was, "labels": LABELS}),
        );
        let (got, _) = settle_at(&cfg, &mut hub, &public(), 0.0);
        assert_eq!(got, due, "{was}");
        assert_eq!(hub.paths().contains(&"repos/sam/r"), due, "{was}");
    }
    assert_eq!(kept(&cfg)["train"], true);
}

#[test]
fn an_old_baseline_gets_the_rules_it_lacks_and_its_bypass_follows_the_train() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let off = cfg.settle.defaults;
    let on = Rules { train: true, ..off };
    let mut hub = github(&cfg);
    let keep_one = |hub: &mut Hub, rules: Rules| {
        let mut jobs = States::new("running");
        let (_, ok) = look(&cfg, hub, &mut jobs, 0.0, |p| {
            Ok::<_, String>(keep_rules(p, &cfg.settle, rules, &[&json!({"id": 1})]))
        });
        assert!(ok.unwrap());
        writes(hub)
            .into_iter()
            .map(|w| w.2.unwrap().clone())
            .collect::<Vec<_>>()
    };
    // One from before the train (made while it pushed directly: only the
    // first two rules, the owner bypassing always) has no required checks
    // to add the train's to: it gets the rules it lacks, and no bypass.
    let old = ruleset(
        "baseline",
        json!([{"type": "deletion"}, {"type": "non_fast_forward"}]),
        owner_bypass("always"),
    );
    hub.on("GET", "repos/sam/r/rulesets/1$", answer(200, old.clone()));
    let sent = keep_one(&mut hub, on);
    let types: Vec<&str> = sent[0]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|y| y["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "deletion",
            "non_fast_forward",
            "pull_request",
            "required_status_checks"
        ]
    );
    assert_eq!(
        sent[0]["rules"][3]["parameters"]["required_status_checks"],
        checks(&[("review-settled", 7), ("conary/train", 7)])
    );
    assert_eq!(sent[0]["bypass_actors"], json!([]));
    // Its bypass comes back with the train off.
    let rs = ruleset(
        "baseline",
        json!([required_rule(false, checks(&SETTLED)), {"type": "deletion"}, {"type": "non_fast_forward"},
               {"type": "pull_request", "parameters": {}}]),
        json!([]),
    );
    hub.on("GET", "repos/sam/r/rulesets/1$", answer(200, rs));
    let sent = keep_one(&mut hub, off);
    assert_eq!(sent[0]["bypass_actors"], owner_bypass("pull_request"));
    // A directPush repo's rules stay as they are, and so does any baseline
    // with rulesets off.
    let mut short = old.clone();
    short["rules"] = json!([{"type": "deletion"}]);
    hub.on("GET", "repos/sam/r/rulesets/1$", answer(200, short));
    for rules in [
        Rules {
            direct_push: true,
            ..off
        },
        Rules {
            ruleset: false,
            ..off
        },
    ] {
        assert_eq!(keep_one(&mut hub, rules), Vec::<Value>::new(), "{rules:?}");
    }
}

#[test]
fn what_the_steward_keeps_in_the_file_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    let own = json!({"databaseId": 2002, "hook": "abc", "hookId": 41, "hookAt": 1800000000.25, "unreached": "gone"});
    let mut st = own.clone();
    st["settings"] = 0.into();
    keep(&cfg, st);
    settle_at(&cfg, &mut hub, &public(), 0.0);
    let st = kept(&cfg);
    for (k, v) in own.as_object().unwrap() {
        assert_eq!(&st[k], v, "{k}");
    }
    assert_eq!(st["settings"], T);
    assert_eq!(st["app"], 7);
}

#[test]
fn github_not_saying_what_the_repo_is_fails_its_settings_and_keeps_its_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on(
        "GET",
        "repos/sam/r$",
        answer(502, json!({"message": "Bad Gateway"})),
    );
    let (due, out) = settle_at(&cfg, &mut hub, &public(), 0.0);
    assert!(due);
    assert_eq!(out.failed.len(), 1, "{out:?}");
    assert!(
        out.failed[0].starts_with("its settings: GET repos/sam/r: HTTP 502"),
        "{out:?}"
    );
    let st = kept(&cfg);
    assert_eq!(
        (st["labels"].clone(), st.get("settings")),
        (json!(LABELS), None)
    );
    // A rate limit is noted, for the pass.
    let limit = json!({"message": "API rate limit exceeded for installation"});
    hub.on("GET", "repos/sam/r$", answer(403, limit));
    hub.on(
        "GET",
        "rate_limit$",
        answer(
            200,
            json!({"resources": {"core": {"remaining": 0, "reset": T + 60.0}}}),
        ),
    );
    let (_, out) = settle_at(&cfg, &mut hub, &public(), 60.0);
    assert_eq!(out.limited, Some(T + 60.0));
}

#[test]
fn a_status_run_changes_nothing_and_keeps_state_of_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut hub = github(&cfg);
    hub.on("GET", &format!("{LISTING}$"), answer(200, json!([])));
    std::fs::create_dir_all(cfg.state_dir.join("dry")).unwrap();
    let mut jobs = States::new("running");
    let mut p = Pass {
        cfg: &cfg,
        repo: "sam/r",
        now: T,
        dry: true,
        searched: 0,
        api: &mut hub,
        jobs: &mut jobs,
        out: Out::default(),
    };
    assert!(settle(&mut p, &cfg.settle, &public()));
    assert!(p.out.failed.is_empty(), "{:?}", p.out);
    assert_eq!(writes(&hub), []);
    assert!(!cfg.state_dir.join("repo-sam_r.json").exists());
    let text = std::fs::read_to_string(cfg.state_dir.join("dry/repo-sam_r.json")).unwrap();
    let st: Value = serde_json::from_str(&text).unwrap();
    // Nothing it would have made is known to be there.
    assert_eq!((st.get("labels"), st.get("app")), (None, None));
    assert_eq!(st["settings"], T);
}
