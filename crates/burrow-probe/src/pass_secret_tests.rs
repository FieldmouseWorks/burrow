use super::*;

// Toy payloads intentionally lack a token shape: only their surrounding
// multiline context identifies them as secret, before it is fragmented.
const PAYLOAD: &str = "QUJDREVGR0hJSktMTU5PUFFS";

fn key(kind: &str, begin: bool, end: bool, lines: usize) -> String {
    let mut text = String::new();
    if begin {
        text += &format!("-----BEGIN {kind} {}-----\n", "PRIVATE KEY");
    }
    for _ in 0..lines {
        text += PAYLOAD;
        text += "\n";
    }
    if end {
        text += &format!("-----END {kind} {}-----\n", "PRIVATE KEY");
    }
    text
}

fn posted(title: &str, body: &str, continuation: bool) -> (Vec<String>, Vec<String>) {
    let mut gh = github(true, json!([issue(9, "sam", title, json!(body))]));
    let mut ran = Vec::new();
    pass(
        &mut gh,
        &config(),
        &mut |q| {
            ran.push(q.to_string());
            if continuation && matches!(q, Query::Usage { .. }) {
                Ok("required totals row\n".repeat(3_600))
            } else {
                Ok(format!("answer to {q}"))
            }
        },
        "now",
    )
    .unwrap();
    let pages = bodies(&gh);
    assert!(pages.iter().all(|p| p.len() <= MAX_COMMENT));
    assert!(
        pages.iter().all(|p| !p.contains(PAYLOAD)),
        "key payload was posted"
    );
    assert_eq!(gh.asked("PATCH").len(), 1);
    (pages, ran)
}

#[test]
fn echoed_requests_redact_complete_and_incomplete_keys_before_refusals() {
    for kind in ["RSA", "OPENSSH", "PGP"] {
        for (begin, end) in [(true, true), (true, false), (false, true)] {
            let mut text = key(kind, begin, end, 3);
            if kind == "PGP" {
                text = text.replace("PRIVATE KEY", "PRIVATE KEY BLOCK");
            }
            // Both request sources: the body, or the multiline title when
            // the body has no questions. No key line reaches a command.
            let (pages, ran) = posted("probe", &text, false);
            assert!(ran.is_empty());
            assert!(pages.join("\n").contains("[redacted private key]"));
            let (_, ran) = posted(&text, "", false);
            assert!(ran.is_empty());
        }
    }
}

#[test]
fn key_context_survives_question_limits_clipping_and_request_normalization() {
    for (begin, end) in [(true, true), (true, false), (false, true)] {
        // More payload lines than MAX_QUESTIONS: an END beyond the question
        // limit must still redact the payload lines that would be echoed.
        let text = key("RSA", begin, end, MAX_QUESTIONS + 3);
        posted("probe", &text, false);
        // A marker beyond MAX_ECHO, and metadata lines normalized away by
        // asked(), must not disappear before their payload is redacted.
        posted("probe", &format!("{} {text}", "prefix".repeat(80)), false);
        posted(
            "probe",
            &text
                .lines()
                .map(|l| format!("- `{l}`"))
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        );
        if begin {
            let hidden = text.replacen("-----BEGIN", "<!-- -----BEGIN", 1);
            posted("probe", &hidden, false);
            let fenced = text.replacen("-----BEGIN", "``` -----BEGIN", 1);
            posted("probe", &fenced, false);
        }
    }
    // Keep the question limit after preprocessing: more than ten ordinary
    // questions still run only ten, and omitted questions are counted.
    let text = format!(
        "{}\n{}",
        key("RSA", true, true, 3),
        ["memory"; MAX_QUESTIONS + 2].join("\n")
    );
    let (pages, ran) = posted("probe", &text, false);
    assert!(ran.len() <= MAX_QUESTIONS);
    assert!(
        pages
            .join("\n")
            .contains("Only the first 10 questions are answered")
    );
}

