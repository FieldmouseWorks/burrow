//! A repo's settings, kept at the baseline (docs/repo-baseline.md): the
//! labels the steward and its jobs put on PRs, the repo's own settings, its
//! `baseline` ruleset, and every rule of its that requires the steward's
//! status, pinned to the steward's App. At most once a day, and at once
//! when that can't wait: another App, a train turned on or off, a repo's
//! first commit.
//!
//! What's kept is in repo-REPO.json, beside what the branch watch keeps
//! there (branches.rs) and what the steward keeps itself (the repo's
//! databaseId and its hook's), which stay as they were.

use std::collections::HashSet;

use burrow_github::{call, call_allowing, encode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::{Rules, Settle};
use crate::pass::{DAY, Pass, repo_file};

/// What GitHub says when a repo's plan has no rules (a private repo on a
/// free account): there are none to pin.
pub const NO_RULES: &str = "Upgrade to GitHub Pro";

/// What a repo's settings keep, in repo-REPO.json.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    /// When they were last kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<f64>,
    /// The App every rule was last known to take the steward's status from,
    /// and whether its train was on then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub train: Option<bool>,
    /// Kept while it had no commit: it gets its ruleset once it has one.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub empty: bool,
    /// The labels last made, by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// GitHub took auto-merge being on and left it off: a private repo on
    /// a free account, whose PRs the steward merges itself.
    #[serde(
        rename = "autoMergeRefused",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub auto_merge_refused: bool,
    /// What the branch watch and the steward keep here.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// What GitHub's discovery says of the repo: whether it's private, and
/// whether it has no commit yet (no default branch).
pub struct Repo {
    pub private: bool,
    pub empty: bool,
}

/// The repo's settings, if they're due. Whether they were is what it gives,
/// whatever became of them: the steward's hook goes by it too. What failed
/// is in `failed`.
pub fn settle(p: &mut Pass, cfg: &Settle, repo: &Repo) -> bool {
    let name = repo_file(p.repo);
    let mut st: State = p.load(&name);
    let rules = cfg.rules(p.repo);
    // Due daily, and straight away for another App (its required checks are
    // pinned to the one it was), a train turned on or off (the baseline
    // requires the train's status only while it's on), or a repo settled
    // empty that has its first commit now (it's due its ruleset).
    let due = p.now - st.settings.unwrap_or(0.0) >= DAY
        || st.app != Some(cfg.app_id)
        || st.train.unwrap_or(false) != rules.train
        || (st.empty && !repo.empty);
    if let Err(e) = keep(p, cfg, rules, repo, &mut st, &name, due) {
        p.out.failed.push(format!("its settings: {e}"));
    }
    due
}

fn keep(
    p: &mut Pass,
    cfg: &Settle,
    rules: Rules,
    repo: &Repo,
    st: &mut State,
    name: &str,
    due: bool,
) -> Result<(), String> {
    // The labels: without them a hold can't be set, a judge can't hand a PR
    // to the owner, no PR can go first in its proof line and none can say
    // what it waits on. Made again with the daily settings too, in case one
    // was deleted.
    let about = labels(cfg);
    let want: Vec<String> = about.iter().map(|(n, _, _)| n.clone()).collect();
    if st.labels.as_ref() != Some(&want) || due {
        let mut made = true;
        for (label_name, color, text) in &about {
            let one = label(p, label_name, color, text);
            made = made && one;
        }
        if made {
            eprintln!("{}: labels {}", p.repo, want.join(", "));
            st.labels = Some(want);
            p.save(name, st)?;
        }
    }
    if !due {
        return Ok(());
    }
    let info = call(p.api, "GET", &format!("repos/{}", p.repo), None, None)
        .and_then(|r| r.json())
        .map_err(|e| p.github(e))?;
    let mut patch = Map::new();
    for key in [
        "allow_auto_merge",
        "delete_branch_on_merge",
        "allow_squash_merge",
    ] {
        if info[key] != true && !(key == "allow_auto_merge" && st.auto_merge_refused) {
            patch.insert(key.into(), true.into());
        }
    }
    if !repo.private {
        for key in ["secret_scanning", "secret_scanning_push_protection"] {
            if info["security_and_analysis"][key]["status"] != "enabled" {
                patch
                    .entry("security_and_analysis")
                    .or_insert_with(|| json!({}))[key] = json!({"status": "enabled"});
            }
        }
    }
    if !patch.is_empty() {
        let keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        eprintln!("{}: settings {}", p.repo, keys.join(", "));
        let asked = patch.contains_key("allow_auto_merge");
        let after = write(
            p,
            "PATCH",
            &format!("repos/{}", p.repo),
            Some(&patch.into()),
        );
        // A private repo on a free account takes allow_auto_merge without
        // an error and leaves it off (the steward merges its PRs itself):
        // once the PATCH went through and it's still off, stop asking
        // every day.
        if asked
            && after
                .as_ref()
                .is_some_and(|a| a.is_object() && a["allow_auto_merge"] != true)
        {
            st.auto_merge_refused = true;
        }
    }
    write(
        p,
        "PUT",
        &format!("repos/{}/vulnerability-alerts", p.repo),
        None,
    );

    // Every page, an org's rulesets included. Only the repo's own baseline
    // counts as its baseline: the steward can't pin what the org keeps, so
    // a repo with the org's gets its own over it, and GitHub enforces both.
    let listed = p.pages(&format!("repos/{}/rulesets?per_page=100", p.repo));
    let ruled = match listed {
        // The plan offers no rules on this repo: there are none to pin.
        // Anything else is GitHub not saying, and nothing's known to be
        // pinned.
        Err(why) => {
            if !why.contains(NO_RULES) {
                eprintln!("{}: its rulesets: {why}", p.repo);
            }
            why.contains(NO_RULES)
        }
        Ok(listed) => {
            let own: Vec<&Value> = listed
                .iter()
                .filter(|x| x["source_type"] == "Repository")
                .collect();
            let mut made = true;
            // Not on a repo with no commit yet: the ruleset's checks and PR
            // rule would turn away the push that makes its default branch,
            // the owner's too (they bypass it only by PR). That push's pass
            // adds it.
            if cfg.rulesets
                && rules.ruleset
                && !repo.empty
                && !own.iter().any(|x| x["name"] == "baseline")
            {
                eprintln!("{}: adding the baseline ruleset", p.repo);
                let path = format!("repos/{}/rulesets", p.repo);
                made = write(p, "POST", &path, Some(&baseline(cfg, rules))).is_some();
            }
            // Whether or not the repo gets the baseline ruleset, its own
            // rules are kept in line with it.
            let branch: Vec<&Value> = own
                .into_iter()
                .filter(|x| x["target"] == "branch")
                .collect();
            keep_rules(p, cfg, rules, &branch) && made
        }
    };
    // Settled for this App only once every rule is known to take its status
    // from it alone; once that's not known, the settings are due again.
    let pinned = keep_protection(p, cfg) && ruled;
    st.settings = Some(p.now);
    st.empty = repo.empty;
    if pinned {
        st.app = Some(cfg.app_id);
        st.train = Some(rules.train);
    } else {
        st.app = None;
    }
    p.save(name, st)
}

/// The labels, each with its colour and what it says: the waiting ones say
/// what a PR waits on, and that its status says more.
fn labels(cfg: &Settle) -> Vec<(String, &'static str, String)> {
    let (l, w, says) = (&cfg.labels, &cfg.labels.waiting, &cfg.context);
    vec![
        (
            l.hold.clone(),
            "fbca04",
            "The steward leaves this PR alone".into(),
        ),
        (
            l.owner.clone(),
            "b60205",
            format!(
                "Waits for {}; take the label off to let the steward carry on",
                cfg.owner
            ),
        ),
        (
            l.first.clone(),
            "0e8a16",
            "Goes ahead of the PRs without it in the line for proofs that take the machine".into(),
        ),
        (
            w.release_age.clone(),
            "c5def5",
            format!(
                "Its updates are too new to take yet (the minimum release age); {says} says until when"
            ),
        ),
        (
            w.proof_slot.clone(),
            "c5def5",
            format!(
                "Waits its turn for a proof slot on the machine; {says} says how many are ahead"
            ),
        ),
        (
            w.reviewer.clone(),
            "c5def5",
            format!("Waits for a reviewer to be back in service; {says} says which and why"),
        ),
    ]
}

/// One label, made or brought in line: whether that went through.
fn label(p: &mut Pass, name: &str, color: &str, text: &str) -> bool {
    // GitHub takes a description of up to 100 characters.
    let text: String = text.chars().take(100).collect();
    let body = json!({"color": color, "description": text});
    if p.dry {
        eprintln!("  would: label {name:?} on {}", p.repo);
        return false;
    }
    let path = format!("repos/{}/labels/{}", p.repo, encode(name));
    match call_allowing(p.api, "PATCH", &path, Some(&body), None, &[404]) {
        Ok(r) if r.status != 404 => return true,
        Ok(_) => {}
        Err(e) => {
            let why = p.github(e);
            eprintln!("  failed: {why}");
            return false;
        }
    }
    let path = format!("repos/{}/labels", p.repo);
    write(
        p,
        "POST",
        &path,
        Some(&json!({"name": name, "color": color, "description": text})),
    )
    .is_some()
}

/// The checks the baseline requires: the App's own status, and in a repo
/// with its train on the train's, which it posts just before it lands a
/// PR. Only the App's: one with the same name from anyone else (the
/// owner's login included) satisfies nothing.
fn required(cfg: &Settle, train: bool) -> Vec<Value> {
    let mut checks = vec![&cfg.context];
    if train {
        checks.push(&cfg.train_context);
    }
    checks
        .into_iter()
        .map(|c| json!({"context": c, "integration_id": cfg.app_id}))
        .collect()
}

/// Who gets past the baseline. The owner (an admin) can still merge past
/// it from the PR page, and automation that pushes (directPush) past it
/// altogether; in a repo with its train on, no one: only the train lands
/// there.
fn bypass(rules: Rules) -> Value {
    if rules.train {
        return json!([]);
    }
    let mode = if rules.direct_push {
        "always"
    } else {
        "pull_request"
    };
    json!([{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": mode}])
}

fn baseline(cfg: &Settle, rules: Rules) -> Value {
    let mut made = vec![
        json!({"type": "deletion"}),
        json!({"type": "non_fast_forward"}),
    ];
    if !rules.direct_push {
        made.push(json!({"type": "pull_request", "parameters": {
            "required_approving_review_count": 0,
            "dismiss_stale_reviews_on_push": false,
            // CODEOWNERS (if any) names the paths the owner signs off on.
            "require_code_owner_review": true,
            "require_last_push_approval": false,
            "required_review_thread_resolution": true,
        }}));
        made.push(json!({"type": "required_status_checks", "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": required(cfg, rules.train),
        }}));
    }
    json!({
        "name": "baseline",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "bypass_actors": bypass(rules),
        "rules": made,
    })
}

/// The steward's own checks among a rule's required ones (its status, its
/// train's), pinned to its App (under key: a ruleset's integration_id,
/// classic protection's app_id): the same name from anyone else satisfies
/// nothing. Whether any changed.
fn keep_checks(cfg: &Settle, checks: &mut Value, key: &str) -> bool {
    let mut moved = false;
    for c in checks.as_array_mut().into_iter().flatten() {
        let ours = c["context"] == *cfg.context || c["context"] == *cfg.train_context;
        if ours && c[key] != cfg.app_id {
            c[key] = cfg.app_id.into();
            moved = true;
        }
    }
    moved
}

/// A baseline made before this repo's settings were what they are (say
/// while it pushed directly, with only the first two rules) gets the rules
/// it lacks: with no required-status rule in it there's nothing for
/// keep_rules to require the train's status in. Who may bypass it follows
/// the train going on or off. What changed, in words.
fn complete_baseline(cfg: &Settle, rs: &mut Value, rules: Rules) -> Vec<String> {
    if rs["name"] != "baseline" || !(cfg.rulesets && rules.ruleset) || rules.direct_push {
        return vec![];
    }
    let mut changed = vec![];
    let mut had: Vec<Value> = rs["rules"].as_array().cloned().unwrap_or_default();
    let have: HashSet<String> = had
        .iter()
        .filter_map(|y| y["type"].as_str().map(String::from))
        .collect();
    let missing: Vec<Value> = baseline(cfg, rules)["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|y| !have.contains(y["type"].as_str().unwrap_or_default()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().filter_map(|y| y["type"].as_str()).collect();
        changed.push(format!("{} added", names.join(", ")));
        had.extend(missing);
        rs["rules"] = had.into();
    }
    // Only the train's own turn: a repo's other bypass is its own to keep.
    let bypassed = rs["bypass_actors"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    if bypassed == rules.train {
        rs["bypass_actors"] = bypass(rules);
        changed.push(if rules.train {
            "no bypass, only the train lands".into()
        } else {
            "the owner's bypass back".into()
        });
    }
    changed
}

/// Each of the repo's own branch rulesets, in line with the baseline: no
/// up-to-date branches (docs/repo-baseline.md says why), and the steward's
/// status required from its App only; the baseline's own has all its rules
/// (complete_baseline) and requires the train's too exactly while the
/// repo's train is on. The listing names a ruleset without its rules, so
/// each one is read. False when one couldn't be read or wouldn't take the
/// change.
fn keep_rules(p: &mut Pass, cfg: &Settle, rules: Rules, rulesets: &[&Value]) -> bool {
    let mut ok = true;
    for x in rulesets {
        let path = format!("repos/{}/rulesets/{}", p.repo, x["id"]);
        let Some(mut rs) = read(p, &path).filter(Value::is_object) else {
            ok = false;
            continue;
        };
        let mut changed = complete_baseline(cfg, &mut rs, rules);
        let is_baseline = rs["name"] == "baseline";
        for y in rs["rules"].as_array_mut().into_iter().flatten() {
            if y["type"] != "required_status_checks" || !y["parameters"].is_object() {
                continue;
            }
            let params = &mut y["parameters"];
            if params["strict_required_status_checks_policy"] == true {
                params["strict_required_status_checks_policy"] = false.into();
                changed.push("up-to-date branches off".into());
            }
            if keep_checks(cfg, &mut params["required_status_checks"], "integration_id") {
                changed.push("the steward's checks from the App only".into());
            }
            let listed = params["required_status_checks"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let has = listed.iter().any(|c| c["context"] == *cfg.train_context);
            if is_baseline && has != rules.train {
                let checks: Vec<Value> = if rules.train {
                    let train = json!({"context": cfg.train_context, "integration_id": cfg.app_id});
                    listed.into_iter().chain([train]).collect()
                } else {
                    listed
                        .into_iter()
                        .filter(|c| c["context"] != *cfg.train_context)
                        .collect()
                };
                params["required_status_checks"] = checks.into();
                let now = if rules.train {
                    "required"
                } else {
                    "no longer required"
                };
                changed.push(format!("{} {now}", cfg.train_context));
            }
        }
        if changed.is_empty() {
            continue;
        }
        let mut said: Vec<String> = vec![];
        for c in changed {
            if !said.contains(&c) {
                said.push(c);
            }
        }
        eprintln!(
            "{}: {} in the {} ruleset",
            p.repo,
            said.join(", "),
            rs["name"]
        );
        // The whole ruleset goes back, so only those settings change.
        let mut back: Map<String, Value> = [
            "name",
            "target",
            "enforcement",
            "bypass_actors",
            "conditions",
        ]
        .into_iter()
        .filter_map(|k| rs.get(k).map(|v| (k.to_string(), v.clone())))
        .collect();
        back.insert("rules".into(), rs["rules"].clone());
        let put = write(p, "PUT", &path, Some(&back.into()));
        ok = ok && put.is_some();
    }
    ok
}

/// The same in the classic protection of every protected branch, since a
/// PR can be merged into any of them. False when GitHub didn't say what a
/// branch's is, or didn't take the change.
fn keep_protection(p: &mut Pass, cfg: &Settle) -> bool {
    let path = format!("repos/{}/branches?protected=true&per_page=100", p.repo);
    let branches = match p.pages(&path) {
        Ok(b) => b,
        Err(why) => {
            eprintln!("  failed: {why}");
            return false;
        }
    };
    let mut ok = true;
    for b in &branches {
        let kept = b["name"]
            .as_str()
            .is_some_and(|name| keep_branch(p, cfg, name));
        ok = ok && kept;
    }
    ok
}

/// One branch's classic protection, in line. Its checks are read only when
/// the branch says it requires some: the endpoint is a 404 otherwise.
/// False when GitHub didn't say what they are, or didn't take the change.
fn keep_branch(p: &mut Pass, cfg: &Settle, name: &str) -> bool {
    // A slash in the name (release/1.0) is part of it, not of the path.
    let branch = format!("repos/{}/branches/{}", p.repo, encode(name));
    let Some(b) = read(p, &branch).filter(Value::is_object) else {
        return false;
    };
    let level = &b["protection"]["required_status_checks"]["enforcement_level"];
    if level.as_str().unwrap_or("off") == "off" {
        return true;
    }
    let path = format!("{branch}/protection/required_status_checks");
    let Some(mut got) = read(p, &path).filter(Value::is_object) else {
        return false;
    };
    let (mut patch, mut changed) = (Map::new(), vec![]);
    if got["strict"] == true {
        patch.insert("strict".into(), false.into());
        changed.push("up-to-date branches off");
    }
    if keep_checks(cfg, &mut got["checks"], "app_id") {
        // The others as they were: GitHub lists one any App may set with no
        // app_id, and reads one sent without it as "whichever set it last".
        let checks: Vec<Value> = got["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let mut c = c.clone();
                if c["app_id"].is_null() {
                    c["app_id"] = (-1).into();
                }
                c
            })
            .collect();
        patch.insert("checks".into(), checks.into());
        changed.push("the steward's checks from the App only");
    }
    if patch.is_empty() {
        return true;
    }
    eprintln!("{}: {} in {name}'s protection", p.repo, changed.join(", "));
    write(p, "PATCH", &path, Some(&patch.into())).is_some()
}

/// What GitHub has at `path`, or None, said, when it won't say.
fn read(p: &mut Pass, path: &str) -> Option<Value> {
    p.get(path).map_err(|why| eprintln!("  failed: {why}")).ok()
}

/// A change: what GitHub answered (null for no body), or None, said, when
/// it didn't take it. A status run only says what it would change.
fn write(p: &mut Pass, method: &str, path: &str, body: Option<&Value>) -> Option<Value> {
    if p.dry {
        eprintln!("  would: {method} {path}");
        return None;
    }
    let answer = call(p.api, method, path, body, None).and_then(|r| {
        if r.body.trim().is_empty() {
            Ok(Value::Null)
        } else {
            r.json()
        }
    });
    match answer {
        Ok(v) => Some(v),
        Err(e) => {
            let why = p.github(e);
            eprintln!("  failed: {why}");
            None
        }
    }
}

#[cfg(test)]
#[path = "settle_tests.rs"]
mod tests;
