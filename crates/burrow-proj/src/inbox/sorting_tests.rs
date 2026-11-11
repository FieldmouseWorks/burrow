//! Closing a finished job's issue and triage, against stand-ins for gh and
//! systemctl that log what they're asked; the drain's are in
//! [`super::drain_tests`].

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::triage::{Verdict, verdict, why};
use super::{Pass, raw};
use crate::config::tests::example;
use crate::config::{Config, Triage};
use crate::engine::Engine;

/// A machine in a temp dir: gh logs each call's arguments to `log` and what
/// it posts to `posted`, runs `hook` with them first when there is one,
/// fails a call that has a line of `fail` in it, answers a comment list with
/// `comments` (none when it's missing), `repos/O/R/issues/N` with the file
/// named that with `_` for `/` (`{}`), and anything else with `{}`;
/// systemctl says every unit is inactive, but a job's is active, waiting
/// for a slot, while there's an `active` file.
pub(super) struct Box {
    tmp: tempfile::TempDir,
    pub(super) cfg: Config,
}

#[test]
fn review_closing_never_replays_a_stale_decision() {
    for reason in ["completed", "not_planned"] {
        for response in [
            "HTTP/2.0 502 Bad Gateway",
            "HTTP/2.0 429 Too Many Requests",
            "reset",
        ] {
            let b = Box::new();
            let attempts = b.path("attempts");
            burrow_test::executable(
                &b.cfg.tools.gh,
                format!(
                    r#"#!/bin/sh
cat >/dev/null
if [ ! -e '{attempts}' ]; then
  echo asked >'{attempts}'
  if [ '{response}' = reset ]; then echo 'connection reset by peer' >&2; else printf '{response}\n\n{{}}'; fi
  exit 1
fi
echo asked >>'{attempts}'
printf 'HTTP/2.0 200 OK\n\n{{}}'
"#,
                    attempts = attempts.display()
                ),
            );
            assert!(
                !b.pass().close("sam/box", "7", reason),
                "{reason}: {response}"
            );
            assert_eq!(fs::read_to_string(attempts).unwrap(), "asked\n");
        }
    }
}

impl Box {
    pub(super) fn new() -> Box {
        let tmp = tempfile::TempDir::new().unwrap();
        let at = |f: &str| tmp.path().join(f);
        let script = |name: &str, body: String| {
            let p = at(name);
            burrow_test::executable(&p, format!("#!/bin/sh\n{body}"));
            p
        };
        let gh = script(
            "gh",
            format!(
                "export TEST='{}'\n{}",
                tmp.path().display(),
                include_str!("github-test.sh")
            ),
        );
        // GraphQL reads the same fixture text as REST, with known authorship.
        fs::write(at("written.jq"), include_str!("written.jq")).unwrap();
        let _graphql = script(
            "graphql",
            format!(
                r#"
for arg in "$@"; do
  case "$arg" in owner=*) owner=${{arg#owner=}} ;; name=*) name=${{arg#name=}} ;; n=*) n=${{arg#n=}} ;; esac
done
[ "$GH_REPO" = "$owner/$name" ] || exit 1
f='{dir}/repos_'"$owner"'_'"$name"'_issues_'"$n"
[ -e "$f" ] || f='{dir}/default-issue'
jq -n --argjson n "$n" --slurpfile i "$f" --slurpfile c '{dir}/comments' -f '{dir}/written.jq'
"#,
                dir = tmp.path().display()
            ),
        );
        fs::write(at("default-issue"), issue(7, "sam", &[]).to_string()).unwrap();
        fs::write(at("comments"), "[[]]").unwrap();
        let systemctl = script(
            "systemctl",
            format!(
                "case \"$*\" in\n  *is-active*) [ -e '{}' ] && exit 0; exit 3 ;;\n  *show*) echo inactive ;;\nesac\nexit 0\n",
                at("active").display()
            ),
        );
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.tools.gh = gh;
        cfg.tools.systemctl = systemctl;
        cfg.jobs = at("jobs");
        cfg.scratch = at("scratch");
        cfg.waits = None;
        let inbox = cfg.inbox.as_mut().unwrap();
        inbox.state = at("state");
        inbox.wake = at("wake.json");
        inbox.outside_label = "outside text".into();
        inbox.triage = Some(Triage {
            sort_unlabelled: true,
            owner_label: "needs-owner".into(),
            owner: Some("sam".into()),
            skip: vec!["probe".into()],
            at_once: 2,
            drain: Some(Engine::Codex),
            drain_jobs: 1,
        });
        fs::create_dir_all(&inbox.state).unwrap();
        Box { tmp, cfg }
    }

    pub(super) fn path(&self, f: &str) -> PathBuf {
        self.tmp.path().join(f)
    }

    pub(super) fn log(&self) -> String {
        fs::read_to_string(self.path("log")).unwrap_or_default()
    }

    pub(super) fn posted(&self) -> String {
        fs::read_to_string(self.path("posted")).unwrap_or_default()
    }

    pub(super) fn pass(&self) -> Pass<'_> {
        Pass {
            cfg: &self.cfg,
            inbox: self.cfg.inbox.as_ref().unwrap(),
            project: BTreeMap::from([
                ("sam/box".to_owned(), "box".to_owned()),
                ("sam/burrow".to_owned(), "burrow".to_owned()),
                ("sam/game".to_owned(), "game".to_owned()),
            ]),
            wake: BTreeMap::new(),
            unreached: Vec::new(),
            looked: Vec::new(),
        }
    }

