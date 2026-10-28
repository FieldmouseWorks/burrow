//! A gate's report on its tests' issues: a test with an open issue gets a
//! comment, one without gets a new issue, which quarantines it.

use serde_json::json;

use crate::github::{Api, Error, call};
use crate::quarantine::Quarantined;
use crate::report::{Entry, Outcome};
use crate::test::{LABEL, title};

/// Where the rules are, for whoever reads an issue.
const DOCS: &str = "https://github.com/FieldmouseWorks/burrow/blob/main/docs/flaky-tests.md";
/// How much of a failed try's output an issue shows: its end.
const DETAIL_LINES: usize = 60;
const DETAIL_BYTES: usize = 4000;

#[derive(Debug, PartialEq, Eq)]
pub struct Filed {
    pub test: String,
    pub outcome: Outcome,
    pub issue: u64,
    /// A new issue: the test wasn't quarantined until now.
    pub new: bool,
}

/// File each test's entries (a test reported twice gets one comment) on
/// `open` (quarantine::list), in the order the report has them. `place` says
/// where it happened, in Markdown: a run's link, a PR, a commit.
pub fn file(
    api: &mut dyn Api,
    repo: &str,
    entries: &[Entry],
    open: &[Quarantined],
    place: &str,
) -> Result<Vec<Filed>, Error> {
    let mut groups: Vec<(&str, Outcome, Vec<&str>)> = Vec::new();
    for e in entries {
        let detail = e.detail.as_deref().filter(|d| !d.trim().is_empty());
        match groups
            .iter_mut()
            .find(|(t, o, _)| *t == e.test && *o == e.outcome)
        {
            Some(g) => g.2.extend(detail),
            None => groups.push((&e.test, e.outcome, detail.into_iter().collect())),
        }
    }
    let mut labelled = false;
    let mut out = Vec::new();
    for (test, outcome, details) in groups {
        let shown = show(outcome, &details);
        if let Some(q) = open.iter().find(|q| q.test == test) {
            let said = match outcome {
                Outcome::Flaky => {
                    format!("Flaky again, in {place}: it failed, then passed when it ran again.")
                }
                Outcome::Quarantined => {
                    format!("Failed again, in {place}, where it's quarantined.")
                }
            };
            call(
                api,
                "POST",
                &format!("repos/{repo}/issues/{}/comments", q.issue),
                Some(&json!({"body": said + &shown})),
                None,
            )?;
            out.push(Filed {
                test: test.into(),
                outcome,
                issue: q.issue,
                new: false,
            });
            continue;
        }
        if !labelled {
            label(api, repo)?;
            labelled = true;
        }
        let what = match outcome {
            Outcome::Flaky => format!(
                "`{test}` failed, then passed when it ran again on the same tree, in {place}."
            ),
            Outcome::Quarantined => format!(
                "`{test}` failed in {place}, where it was quarantined, but no issue quarantines it now: \
                 this one does, until it's seen to."
            ),
        };
        let body = format!(
            "{what}\n\nWhile this issue is open the test is quarantined: every gate still runs it, but its failures fail \
             nothing, and each one is noted here. A Claude job looks for the cause, and the pull request with its fix \
             closes this issue, which lifts the quarantine. How it works: [burrow's docs/flaky-tests.md]({DOCS}).{shown}"
        );
        let r = call(
            api,
            "POST",
            &format!("repos/{repo}/issues"),
            Some(&json!({"title": title(test), "body": body, "labels": [LABEL]})),
            None,
        )?;
        let issue = r.json()?["number"]
            .as_u64()
            .ok_or_else(|| Error("GitHub's new issue has no number".into()))?;
        out.push(Filed {
            test: test.into(),
            outcome,
            issue,
            new: true,
        });
    }
    Ok(out)
}

/// The label, made if the repo hasn't got it, so a new issue doesn't come
/// out unlabelled (GitHub drops a label it doesn't know rather than fail).
fn label(api: &mut dyn Api, repo: &str) -> Result<(), Error> {
    let want = json!({"name": LABEL, "color": "d93f0b",
                      "description": "A flaky test, quarantined while this is open (burrow's docs/flaky-tests.md)"});
    match call(
        api,
        "POST",
        &format!("repos/{repo}/labels"),
        Some(&want),
        None,
    ) {
        Ok(_) => Ok(()),
        Err(e) if e.0.contains("HTTP 422") => Ok(()), // it has it
        Err(e) => Err(e),
    }
}

/// The failed tries' output, folded, each as its last lines.
fn show(outcome: Outcome, details: &[&str]) -> String {
    let summary = match (outcome, details.len()) {
        (Outcome::Flaky, 1) => "The failed try".to_string(),
        (Outcome::Flaky, n) => format!("The {n} failed tries"),
        (Outcome::Quarantined, 1) => "The failure".to_string(),
        (Outcome::Quarantined, n) => format!("The {n} failures"),
    };
    if details.is_empty() {
        return String::new();
    }
    let blocks: Vec<String> = details.iter().map(|d| fence(&tail(d))).collect();
    format!(
        "\n\n<details><summary>{summary}</summary>\n\n{}\n\n</details>",
        blocks.join("\n\n")
    )
}

/// The end of a long output: its last DETAIL_LINES lines, at most
/// DETAIL_BYTES of them, saying how much was left out.
fn tail(d: &str) -> String {
    let lines: Vec<&str> = d.trim_end().lines().collect();
    let mut kept = lines[lines.len().saturating_sub(DETAIL_LINES)..].join("\n");
    if kept.len() > DETAIL_BYTES {
        let mut at = kept.len() - DETAIL_BYTES;
        while !kept.is_char_boundary(at) {
            at += 1;
        }
        kept = kept[at..].to_string();
    }
    let cut = d.trim_end().len() - kept.len();
    if cut > 0 {
        format!("[{cut} bytes before this left out]\n{kept}")
    } else {
        kept
    }
}

