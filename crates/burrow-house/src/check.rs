//! One run of the check: judge the PR its event names, post each rule's
//! verdict on the event's head, and read the PR again.

use std::io::Write;

use serde_json::Value;

use crate::diff::{self, Diff};
use crate::github::{GitHub, Status};
use crate::rules::{self, Rule, Said};

/// What the run is for, from the event that started it.
#[derive(Clone, Debug)]
pub struct Event {
    /// OWNER/REPO.
    pub repo: String,
    /// The PR's number.
    pub number: u64,
    /// The head the event names, which this run's own check is on and its
    /// statuses go on: a re-run keeps it.
    pub head: String,
    /// How many lines a PR may add to Python and shell, net, before it needs
    /// the line: enough for a fix, not for a feature.
    pub growth: i64,
    /// How long a code file may grow before it's split.
    pub lines: usize,
    /// Whether a PR that names a model needs the line: off only for a repo
    /// whose subject is models.
    pub model_names: bool,
    /// The run's page, which each status links.
    pub run_url: String,
}

/// How many times a run looks at a PR that keeps changing before it fails.
pub const LOOKS: usize = 10;

/// One rule's verdict on the PR as read.
struct Verdict {
    rule: Rule,
    passed: bool,
    /// The status's description: never the author's text.
    why: String,
    /// What the log says when it fails, for GitHub to show on the PR.
    error: Option<String>,
}

fn pass(rule: Rule, why: &str) -> Verdict {
    Verdict {
        rule,
        passed: true,
        why: why.to_owned(),
        error: None,
    }
}

/// A rule's verdict on the PR's title and body, and its diff.
fn judge(rule: Rule, title: &str, body: &str, diff: &Diff, event: &Event) -> Verdict {
    let (broke, mut what, hint) = match rule {
        Rule::Title => {
            return match rules::title_style(title) {
                None => pass(rule, "the title is 'area: plain sentence'"),
                Some(what) => Verdict {
                    rule,
                    passed: false,
                    why: format!("the title isn't 'area: plain sentence': {what}"),
                    error: Some(format!(
                        "::error title=PR title::This PR's title isn't in the house style: {what}. The steward squash-merges, so the title is the subject main gets, and the house rules want it as 'area: plain sentence', the area being the module, directory or tool the change is about (autoland: merge only the head that was checked). Lowercase after the colon, no trailing period, no feat:/fix: types. Editing the title runs this again."
                    )),
                },
            };
        }
        Rule::RustFirst => (
            !diff.new_python.is_empty() || diff.growth > event.growth,
            format!(
                "adds {} Python file(s) and {} net line(s) of Python and shell",
                diff.new_python.len(),
                diff.growth
            ),
            "The house rules want new code in Rust, shell included, unless there's a solid reason: a coin toss goes to Rust. Port it, or add a line to the PR body starting 'Why not Rust:' with the reason (a file a tool generates, glue that only runs another program, a throwaway script, deleted once it's run)".to_owned(),
        ),
        Rule::Timers => (
            !diff.timers.is_empty(),
            format!("adds a timer in {} file(s)", diff.timers.len()),
            "Work starts from an event (a push, a webhook, a unit finishing), not a clock. Start it from one, or add a line to the PR body starting 'Why a timer:' with the reason (a watchdog's heartbeat, a catch-up for a delivery that never came)".to_owned(),
        ),
        Rule::FileSize => (
            !diff.big.is_empty(),
            format!("grows {} code file(s) past {} lines", diff.big.len(), event.lines),
            format!("The house rules split a code file before it passes {} lines. Split it in this PR, or add a line to the PR body starting 'Why not split:' with the reason", event.lines),
        ),
        Rule::ModelNames => (
            !diff.models.is_empty(),
            format!("names a model in {} file(s)", diff.models.len()),
            "Tracked files never name a model: which model does what is the machine's business, and the names go stale in weeks. Take it out, or add a line to the PR body starting 'Why a model name:' with the reason (a recorded screen in a test)".to_owned(),
        ),
    };
    let line = rule
        .line()
        .expect("a rule other than the title takes a line");
    let broke = if diff.listed >= diff::LISTED {
        what = format!(
            "has {} files, the most GitHub lists, so one it leaves out could need it",
            diff.listed
        );
        true
    } else {
        broke
    };
    if !broke {
        return pass(rule, "nothing it needs a reason for");
    }
    let lacks = match rules::says(body, line) {
        Said::Why => return pass(rule, "the body says why"),
        Said::TooLittle => format!("the body's '{line}:' line gives no reason"),
        Said::Nothing => format!("the body has no '{line}:' line"),
    };
    Verdict {
        rule,
        passed: false,
        why: format!("{what}, and {lacks}"),
        error: Some(format!(
            "::error title={line}?::This PR {what}, and {lacks}. {hint}. A reason takes {} words or more. Editing the body runs this again.",
            rules::REASON_WORDS
        )),
    }
}