    /// A job whose one turn ended as `outcome` ("done" when it ran to its
    /// end, "error" when it didn't), answering `said`, having said it ends
    /// with `proj done`.
    pub(super) fn job(&self, id: &str, outcome: &str, said: &str) {
        self.job_ending(id, outcome, Some("done"), said);
    }

    /// A job whose one turn ended as `outcome`, saying it ends with `proj
    /// END` (None: it said nothing).
    pub(super) fn job_ending(&self, id: &str, outcome: &str, end: Option<&str>, said: &str) {
        let d = self.cfg.jobs.join(id);
        fs::create_dir_all(&d).unwrap();
        crate::policy::bind(
            &crate::job::Job::new(&self.cfg, id),
            crate::policy::Identity::Repository("sam/box".into()),
            crate::policy::Kind::Automated,
        )
        .unwrap();
        fs::write(d.join("usage-kind"), "inbox").unwrap();
        crate::ask::ensure(&crate::job::Job::new(&self.cfg, id)).unwrap();
        fs::write(
            crate::ask::host_path(&crate::job::Job::new(&self.cfg, id), "question-text"),
            said,
        )
        .unwrap();
        fs::write(
            crate::ask::host_path(&crate::job::Job::new(&self.cfg, id), "question-id"),
            "initial",
        )
        .unwrap();
        fs::write(d.join("turns"), "1\n").unwrap();
        fs::write(d.join("1.outcome"), format!("{outcome}\n")).unwrap();
        if let Some(end) = end {
            fs::write(d.join("1.end"), format!("{end}\n")).unwrap();
        }
        fs::write(
            d.join("1.jsonl"),
            format!("{}\n", json!({"type": "result", "result": said})),
        )
        .unwrap();
    }

    /// The inbox's record of issue 7's job, nothing of it answered yet,
    /// handed the issue as [`issue`] has it and no comments.
    pub(super) fn record(&self, id: &str) -> PathBuf {
        let d = self.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
        fs::create_dir_all(&d).unwrap();
        for (f, v) in [
            ("issue", "sam/box#7"),
            ("job", id),
            ("answered", "0"),
            ("seen", "0"),
        ] {
            fs::write(d.join(f), format!("{v}\n")).unwrap();
        }
        let input = self
            .pass()
            .issue_text("sam/box", &issue(7, "sam", &[]), &[]);
        fs::write(d.join("input"), input).unwrap();
        d
    }
}

/// When an issue was last changed, as the fixtures have it.
const UPDATED: &str = "2026-11-01T00:00:00Z";

pub(super) fn issue(n: u64, opener: &str, labels: &[&str]) -> Value {
    json!({
        "number": n,
        "user": burrow_github::fake::actor(opener),
        "title": "Fix it",
        "body": "It's broken.",
        "html_url": format!("https://github.com/sam/box/issues/{n}"),
        "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
        "updated_at": UPDATED,
    })
}

const CLOSE: &str = "-X PATCH repos/sam/box/issues/7 -f state=closed -f state_reason=completed";

