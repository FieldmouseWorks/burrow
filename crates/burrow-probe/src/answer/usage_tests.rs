use super::*;
use serde_json::json;

#[test]
fn usage_totals_preserve_cost_recovery_and_partial_tokens_at_every_level() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let job = jobs.join("example");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("role"), "repair").unwrap();
    fs::write(job.join("project"), "/src/_infra").unwrap();
    let result = |cost| json!({"type":"result", "session_id":"s", "total_cost_usd":cost, "usage":{"input_tokens":10,"output_tokens":3}});
    let crash = json!({"type":"result", "session_id":"s", "subtype":"error_during_execution", "total_cost_usd":0, "usage":{"input_tokens":0}});
    for (n, value) in [
        result(1.5),
        crash,
        result(2.0),
        json!({"type":"result", "session_id":"s", "total_cost_usd":2.25}),
    ]
    .iter()
    .enumerate()
    {
        fs::write(
            job.join(format!("{}.started", n + 1)),
            ((n + 1) * 100).to_string(),
        )
        .unwrap();
        fs::write(job.join(format!("{}.jsonl", n + 1)), value.to_string()).unwrap();
    }
    let mut cfg = config(&tmp.path().join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    // since, now, turns, tokens, cost, missing, recovered, outside, partial tokens
    for (since, now, turns, input, output, cost, missing, recovered, outside, partial) in [
        (300, 300, 1, 10, 3, "0.50", 0, 0, 1, 0), // excluded crash
        (200, 300, 2, 10, 3, "0.50", 0, 1, 0, 1), // recovered crash
        (0, 300, 3, 20, 6, "2.00", 0, 1, 0, 1),
        (200, 200, 1, 0, 0, "0.00", 1, 0, 0, 1), // recovery is in the future
        (400, 400, 1, 0, 0, "0.25", 0, 0, 0, 1), // tokens missing, cost complete
        (200, 400, 3, 10, 3, "0.75", 0, 1, 0, 2),
    ] {
        let m = Machine {
            cfg: &cfg,
            root: "/".into(),
            now,
            uid: 1,
            run: &fake,
        };
        for totals in [false, true] {
            let query = format!("usage @{since}{}", if totals { " --totals" } else { "" });
            let got = m.answer(&Query::parse(&query).unwrap()).unwrap();
            assert!(got.contains(&format!("Cost: ${cost} recorded over {turns} turns; {missing} runs with cost still missing; {recovered} runs with cost recovered in this span; {outside} earlier runs outside the selected turns whose cost may be included; 0 turns whose engine's cost and tokens aren't read.")), "{query}: {got}");
            assert!(
                got.contains(&format!("Tokens: {partial} turns with partial counts")),
                "{got}"
            );
            let rows: Vec<_> = got
                .lines()
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect();
            for key in [
                "claude all all",
                "claude repair all",
                "claude all _infra",
                "claude repair _infra",
            ] {
                let row = format!(
                    "{key} {turns} {input} {output} 0 0 ${cost} 0 {missing} {recovered} {outside} {partial}"
                );
                assert!(rows.contains(&row), "missing {row}: {got}");
            }
        }
    }
}

#[test]
fn usage_projects_keep_supported_basenames_distinct_and_escape_display() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let names = [
        ".github",
        "_infra",
        "normal.repo-2",
        "space name",
        "back\\slash",
        "非ascii",
    ];
    for (n, name) in names.iter().enumerate() {
        let job = jobs.join(format!("job-{n}"));
        fs::create_dir_all(&job).unwrap();
        fs::write(job.join("project"), format!("/src/{name}")).unwrap();
        fs::write(job.join("1.started"), "100").unwrap();
        fs::write(
            job.join("1.jsonl"),
            json!({"type":"result", "total_cost_usd":1,"usage":{"input_tokens":1}}).to_string(),
        )
        .unwrap();
    }
    let unknown = jobs.join("unknown");
    fs::create_dir_all(&unknown).unwrap();
    fs::write(unknown.join("1.started"), "100").unwrap();
    fs::write(unknown.join("1.engine"), "codex").unwrap();
    fs::write(unknown.join("1.jsonl"), "{}").unwrap();
    let mut cfg = config(&tmp.path().join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 200,
        uid: 1,
        run: &fake,
    };
    let got = m
        .answer(&Query::parse("usage @0 --totals").unwrap())
        .unwrap();
    let rows: Vec<_> = got
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    for shown in [
        ".github",
        "_infra",
        "normal.repo-2",
        "space\\x20name",
        "back\\\\slash",
        "\\u{975e}ascii",
    ] {
        for key in [
            format!("claude all {shown}"),
            format!("claude (none) {shown}"),
        ] {
            assert!(
                rows.contains(&format!("{key} 1 1 0 0 0 $1.00 0 0 0 0 0")),
                "{got}"
            );
        }
    }
    assert!(
        rows.contains(&"codex all (unknown) 1 - - - - - 1 0 0 0 0".into()),
        "{got}"
    );
    assert!(
        !rows.iter().any(|r| r.starts_with("claude all (unknown)")),
        "{got}"
    );
}

#[test]
fn usage_totals_recover_appended_runs_without_matching_other_jobs_gaps() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let result = |cost| {
        json!({"type":"result","session_id":"s","usage":{"input_tokens":1},"total_cost_usd":cost})
            .to_string()
    };
    let crash = json!({"type":"result","session_id":"s","subtype":"error_during_execution","usage":{"input_tokens":0},"total_cost_usd":0}).to_string();
    for (name, stream) in [
        ("a", format!("{}\n{crash}\n{}", result(1.5), result(2.0))),
        ("b", crash),
    ] {
        let job = jobs.join(name);
        fs::create_dir_all(&job).unwrap();
        fs::write(job.join("role"), "repair").unwrap();
        fs::write(job.join("project"), "/src/web").unwrap();
        fs::write(job.join("1.started"), "100").unwrap();
        fs::write(job.join("1.jsonl"), stream).unwrap();
    }
    let mut cfg = config(&tmp.path().join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 200,
        uid: 1,
        run: &fake,
    };
    let got = m
        .answer(&Query::parse("usage @0 --totals").unwrap())
        .unwrap();
    assert!(got.contains("1 runs with cost still missing; 1 runs with cost recovered in this span; 0 earlier runs outside"), "{got}");
    let rows: Vec<_> = got
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    for key in [
        "claude all all",
        "claude repair all",
        "claude all web",
        "claude repair web",
    ] {
        assert!(
            rows.contains(&format!("{key} 2 2 0 0 0 $2.00 0 1 1 0 2")),
            "{got}"
        );
    }
}
