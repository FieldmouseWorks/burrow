//! A gate's report on its tests' issues. A flaky test goes on the issue
//! that quarantined it when the run began, whatever's become of it since: a
//! run that began before a fix merged can't quarantine its test again. One
//! that wasn't quarantined then goes on its open issue, or opens one, which
//! quarantines it.

use serde_json::json;

use crate::quarantine::{Quarantine, Quarantined};
use crate::report::Entry;
use crate::test::{EXPIRES, LABEL, title};
use burrow_github::{Api, Error, call};

/// Where the rules are, for whoever reads an issue.
const DOCS: &str = "https://github.com/FieldmouseWorks/burrow/blob/main/docs/flaky-tests.md";
/// How much of a failed try's output an issue shows: its end.
const DETAIL_LINES: usize = 60;
const DETAIL_BYTES: usize = 4000;

#[derive(Debug, PartialEq, Eq)]
pub struct Filed {
    pub test: String,
    pub issue: u64,
    /// A new issue: the test wasn't quarantined until now.
    pub new: bool,
}

/// File each flaky test (one reported twice gets one comment), in the order
/// the report has them. `start` is the quarantine the gate was given, `open`
/// the issues that quarantine a test now (quarantine::list, both). `place`
/// says where it happened, in Markdown: a run's link, a PR, a commit. A test
/// reported flaky whose quarantine had run out is an error, and nothing is
/// filed: the gate was told its failure fails, and passed it on a second
/// try.
pub fn file(
    api: &mut dyn Api,
    repo: &str,
    entries: &[Entry],
    start: &Quarantine,
    open: &[Quarantined],
    place: &str,
) -> Result<Vec<Filed>, Error> {
    let held = &start.held;
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for e in entries {
        if start.expired.contains(&e.test) {
            return Err(Error(format!(
                "the gate passed `{}` on a second try, but its quarantine had run out: its failure fails the gate",
                e.test
            )));
        }
        let detail = e.detail.as_deref().filter(|d| !d.trim().is_empty());
        match groups.iter_mut().find(|(t, _)| *t == e.test) {
            Some(g) => g.1.extend(detail),
            None => groups.push((&e.test, detail.into_iter().collect())),
        }
    }
    let mut labelled = false;
    let mut out = Vec::new();
    for (test, details) in groups {
        let shown = show(&details);
        let on = |qs: &[Quarantined]| qs.iter().find(|q| q.test == test).map(|q| q.issue);
        let flaked = "it failed, then passed when it ran again.";
        let again = match (on(held), on(open)) {
            (Some(n), now) if now == Some(n) => {
                Some((n, format!("Flaky again, in {place}: {flaked}")))
            }
            (Some(n), _) => Some((
                n,
                format!(
                    "Flaky again, in {place}, which began while this issue quarantined it: {flaked} This issue \
                     doesn't quarantine it any more."
                ),
            )),
            // Opened by another run since this one began.
            (None, Some(n)) => Some((n, format!("Flaky again, in {place}: {flaked}"))),
            (None, None) => None,
        };
        if let Some((n, said)) = again {
            call(
                api,
                "POST",
                &format!("repos/{repo}/issues/{n}/comments"),
                Some(&json!({"body": said + &shown})),
                None,
            )?;
            out.push(Filed {
                test: test.into(),
                issue: n,
                new: false,
            });
            continue;
        }
        if !labelled {
            label(api, repo)?;
            labelled = true;
        }
        let days = EXPIRES / 86400;
        let body = format!(
            "`{test}` failed, then passed when it ran again on the same tree, in {place}.\n\nWhile this issue is \
             open the test is quarantined: a Claude job looks for the cause, and the pull request with its fix \
             closes this issue. Every gate still runs the test meanwhile. A failure that passes on its second try \
             fails nothing and is noted here, as any flaky test's does; one that fails both tries fails the gate. \
             {days} days after this issue opened, the quarantine runs out: until the issue closes, any failure of \
             the test fails the gate at once, with no second try. How it works: [burrow's \
             docs/flaky-tests.md]({DOCS}).{shown}"
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
fn show(details: &[&str]) -> String {
    let summary = match details.len() {
        0 => return String::new(),
        1 => "The failed try".to_string(),
        n => format!("The {n} failed tries"),
    };
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
    use burrow_github::fake::{Fake, ok};

    fn entry(test: &str, detail: Option<&str>) -> Entry {
        Entry {
            test: test.into(),
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

    fn q(test: &str, issue: u64) -> Quarantined {
        Quarantined {
            test: test.into(),
            issue,
            author: "github-actions[bot]".into(),
        }
    }

    fn held(q: &[Quarantined]) -> Quarantine {
        Quarantine {
            held: q.to_vec(),
            expired: Vec::new(),
        }
    }

    #[test]
    fn new_and_again() {
        let mut gh = gh();
        // `gone`'s fix merged while the gate ran, closing its issue, and
        // another run opened `mid`'s.
        let held = held(&[q("old", 12), q("gone", 20)]);
        let open = [q("mid", 40), q("old", 12)];
        let got = file(
            &mut gh,
            "sam/game",
            &[
                entry("new", Some("assert left == right\n  left: 1")),
                entry("old", Some("timed out")),
                entry("new", Some("second")),
                entry("gone", None),
                entry("mid", None),
            ],
            &held,
            &open,
            "[run 5](https://x) on PR #3",
        )
        .unwrap();
        let f = |t: &str, n, new| Filed {
            test: t.into(),
            issue: n,
            new,
        };
        assert_eq!(
            got,
            vec![
                f("new", 31, true),
                f("old", 12, false),
                f("gone", 20, false),
                f("mid", 40, false),
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
            body.contains("one that fails both tries fails the gate")
                && body.contains("2 days after this issue opened, the quarantine runs out")
                && body.contains("<summary>The 2 failed tries</summary>")
                && body.contains("~~~\nassert left == right\n  left: 1\n~~~")
                && body.contains("~~~\nsecond\n~~~"),
            "{body}"
        );
        let again = posts[2];
        assert_eq!(again.1, "repos/sam/game/issues/12/comments");
        assert_eq!(
            again.2.as_ref().unwrap()["body"].as_str().unwrap(),
            "Flaky again, in [run 5](https://x) on PR #3: it failed, then passed when it ran again.\n\n<details><summary>The failed try</summary>\n\n~~~\ntimed out\n~~~\n\n</details>"
        );
        // Noted where it was quarantined, and quarantined again nowhere.
        assert_eq!(posts[3].1, "repos/sam/game/issues/20/comments");
        let gone = posts[3].2.as_ref().unwrap()["body"].as_str().unwrap();
        assert!(
            gone.starts_with("Flaky again, in [run 5](https://x) on PR #3, which began while this issue quarantined it")
                && gone.ends_with("doesn't quarantine it any more."),
            "{gone}"
        );
        assert_eq!(posts[4].1, "repos/sam/game/issues/40/comments");
        assert_eq!(posts.len(), 5);
    }

    #[test]
    fn a_test_whose_quarantine_ran_out_cant_pass_on_a_second_try() {
        let mut gh = gh();
        let start = Quarantine {
            held: vec![q("held", 4), q("stale", 5)],
            expired: vec!["stale".into()],
        };
        let e = file(
            &mut gh,
            "sam/game",
            &[entry("wobbly", None), entry("stale", None)],
            &start,
            &start.held,
            "x",
        )
        .unwrap_err();
        assert!(
            e.0.contains("passed `stale` on a second try, but its quarantine had run out"),
            "{e}"
        );
        assert!(gh.asked.is_empty());
    }

    #[test]
    fn nothing_to_file_asks_nothing() {
        let mut gh = Fake::default();
        assert_eq!(
            file(&mut gh, "sam/game", &[], &Quarantine::default(), &[], "x").unwrap(),
            vec![]
        );
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
            &[entry("a", None)],
            &Quarantine::default(),
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