// A job that ended `proj done` gets its answer posted, then its issue closed
// and its record dropped: what's open is what's still to do.
#[test]
fn a_finished_job_closes_its_issue() {
    let b = Box::new();
    b.job("j1", "done", "Fixed in #12.");
    fs::write(b.cfg.jobs.join("j1/1.engine"), "codex").unwrap();
    let d = b.record("j1");
    let mut now = issue(7, "sam", &["claude"]);
    now["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
    b.pass()
        .jobs("sam/box", "box", &[issue(7, "sam", &["claude"])])
        .unwrap();
    assert!(b.posted().contains("**Codex job**"), "{}", b.posted());
    assert!(b.posted().contains("Fixed in #12."), "{}", b.posted());
    assert!(b.log().contains(CLOSE), "{}", b.log());
    assert!(!d.exists());
}

// Not when it waits on a person, nor with a reply it hasn't had yet, nor the
// stuck issue, which the steward closes.
#[test]
fn an_unfinished_job_keeps_its_issue_open() {
    // Asked a person, awaits a PR, or said nothing of how it ends: its turn
    // ran to its end all the same, but the job isn't finished.
    for end in [Some("ask"), Some("await"), Some("undeclared"), None] {
        let b = Box::new();
        b.job_ending("j1", "done", end, "Which one?");
        let d = b.record("j1");
        b.pass()
            .jobs("sam/box", "box", &[issue(7, "sam", &["claude"])])
            .unwrap();
        assert!(b.posted().contains("Which one?"), "{end:?}");
        assert!(
            !b.log().contains("PATCH") && d.exists(),
            "{end:?}: {}",
            b.log()
        );
    }

    // Done, but a reply came in that's not a minute old yet: it's handed on
    // next pass, which the pass books.
    let b = Box::new();
    b.job("j1", "done", "Fixed.");
    let d = b.record("j1");
    let now = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    fs::write(
        b.path("comments"),
        json!([[{"id": 9, "user": burrow_github::fake::actor("sam"), "body": "one more thing", "created_at": now}]])
            .to_string(),
    )
    .unwrap();
    let mut p = b.pass();
    p.jobs("sam/box", "box", &[issue(7, "sam", &["claude"])])
        .unwrap();
    assert!(!b.log().contains("PATCH") && d.exists(), "{}", b.log());
    assert!(p.wake.contains_key("sam/box"));

    // The stuck issue.
    let b = Box::new();
    b.job("j1", "done", "Nothing's stuck.");
    let d = b.record("j1");
    b.pass()
        .jobs("sam/box", "box", &[issue(7, "sam", &["claude", "stuck"])])
        .unwrap();
    assert!(!b.log().contains("PATCH") && d.exists(), "{}", b.log());
}

#[test]
fn verdicts_read() {
    for (answer, want) in [
        ("ready\nIt says enough.", Some(Verdict::Ready)),
        ("`later`: waits on #4", Some(Verdict::Later)),
        ("**Verdict: tracker**\n", Some(Verdict::Tracker)),
        ("\n\nOwner.\nIt costs money.", Some(Verdict::Owner)),
        ("duplicate #31\nsame ask", Some(Verdict::Duplicate(31))),
        ("Duplicate 31.", Some(Verdict::Duplicate(31))),
        ("duplicate\n", None),
        ("duplicate #0", None),
        ("done\nmain has it", Some(Verdict::Done)),
        ("I think it's ready", None),
        ("(turn 1 ended without an answer)", None),
        ("", None),
    ] {
        assert_eq!(verdict(answer), want, "{answer:?}");
    }
    assert_eq!(why("ready\n\nIt says enough.\n"), "It says enough.");
    assert_eq!(why("ready"), "(No reason given.)");
    assert_eq!(why(&format!("later\n{}", "x".repeat(5000))).len(), 4000);
}

// Sorted: a sorting label, an engine's, the unstick label or one other
// machinery owns; a stranger's issue and a pull request are never triage's.
#[test]
fn what_counts_as_sorted() {
    let b = Box::new();
    let mut p = b.pass();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    for (i, sorted) in [
        (issue(1, "sam", &[]), false),
        (issue(1, "sam", &["bug", "v1.0-blocker"]), false),
        (issue(1, "sam", &["ready"]), true),
        (issue(1, "sam", &["later"]), true),
        (issue(1, "sam", &["tracker"]), true),
        (issue(1, "sam", &["needs-owner"]), true),
        (issue(1, "sam", &["codex"]), true),
        (issue(1, "sam", &["stuck"]), true),
        (issue(1, "sam", &["probe"]), true),
        (issue(1, "player", &[]), true),
        (issue(1, "sam", &["outside text"]), true),
    ] {
        assert_eq!(
            p.sorted("sam/box", &t, &i, &Default::default()),
            sorted,
            "{i}"
        );
    }
    let mut pr = issue(1, "sam", &[]);
    pr["pull_request"] = json!({});
    assert!(p.sorted("sam/box", &t, &pr, &Default::default()));
}

/// What a sorting job for issue N, as the fixtures have it, read of it.
fn input(b: &Box, n: u64) -> String {
    b.pass().issue_text("sam/box", &issue(n, "sam", &[]), &[])
}

/// A sorting job for issue 7 that answered `said`, then what the pass did.
fn sorted_as(said: &str, still: Value) -> Box {
    let b = Box::new();
    b.job("s1", "done", said);
    fs::write(b.cfg.jobs.join("s1/1.engine"), "codex").unwrap();
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "s1\n").unwrap();
    fs::write(d.join("updated"), format!("{UPDATED}\n")).unwrap();
    fs::write(d.join("input"), input(&b, 7)).unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    b.pass().verdicts("sam/box", &t, &[still]).unwrap();
    assert!(!d.exists(), "the record goes once it's carried out");
    b
}

