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
        fs::write(job.join(format!("{}.outcome", n + 1)), "done\n").unwrap();
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
            assert!(got.contains(&format!("Cost: ${cost} recorded over {turns} turns; {missing} runs with cost still missing; {recovered} runs with cost recovered in this span; {outside} earlier runs outside the selected turns whose cost may be included; 0 turns whose engine's stream carries tokens but no cost (their runs' missing and recovered tokens count above); 0 turns whose engine's cost and tokens aren't read.")), "{query}: {got}");
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
                    "{key} {turns} {input} {output} 0 0 ${cost} - 0 {missing} {recovered} {outside} {partial}"
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
        fs::write(job.join("1.outcome"), "done\n").unwrap();
        fs::write(
            job.join("1.jsonl"),
            json!({"type":"result", "total_cost_usd":1,"usage":{"input_tokens":1}}).to_string(),
        )
        .unwrap();
    }
    let unknown = jobs.join("unknown");
    fs::create_dir_all(&unknown).unwrap();
    fs::write(unknown.join("1.started"), "100").unwrap();
    fs::write(unknown.join("1.outcome"), "done\n").unwrap();
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
                rows.contains(&format!("{key} 1 1 0 0 0 $1.00 - 0 0 0 0 0")),
                "{got}"
            );
        }
    }
    assert!(
        rows.contains(&"codex all (unknown) 1 0 0 0 0 - - 0 1 0 0 1".into()),
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
        fs::write(job.join("1.outcome"), "done\n").unwrap();
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
            rows.contains(&format!("{key} 2 2 0 0 0 $2.00 - 0 1 1 0 2")),
            "{got}"
        );
    }
}

#[test]
fn a_jobs_unfinished_turn_and_estimates_are_labelled_and_reconcile_when_it_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let job = jobs.join("live");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("project"), "/src/web").unwrap();
    fs::write(job.join("link"), "https://github.com/o/r/issues/5").unwrap();
    let line = |v: serde_json::Value| format!("{v}\n");
    let turns = [
        (
            "claude",
            line(json!({"type":"burrow.run"}))
                + &line(
                    json!({"type":"assistant","session_id":"a","message":{"id":"m","usage":{"input_tokens":5,"cache_read_input_tokens":900}}}),
                )
                + &line(
                    json!({"type":"result","session_id":"a","usage":{"input_tokens":5,"cache_read_input_tokens":900},"total_cost_usd":2.0}),
                ),
        ),
        (
            "codex",
            line(json!({"type":"thread.started","thread_id":"t"}))
                + &line(
                    json!({"type":"turn.completed","usage":{"input_tokens":2_000_000,"cached_input_tokens":1_000_000,"output_tokens":100_000}}),
                ),
        ),
        // Resumes session a: one call so far, its result half written.
        (
            "claude",
            line(json!({"type":"burrow.run"}))
                + &line(
                    json!({"type":"assistant","session_id":"a","message":{"id":"n","usage":{"input_tokens":5,"cache_read_input_tokens":3000}}}),
                )
                + r#"{"type":"result","session_id":"a","usage":{"input_tokens":5,"cache_read_input_tokens":3000},"total_cost_usd":9"#,
        ),
    ];
    for (n, (engine, stream)) in turns.iter().enumerate() {
        let n = n + 1;
        fs::write(job.join(format!("{n}.engine")), engine).unwrap();
        fs::write(job.join(format!("{n}.started")), (100 * n).to_string()).unwrap();
        fs::write(job.join(format!("{n}.jsonl")), stream).unwrap();
        if n < 3 {
            fs::write(job.join(format!("{n}.outcome")), "done\n").unwrap();
        }
    }
    let mut cfg = config(&tmp.path().join("state"));
    cfg.jobs = Some(jobs);
    cfg.prices = serde_json::from_value(
        json!({"codex":{"input":1.0,"output":8.0,"cacheRead":0.5,"cacheWrite":0.0}}),
    )
    .unwrap();
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    let got = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
    // The assumption is shown with the estimate it made, apart from cost.
    assert!(got.contains("Prices: configured prices (USD per million input/output/cache-read/cache-write tokens), used only where a stream carries no cost: codex 1/8/0.5/0."), "{got}");
    assert!(got.contains("Estimate: $2.30 over 1 of those turns at configured prices, not measured; 0 turns with cost unavailable."), "{got}");
    assert!(got.contains("live o/r#5 turn 2 1970-01-01 00:03:20Z input=1000000 output=100000 cache-read=1000000 cache-write=0 estimate=$2.300000 (codex) context unavailable\n"), "{got}");
    // The unfinished turn counts its call, not its half-written result.
    assert!(got.contains("live o/r#5 turn 3 1970-01-01 00:05:00Z input=5 output=0 cache-read=3000 cache-write=0 known-cost=$0.000000 context=3005 peak=3005; partial tokens (output counts completed runs only); additional cost unavailable; unfinished (no outcome yet: counts to its last complete record)\n"), "{got}");
    // Context is the latest prompt; cache-read is every call's traffic.
    assert!(got.contains("  live o/r#5: 3 turns (3 in span, 1 unfinished), 2 sessions; input=1000010 output=100000 cache-read=1003900 cache-write=0 known-cost=$2.00 estimate=$2.30 (codex); span known-cost=$2.00 estimate=$2.30 (codex); context=3005 peak=3005\n"), "{got}");
    let rows: Vec<_> = got
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert!(
        rows.contains(&"codex all all 1 1000000 100000 1000000 0 - $2.30 0 0 0 0 0".into()),
        "{got}"
    );
    // The turn ends: its result is whole now, and the job's lifetime cost
    // is session a's last total, counted once.
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(job.join("3.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut f, b"}\n").unwrap();
    fs::write(job.join("3.outcome"), "done\n").unwrap();
    let got = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
    assert!(
        got.contains("  live o/r#5: 3 turns (3 in span), 2 sessions"),
        "{got}"
    );
    assert!(
        got.contains("cache-write=0 cost=$9.00 estimate=$2.30 (codex); span cost=$9.00"),
        "{got}"
    );
    // From turn 3 alone, only what it added to session a.
    let got = m.answer(&Query::parse("usage @300").unwrap()).unwrap();
    assert!(
        got.contains("live o/r#5: 3 turns (1 in span), 2 sessions"),
        "{got}"
    );
    assert!(
        got.contains("; span cost=$7.00; context=3005 peak=3005"),
        "{got}"
    );
}