/// A code block no text inside it can close.
fn fence(text: &str) -> String {
    let longest = text.split(|c| c != '~').map(str::len).max().unwrap_or(0);
    let f = "~".repeat(longest.max(2) + 1);
    format!("{f}\n{text}\n{f}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fake::{Fake, ok};

    fn entry(test: &str, outcome: Outcome, detail: Option<&str>) -> Entry {
        Entry {
            test: test.into(),
            outcome,
            detail: detail.map(String::from),
        }
    }

    fn gh() -> Fake {
        let mut gh = Fake::default();
        gh.on(
            "POST",
            "repos/sam/game/labels",
            vec![ok(422, json!({"message": "Validation Failed"}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues/",
            vec![ok(201, json!({"id": 1}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues",
            vec![
                ok(201, json!({"number": 31}), &[]),
                ok(201, json!({"number": 32}), &[]),
            ],
        );
        gh
    }

    #[test]
    fn new_and_again() {
        let mut gh = gh();
        let open = [Quarantined {
            test: "old".into(),
            issue: 12,
            author: "github-actions[bot]".into(),
        }];
        let got = file(
            &mut gh,
            "sam/game",
            &[
                entry(
                    "new",
                    Outcome::Flaky,
                    Some("assert left == right\n  left: 1"),
                ),
                entry("old", Outcome::Quarantined, Some("timed out")),
                entry("new", Outcome::Flaky, Some("second")),
                entry("old", Outcome::Flaky, None),
                entry("gone", Outcome::Quarantined, None),
            ],
            &open,
            "[run 5](https://x) on PR #3",
        )
        .unwrap();
        let f = |t: &str, o, n, new| Filed {
            test: t.into(),
            outcome: o,
            issue: n,
            new,
        };
        assert_eq!(
            got,
            vec![
                f("new", Outcome::Flaky, 31, true),
                f("old", Outcome::Quarantined, 12, false),
                f("old", Outcome::Flaky, 12, false),
                f("gone", Outcome::Quarantined, 32, true),
            ]
        );
        let posts = gh.asked("POST");
        // The label once, before the first new issue.
        assert_eq!(posts.iter().filter(|p| p.1.ends_with("/labels")).count(), 1);
        assert_eq!(posts[0].1, "repos/sam/game/labels");
        let new = posts[1].2.as_ref().unwrap();
        assert_eq!(
            (new["title"].as_str(), new["labels"].clone()),
            (Some("flaky: new"), json!(["flaky"]))
        );
        let body = new["body"].as_str().unwrap();
        assert!(body.starts_with("`new` failed, then passed when it ran again on the same tree, in [run 5](https://x) on PR #3."), "{body}");
        assert!(
            body.contains("<summary>The 2 failed tries</summary>")
                && body.contains("~~~\nassert left == right\n  left: 1\n~~~")
                && body.contains("~~~\nsecond\n~~~"),
            "{body}"
        );
        let again = posts[2];
        assert_eq!(again.1, "repos/sam/game/issues/12/comments");
        assert_eq!(
            again.2.as_ref().unwrap()["body"].as_str().unwrap(),
            "Failed again, in [run 5](https://x) on PR #3, where it's quarantined.\n\n<details><summary>The failure</summary>\n\n~~~\ntimed out\n~~~\n\n</details>"
        );
        assert_eq!(
            posts[3].2.as_ref().unwrap()["body"],
            json!(
                "Flaky again, in [run 5](https://x) on PR #3: it failed, then passed when it ran again."
            )
        );
        let gone = posts[4].2.as_ref().unwrap()["body"].as_str().unwrap();
        assert!(gone.starts_with("`gone` failed in [run 5](https://x) on PR #3, where it was quarantined, but no issue"), "{gone}");
        assert_eq!(posts.len(), 5);
    }

    #[test]
    fn nothing_to_file_asks_nothing() {
        let mut gh = Fake::default();
        assert_eq!(file(&mut gh, "sam/game", &[], &[], "x").unwrap(), vec![]);
        assert!(gh.asked.is_empty());
    }

    #[test]
    fn a_failed_label_is_a_failure() {
        let mut gh = Fake::default();
        gh.on(
            "POST",
            "repos/sam/game/labels",
            vec![ok(
                403,
                json!({"message": "Resource not accessible by integration"}),
                &[],
            )],
        );
        let e = file(
            &mut gh,
            "sam/game",
            &[entry("a", Outcome::Flaky, None)],
            &[],
            "x",
        )
        .unwrap_err();
        assert!(e.0.contains("HTTP 403"), "{e}");
    }

    #[test]
    fn long_output_keeps_its_end() {
        let long: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let t = tail(&long);
        assert!(
            t.starts_with("[")
                && t.contains("bytes before this left out]\nline 41\n")
                && t.ends_with("line 100"),
            "{t}"
        );
        let wide = "é".repeat(3000);
        let t = tail(&wide);
        assert!(t.len() <= DETAIL_BYTES + 40 && t.ends_with('é'));
        assert_eq!(tail("short\n"), "short");
        // Text that would close the block can't.
        assert_eq!(fence("a\n~~~~\nb"), "~~~~~\na\n~~~~\nb\n~~~~~");
    }
}