#[test]
fn verdicts_are_carried_out() {
    let label = |l: &str| format!("-X POST repos/sam/box/issues/7/labels -f labels[]={l}");

    let b = sorted_as("ready\nclear enough", issue(7, "sam", &[]));
    assert!(b.log().contains(&label("ready")), "{}", b.log());
    assert!(b.posted().is_empty(), "a plain label says nothing");

    let b = sorted_as("owner\nIt spends money.", issue(7, "sam", &[]));
    assert!(b.log().contains(&label("needs-owner")), "{}", b.log());
    assert!(
        b.log()
            .contains("-X POST repos/sam/box/issues/7/assignees -f assignees[]=sam")
    );
    assert!(b.posted().contains("It spends money."));

    let b = sorted_as("duplicate #3\nsame ask", issue(7, "sam", &[]));
    assert!(b.posted().contains("a duplicate of #3"), "{}", b.posted());
    assert!(b.log().contains("state_reason=not_planned"));

    let b = sorted_as("done\nmain has it since #9", issue(7, "sam", &[]));
    assert!(b.posted().contains("main has it since #9"));
    assert!(b.posted().contains("**Codex job**"));
    assert!(b.log().contains(CLOSE));

    let b = sorted_as("Hmm, hard to say.", issue(7, "sam", &[]));
    assert!(b.log().contains(&label("later")), "{}", b.log());
    assert!(b.posted().contains("gave no verdict"));

    // No answer at all: nothing said or labelled, and not sorted again until
    // the issue changes, or another's verdict shows sorting works again.
    let b = Box::new();
    b.job("s1", "error", "");
    fs::write(b.cfg.jobs.join("s1/1.jsonl"), "").unwrap();
    let root = b.cfg.inbox.as_ref().unwrap().state.join(".triage");
    fs::create_dir_all(root.join("sam_box-7")).unwrap();
    fs::write(root.join("sam_box-7/job"), "s1\n").unwrap();
    fs::write(root.join("sam_box-7/updated"), "2026-11-01T00:00:00Z\n").unwrap();
    fs::write(root.join("sam_box-7/input"), input(&b, 7)).unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let mut seven = issue(7, "sam", &[]);
    seven["updated_at"] = json!("2026-11-01T00:00:00Z");
    let mut p = b.pass();
    p.verdicts("sam/box", &t, std::slice::from_ref(&seven))
        .unwrap();
    assert!(
        !b.log().contains("-X") && b.posted().is_empty(),
        "{}",
        b.log()
    );
    assert!(root.join("sam_box-7/failed").exists() && !root.join("sam_box-7/job").exists());
    // Unchanged, it's passed over: no job tried (none can start here, which
    // would book a retry).
    p.triage("sam/box", std::slice::from_ref(&seven)).unwrap();
    assert!(p.wake.is_empty(), "{:?}", p.wake);
    // Changed since, it's tried again.
    seven["updated_at"] = json!("2026-11-02T00:00:00Z");
    p.triage("sam/box", std::slice::from_ref(&seven)).unwrap();
    assert!(p.wake.contains_key("sam/box") && !root.join("sam_box-7").exists());
    // Or another issue's verdict clears it.
    fs::create_dir_all(root.join("sam_box-7")).unwrap();
    fs::write(root.join("sam_box-7/failed"), "x\n").unwrap();
    b.job("s2", "done", "later\nbig");
    fs::create_dir_all(root.join("sam_box-8")).unwrap();
    fs::write(root.join("sam_box-8/job"), "s2\n").unwrap();
    fs::write(root.join("sam_box-8/updated"), format!("{UPDATED}\n")).unwrap();
    fs::write(root.join("sam_box-8/input"), input(&b, 8)).unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(8, "sam", &[])])
        .unwrap();
    assert!(!root.join("sam_box-7").exists() && !root.join("sam_box-8").exists());

    // Sorted by someone meanwhile, or closed: the job's only stopped.
    let b = sorted_as("ready", issue(7, "sam", &["later"]));
    assert!(!b.log().contains("labels"), "{}", b.log());
    let b = Box::new();
    b.job("s1", "done", "ready");
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "s1\n").unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    b.pass().verdicts("sam/box", &t, &[]).unwrap();
    assert!(!d.exists() && !b.log().contains("labels"), "{}", b.log());
}