fn say(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}").map_err(|e| format!("the log: {e}"))
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// What the run found in a diff, for the log. Paths are printed quoted, so
/// none can start a line the runner would read as a workflow command.
fn found(out: &mut dyn Write, diff: &Diff, event: &Event) -> Result<(), String> {
    for file in &diff.new_python {
        say(out, &format!("new Python: {file:?}"))?;
    }
    say(
        out,
        &format!(
            "lines added to Python and shell, net: {} (more than {} needs a reason)",
            diff.growth, event.growth
        ),
    )?;
    for (file, length) in &diff.big {
        say(
            out,
            &format!("grown past {} lines: {file:?}, {length} lines", event.lines),
        )?;
    }
    for file in &diff.timers {
        say(out, &format!("timers: {file:?}"))?;
    }
    for file in &diff.models {
        say(out, &format!("models: {file:?}"))?;
    }
    Ok(())
}

/// Judge the PR as it is now, not as the event saw it, post the verdicts on
/// the event's head, then read the PR again: if its base, title or body has
/// changed since, judge and post again. Every push and every edit starts a
/// run, they overlap, and the newest status posted wins, so a run that read
/// the PR before an edit can post after the edit's own run. Reading again
/// after each post makes the last statuses posted on the head the verdicts
/// for the PR as it was read after that post; a change after that read
/// starts a run for the head it has then, which posts later. Each run posts
/// only on its own head, where its check stays pending until its loop ends,
/// so nobody takes a verdict in between for the last word. Once the PR has
/// another head, this one is no longer the run's to judge: that head's own
/// run judges it. A PR still changing after LOOKS looks fails, until it's
/// edited again.
///
/// A fork's PR runs with a token that can't post a status, so there the
/// verdict is the run's result, and it's the owner's to merge anyway:
/// `Ok(false)` for a failure. Everywhere else a run that judged ends
/// `Ok(true)`, its verdicts in the statuses. An error is no verdict.
pub fn run(event: &Event, gh: &dyn GitHub, out: &mut dyn Write) -> Result<bool, String> {
    let rules: Vec<Rule> = Rule::ALL
        .into_iter()
        .filter(|r| *r != Rule::ModelNames || event.model_names)
        .collect();
    let status = |rule: Rule, passed: bool, description: String| Status {
        context: format!("house/{} (#{})", rule.name(), event.number),
        state: if passed { "success" } else { "failure" },
        description,
        target_url: event.run_url.clone(),
    };
    let mut judged: Option<(String, String, String)> = None;
    let mut diff: Option<(String, Diff)> = None;
    let mut looks = 0;
    loop {
        let pr = gh.pull(event.number)?;
        if text(&pr["head"], "sha") != event.head {
            say(
                out,
                &format!(
                    "the PR's head is no longer {}: the run for its new one judges it",
                    event.head
                ),
            )?;
            return Ok(true);
        }
        let seen = (
            text(&pr["base"], "ref").to_owned(),
            text(&pr, "title").to_owned(),
            text(&pr, "body").replace('\r', ""),
        );
        if judged.as_ref() == Some(&seen) {
            return Ok(true);
        }
        if looks == LOOKS {
            say(
                out,
                "::error title=Kept changing::This PR kept changing while the house check read it. Edit it to check it again.",
            )?;
            for rule in &rules {
                let why = "the PR kept changing while this ran: edit it to check again";
                gh.post(&event.head, &status(*rule, false, why.to_owned()))?;
            }
            return Ok(true);
        }
        looks += 1;
        if judged.is_some() {
            say(out, "the PR changed while this ran: judging it again")?;
        }
        let base = text(&pr["base"], "sha");
        if diff.as_ref().is_none_or(|(at, _)| at != base) {
            let read = diff::read(gh, base, &event.head, event.lines)?;
            found(out, &read, event)?;
            diff = Some((base.to_owned(), read));
        }
        let (_, read) = diff.as_ref().expect("read above");
        let (_, title, body) = &seen;
        let verdicts: Vec<Verdict> = rules
            .iter()
            .map(|rule| judge(*rule, title, body, read, event))
            .collect();
        for v in &verdicts {
            if let Some(error) = &v.error {
                say(out, error)?;
            }
            say(out, &format!("{}: {}", v.rule.name(), v.why))?;
        }
        if text(&pr["head"]["repo"], "full_name") != event.repo {
            return Ok(verdicts.iter().all(|v| v.passed));
        }
        for v in verdicts {
            gh.post(&event.head, &status(v.rule, v.passed, v.why))?;
        }
        judged = Some(seen);
    }
}