#[test]
fn echoed_key_redaction_holds_on_continuation_pages() {
    for kind in ["RSA", "OPENSSH", "PGP"] {
        for (begin, end) in [(true, true), (true, false), (false, true)] {
            let mut text = key(kind, begin, end, MAX_QUESTIONS + 2);
            if kind == "PGP" {
                text = text.replace("PRIVATE KEY", "PRIVATE KEY BLOCK");
            }
            // First issue: a large required answer precedes the key and
            // puts its refusal on a later page. For an orphan END the
            // scrubber intentionally removes everything before that END,
            // so put the surviving large question after the key instead.
            let body = if begin {
                format!("usage --totals\n{text}")
            } else {
                format!("{text}\nusage --totals")
            };
            let (pages, ran) = posted("probe", &body, true);
            assert!(pages.len() > 1);
            assert_eq!(ran, ["usage -5h --totals"]);
            assert_eq!(
                pages
                    .iter()
                    .flat_map(|p| p.lines())
                    .filter(|l| *l == "required totals row")
                    .count(),
                3_600
            );
        }
    }
}

#[test]
fn redacting_request_blocks_preserves_surrounding_questions() {
    let text = key("RSA", true, true, 2).replace(PAYLOAD, &format!("{PAYLOAD}\ndeployed"));
    let (pages, ran) = posted("probe", &format!("memory\n{text}units"), false);
    assert_eq!(ran, ["memory", "units"]);
    let body = pages.join("\n");
    assert!(body.contains("answer to memory") && body.contains("answer to units"));
    assert!(!body.contains("answer to deployed"));
}

const SHA: &str = "3520fa4c1b9e2d7f6a8b0c3d5e7f9a1b2c4d6e8f";

#[test]
fn valid_questions_run_as_asked_even_when_they_look_like_secrets() {
    let questions = format!("contains sam/token {SHA}\njournal secret:worker -1h now");
    let want = [
        format!("contains sam/token {SHA}"),
        "journal secret:worker.service -1h now".to_string(),
    ];
    // From the body, and from the title when the body has no questions.
    let (_, ran) = posted("probe", &questions, false);
    assert_eq!(ran, want);
    let (_, ran) = posted(&questions, "", false);
    assert_eq!(ran, want);
    // A credential rule's match that would run from one line into the next
    // leaves the next question as it was asked.
    for (first, then) in [
        ("memory password:", "units"),
        ("deployed Authorization:", "queue"),
        ("rc token", &*format!("contains sam/repo {SHA}")),
    ] {
        let (_, ran) = posted("probe", &format!("{first}\n{then}"), false);
        assert_eq!(ran, [then], "after {first:?}");
    }
    // Around a key block, past the question limit and across pages: the
    // block goes whole, and the questions outside it run unchanged.
    for (begin, end) in [(true, true), (true, false), (false, true)] {
        let block = key("RSA", begin, end, MAX_QUESTIONS + 2);
        let body = if begin {
            format!("usage --totals\n{questions}\n{block}")
        } else {
            format!("{block}{questions}\nusage --totals")
        };
        let (pages, ran) = posted("probe", &body, true);
        assert!(pages.len() > 1);
        let mut expected = want.to_vec();
        expected.insert(if begin { 0 } else { 2 }, "usage -5h --totals".into());
        assert_eq!(ran, expected);
    }
}

#[test]
fn a_refusal_hides_a_secret_the_line_before_identifies() {
    let value = "hunter2hunter2hunter2hunter2";
    for before in ["password:", "Authorization:", "token", "cookie ="] {
        let (pages, ran) = posted("probe", &format!("memory\n{before}\n{value}\nunits"), false);
        assert_eq!(ran, ["memory", "units"]);
        let body = pages.join("\n");
        assert!(!body.contains(value), "after {before:?}: {body}");
        assert!(body.contains("[redacted]"));
    }
}