// The module's triage settings, as it writes them, are what proj reads; off
// is null.
#[test]
fn triage_settings_read() {
    let mut cfg = example();
    cfg["inbox"]["triage"] = json!({"ownerLabel": "needs-owner", "owner": null,
        "skip": ["probe", "undo", "flaky"], "atOnce": 2, "drain": "codex", "drainJobs": 2});
    let cfg: Config = serde_json::from_value(cfg).unwrap();
    let t = cfg.inbox.unwrap().triage.unwrap();
    assert_eq!(
        (t.drain, t.at_once, t.owner),
        (Some(Engine::Codex), 2, None)
    );
    let mut off = example();
    off["inbox"]["triage"] = Value::Null;
    let off: Config = serde_json::from_value(off).unwrap();
    assert!(off.inbox.unwrap().triage.is_none());
}

// An engine-labelled issue with the outside label starts no job, whoever
// opened it, and says so once.
#[test]
fn outside_text_starts_no_job() {
    let b = Box::new();
    let mut p = b.pass();
    let report = issue(7, "sam", &["claude", "outside text"]);
    for _ in 0..2 {
        p.new_issues("sam/box", std::slice::from_ref(&report))
            .unwrap();
    }
    let said = b.posted();
    assert_eq!(said.matches("labelled `outside text`").count(), 1, "{said}");
    assert!(!said.contains("It's broken."), "{said}");
    let d = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
    assert!(d.join("refused").exists() && !d.join("job").exists());
}

// A reply that started the job's next turn while the pass was asking GitHub
// about the issue keeps it open, and that turn going: the close is decided
// again under the lock turns start under.
#[test]
fn a_turn_started_meanwhile_keeps_the_issue_open() {
    let b = Box::new();
    b.job("j1", "done", "Fixed in #12.");
    let d = b.record("j1");
    // proj reply, as the pass reads the issue's comments: turn 2 starts.
    burrow_test::executable(
        b.path("hook"),
        format!(
            "#!/bin/sh\ncase \"$1\" in *--paginate*) echo 2 >'{}' ;; esac\n",
            b.cfg.jobs.join("j1/turns").display()
        ),
    );
    b.pass()
        .jobs("sam/box", "box", &[issue(7, "sam", &["claude"])])
        .unwrap();
    assert!(b.posted().contains("Fixed in #12."), "{}", b.posted());
    assert!(!b.log().contains("PATCH"), "{}", b.log());
    assert!(d.exists() && !b.cfg.jobs.join("j1/2.stopped").exists());
}

// A job whose issue has since been labelled outside text, or whose opener is
// no longer an author, is let go like a closed one: no reply of it is handed
// on, and the next look says why.
#[test]
fn a_job_lets_go_of_an_issue_it_may_no_longer_read() {
    for now in [
        issue(7, "sam", &["claude", "outside text"]),
        issue(7, "former", &["claude"]),
    ] {
        let b = Box::new();
        b.job_ending("j1", "done", Some("ask"), "Which one?");
        let d = b.record("j1");
        fs::write(
            b.path("comments"),
            json!([[{"id": 9, "user": burrow_github::fake::actor("sam"), "body": "the red one",
                     "created_at": "2026-11-01T00:00:00Z"}]])
            .to_string(),
        )
        .unwrap();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&now))
            .unwrap();
        assert!(!d.exists(), "{now}");
        assert!(
            !b.log()
                .contains("--paginate --slurp repos/sam/box/issues/7/comments"),
            "no reply looked for: {}",
            b.log()
        );
    }
}

/// A sorting job s1 for issue 7 that answered `said`, its record made as
/// of [`UPDATED`]: the record.
pub(super) fn sorting(b: &Box, said: &str) -> PathBuf {
    b.job("s1", "done", said);
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "s1\n").unwrap();
    fs::write(d.join("updated"), format!("{UPDATED}\n")).unwrap();
    fs::write(d.join("input"), input(b, 7)).unwrap();
    d
}

// A verdict GitHub took only part of is finished on a later pass, from where
// it stopped: nothing said twice, and the label that sorts the issue last, so
// it isn't taken for sorted before the rest is done.
#[test]
fn a_verdict_cut_short_is_finished_later() {
    let t = Box::new().cfg.inbox.unwrap().triage.unwrap();
    for (said, fails, wants) in [
        ("owner\nIt spends money.", "labels", "needs-owner"),
        ("owner\nIt spends money.", "assignees", "needs-owner"),
        ("Hmm, hard to say.", "labels", "later"),
        (
            "done\nmain has it",
            "state=closed",
            "state_reason=completed",
        ),
    ] {
        let b = Box::new();
        let d = sorting(&b, said);
        fs::write(b.path("fail"), format!("{fails}\n")).unwrap();
        b.pass()
            .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
            .unwrap();
        assert!(d.exists(), "{said}: kept to finish");
        if fails != "labels" {
            assert!(
                !b.log().contains(&format!("labels[]={wants}")),
                "{said}: the sorting label goes last"
            );
        }
        // Next pass, GitHub takes the rest.
        fs::remove_file(b.path("fail")).unwrap();
        b.pass()
            .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
            .unwrap();
        assert!(!d.exists(), "{said}");
        assert!(b.log().contains(wants), "{said}: {}", b.log());
        let posts = b.log().matches("body=@-").count();
        assert_eq!(posts, 1, "{said}: said once");
        if said.starts_with("owner") {
            let tries = if fails == "assignees" { 2 } else { 1 };
            assert_eq!(b.log().matches("assignees[]=sam").count(), tries, "{said}");
        }
    }
}

// A verdict on an issue whose text or authors' comments changed while it
// was sorted, or between passes carrying it out, is of something else:
// dropped, and the issue sorted again as it stands. The inbox's own
// comments aren't a change.
#[test]
fn a_verdict_on_a_changed_issue_is_dropped() {
    let mut edited = issue(7, "sam", &[]);
    edited["body"] = json!("It's broken, and don't close it until the docs say so.");

    let b = Box::new();
    let d = sorting(&b, "done\nmain has it");
    fs::write(b.path("repos_sam_box_issues_7"), edited.to_string()).unwrap();
    let mut p = b.pass();
    p.triage("sam/box", std::slice::from_ref(&edited)).unwrap();
    assert!(
        !b.log().contains("PATCH") && b.posted().is_empty(),
        "{}",
        b.log()
    );
    assert!(!d.join("job").exists());
    // Sorted again: a job tried (none can start here, which books a retry).
    assert!(p.wake.contains_key("sam/box"));

    // Partway: its comment posted, the close refused; then the author
    // comments before the next pass.
    let b = Box::new();
    let d = sorting(&b, "done\nmain has it");
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    fs::write(b.path("fail"), "state=closed\n").unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert_eq!(fs::read_to_string(d.join("did")).unwrap(), "2\n");
    fs::remove_file(b.path("fail")).unwrap();
    let said = |who: &str, id: u64, body: &str| json!({"id": id, "user": burrow_github::fake::actor(who), "body": body, "created_at": UPDATED});
    let mark = &b.cfg.inbox_mark;
    // Its own answer alone isn't a change: the close goes ahead.
    fs::write(
        b.path("comments"),
        json!([[said("sam", 1, &format!("Sorted: already done.\n{mark}"))]]).to_string(),
    )
    .unwrap();
    let before = b.log().matches("PATCH").count();
    let d2 = d.clone();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert_eq!(b.log().matches("PATCH").count(), before + 1);
    assert!(!d2.exists());
    // The author's reply is: the close doesn't.
    let b = Box::new();
    let d = sorting(&b, "done\nmain has it");
    fs::write(b.path("fail"), "state=closed\n").unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    fs::remove_file(b.path("fail")).unwrap();
    fs::write(
        b.path("comments"),
        json!([[said("sam", 2, "Not yet: the docs still say otherwise.")]]).to_string(),
    )
    .unwrap();
    let before = b.log().matches("PATCH").count();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert_eq!(b.log().matches("PATCH").count(), before, "{}", b.log());
    assert!(!d.exists());
}

// A verdict partway through finishes the steps it was given, whatever the
// settings say by the time a pass comes back to it.
#[test]
fn a_verdict_keeps_its_plan() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().triage.as_mut().unwrap().owner = None;
    let d = sorting(&b, "owner\nIt spends money.");
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    fs::write(b.path("fail"), "labels[]=needs-owner\n").unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert_eq!(fs::read_to_string(d.join("did")).unwrap(), "2\n");
    fs::remove_file(b.path("fail")).unwrap();
    // An owner set since: not assigned now, and nothing said twice.
    let mut t = t;
    t.owner = Some("sam".into());
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert!(!d.exists());
    assert!(!b.log().contains("assignees"), "{}", b.log());
    assert_eq!(b.log().matches("body=@-").count(), 1);
    assert_eq!(b.log().matches("labels[]=needs-owner").count(), 2);
}

// An issue whose verdict this pass carried out isn't sorted again from the
// pass's list of issues, which is from before it.
#[test]
fn a_carried_out_verdict_is_not_sorted_again() {
    for said in [
        "ready",
        "later",
        "tracker",
        "owner\nmoney",
        "duplicate #3",
        "done",
        "Hmm.",
    ] {
        let b = Box::new();
        sorting(&b, said);
        let mut p = b.pass();
        p.triage("sam/box", &[issue(7, "sam", &[])]).unwrap();
        // No job tried for it (one would fail here, booking a retry).
        assert!(p.wake.is_empty(), "{said}: {:?}", p.wake);
    }
}

/// An issue as GitHub gives it when asked for it alone, saved for the
/// stand-in gh.
pub(super) fn on_github(b: &Box, i: &Value) {
    let repo = raw(&i["repository_url"]);
    let repo = repo.trim_start_matches("https://api.github.com/repos/");
    let mut i = i.clone();
    i["state"] = json!("open");
    fs::write(
        b.path(&format!(
            "repos_{}_issues_{}",
            repo.replace('/', "_"),
            i["number"]
        )),
        i.to_string(),
    )
    .unwrap();
}

// A job, a sorting job's included, whose issue an agent may no longer read
// is stopped even while its turn waits for a slot, before it reads any.
#[test]
fn a_queued_job_is_stopped_once_its_issue_is_barred() {
    for barred in [
        issue(7, "sam", &["claude", "outside text"]),
        issue(7, "former", &["claude"]),
    ] {
        let b = Box::new();
        b.job("j1", "done", "");
        fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        let d = b.record("j1");
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&barred))
            .unwrap();
        assert!(!d.exists(), "{barred}");
        assert!(b.cfg.jobs.join("j1/1.stopped").exists(), "{barred}");

        let b = Box::new();
        let d = sorting(&b, "");
        fs::remove_file(b.cfg.jobs.join("s1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        let mut unlabelled = barred.clone();
        unlabelled["labels"] = json!(
            barred["labels"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| l["name"] != "claude")
                .collect::<Vec<_>>()
        );
        b.pass()
            .verdicts("sam/box", &t, std::slice::from_ref(&unlabelled))
            .unwrap();
        assert!(!d.exists(), "{unlabelled}");
        assert!(b.cfg.jobs.join("s1/1.stopped").exists(), "{unlabelled}");
    }
}

// Barred while its label comes off or it closes, so the pass's list no
// longer has it: GitHub is asked, and a job still to finish is stopped. One
// closed that may still be read finishes, as before.
#[test]
fn a_barred_issue_off_the_list_stops_its_job() {
    let seven = |labels: &[&str]| issue(7, "sam", labels);
    for (now, stopped) in [
        (seven(&["outside text"]), true),
        (issue(7, "former", &[]), true),
        (seven(&[]), false),
    ] {
        let mut now = now;
        now["repository_url"] = json!("https://api.github.com/repos/sam/box");
        let b = Box::new();
        b.job("j1", "done", "");
        fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        on_github(&b, &now);
        let d = b.record("j1");
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert_eq!(!d.exists(), stopped, "{now}");
        assert_eq!(b.cfg.jobs.join("j1/1.stopped").exists(), stopped, "{now}");

        let b = Box::new();
        let d = sorting(&b, "");
        fs::remove_file(b.cfg.jobs.join("s1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        on_github(&b, &now);
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        b.pass().verdicts("sam/box", &t, &[]).unwrap();
        assert_eq!(!d.exists(), stopped, "{now}");
    }
    // GitHub doesn't say: kept, and asked again.
    let b = Box::new();
    b.job("j1", "done", "");
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    let d = b.record("j1");
    fs::write(b.path("fail"), "repos/sam/box/issues/7\n").unwrap();
    let mut p = b.pass();
    p.jobs("sam/box", "box", &[]).unwrap();
    assert!(d.exists() && p.wake.contains_key("sam/box"));
}

// An already finished sorting verdict must not change a running lane's work.
#[test]
fn a_handed_issue_url_blocks_triage_and_its_completed_verdict() {
    let b = Box::new();
    b.job("lane", "done", "finished");
    let old = b.record("lane");
    let lane = old.with_file_name("sam_box-100");
    fs::rename(old, &lane).unwrap();
    fs::write(lane.join("issue"), "sam/box#100\n").unwrap();
    Pass::hand(
        &lane,
        "[deliver](HTTPS://GITHUB.COM/SAM/BOX/ISSUES/7#issue-123)",
    )
    .unwrap();
    // Only the delivery job is active; the sorter has finished.
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        format!(
            "#!/bin/sh\ncase \"$*\" in *is-active*proj-lane.service*) [ -e '{}' ] && exit 0; exit 3 ;; *is-active*) exit 3 ;; *show*) echo inactive ;; esac\nexit 0\n",
            b.path("active").display()
        ),
    );
    fs::write(b.path("active"), "").unwrap();
    let seven = issue(7, "sam", &[]);
    let mut p = b.pass();
    p.triage("sam/box", std::slice::from_ref(&seven)).unwrap();
    assert!(p.wake.is_empty(), "no sorting job started");
    let d = sorting(&b, "done\nalready finished");
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    p.verdicts("sam/box", t, std::slice::from_ref(&seven))
        .unwrap();
    assert!(!d.exists(), "the obsolete verdict is discarded");
    assert!(!b.log().contains("-X"), "{}", b.log());
    assert!(b.posted().is_empty());
    fs::remove_file(b.path("active")).unwrap();
    sorting(&b, "done\nalready finished");
    p.verdicts("sam/box", t, &[seven]).unwrap();
    assert!(
        b.log().contains(CLOSE),
        "eligible after the lane ends: {}",
        b.log()
    );
}

#[test]
fn existing_machine_issue_jobs_adopt_the_role_and_drop_the_label_pin() {
    let mut b = Box::new();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .authors
        .push(burrow_github::fake::author("steward[bot]"));
    b.job("j1", "done", "Nothing's stuck.");
    b.record("j1");
    let job = crate::job::Job::new(&b.cfg, "j1");
    fs::write(job.path("engine"), "claude").unwrap();
    let i = issue(7, "steward[bot]", &["claude", "stuck"]);
    fs::write(b.path("repos_sam_box_issues_7"), i.to_string()).unwrap();
    b.pass().jobs("sam/box", "box", &[i]).unwrap();
    assert_eq!(fs::read_to_string(job.path("role")).unwrap(), "stuck\n");
    assert!(!job.path("engine").exists());
}

#[test]
fn triage_assigns_priority_but_preserves_existing_labels() {
    for (labels, want) in [
        (vec![], Some("priority: high")),
        (vec!["priority: low"], None),
    ] {
        let b = Box::new();
        sorting(&b, "ready high\nA pipeline blocker.");
        let mut i = issue(7, "sam", &labels);
        i["repository_url"] = json!("https://api.github.com/repos/sam/box");
        on_github(&b, &i);
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        b.pass().verdicts("sam/box", &t, &[i]).unwrap();
        assert!(b.log().contains("labels[]=ready"), "{}", b.log());
        match want {
            Some(label) => assert!(b.log().contains(&format!("labels[]={label}"))),
            None => assert!(!b.log().contains("labels[]=priority:")),
        }
    }
}
