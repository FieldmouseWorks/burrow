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

#[test]
fn usage_shows_the_api_credit_meter_in_both_forms_and_it_survives_the_scrubber() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let jobs = tmp.path().join("jobs");
    // 2026-11-20T00:00:00Z, the balance read then; the credit renews on the
    // 25th, so 2026-11-21T12:00:00Z has 4 days left, today included.
    let since = 1_795_132_800;
    let now = since + 36 * 3600;
    let job = jobs.join("fallback");
    fs::create_dir_all(&job).unwrap();
    let put = |n: u64, engine: &str, at: i64, total: f64| {
        fs::write(job.join(format!("{n}.engine")), engine).unwrap();
        fs::write(job.join(format!("{n}.started")), at.to_string()).unwrap();
        let line = json!({"type":"result","session_id":"s","usage":{"input_tokens":1},"total_cost_usd":total,"result":"SECRET PROMPT"});
        fs::write(job.join(format!("{n}.jsonl")), format!("{line}\n")).unwrap();
    };
    put(1, "claude", since + 3600, 3.0); // the subscription's part of session s
    put(2, "claude-api", since + 7200, 13.0); // yesterday: $10
    put(3, "claude-api", now - 3600, 14.5); // today: $1.50
    let mut cfg = config(&state);
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now,
        uid: 1,
        run: &fake,
    };
    let unset = m.answer(&Query::parse("usage --totals").unwrap()).unwrap();
    assert!(
        unset.contains("claude-api credit: no room (no API credit is set)"),
        "{unset}"
    );
    fs::create_dir_all(state.join("settings")).unwrap();
    fs::write(
        state.join(burrow_settings::credit::FILE),
        json!({"credit": {"budgetUsd": 214.37, "renews": "2026-11-25", "monthlyUsd": 200, "reserveUsd": 5, "since": since}, "receipts": {}}).to_string(),
    )
    .unwrap();
    for query in ["usage", "usage --totals"] {
        let got = m.answer(&Query::parse(query).unwrap()).unwrap();
        let got = crate::scrub::Redactor::default().plain(&got);
        for line in [
            "API credit at 2026-11-21 12:00:00Z:",
            "claude-api credit: room",
            "cycle 2026-11-20 00:00:00Z to 2026-11-25 00:00:00Z (credit expires then), 4 UTC days left with today; the balance as set",
            "budget $214.37, spent $11.50, reserve $5.00, remaining $202.87",
            "today spent $1.50 of $51.0925 allowed",
        ] {
            assert!(got.lines().any(|l| l == line), "{query}: {line}: {got}");
        }
        assert!(!got.contains("SECRET"), "{got}");
        let at = |s: &str| got.find(s).unwrap_or_else(|| panic!("{s}: {got}"));
        assert!(at("By engine") < at("API credit at"));
        if query == "usage" {
            assert!(at("API credit at") < at("Meter readings:"));
        }
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn usage_aggregates_normalized_records_across_engines_without_reading_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let jobs = tmp.path().join("jobs");
    let now = 2_000_000_000;

    // Engine 1: antigravity with ONLY `<N>.usage.json` (no `.jsonl` file at all).
    let ag = jobs.join("ag-job");
    fs::create_dir_all(&ag).unwrap();
    fs::write(ag.join("role"), "repair").unwrap();
    fs::write(ag.join("project"), "/src/web").unwrap();
    fs::write(ag.join("1.started"), (now - 100).to_string()).unwrap();
    fs::write(ag.join("1.engine"), "antigravity").unwrap();
    let ag_usage = crate::usage_record::TurnUsage {
        version: crate::usage_record::VERSION,
        engine: "antigravity".into(),
        model: Some("model-b".into()),
        runs: vec![crate::usage_record::RunUsage {
            engine: "antigravity".into(),
            started_at: now - 100,
            model: Some("model-b".into()),
            input_tokens: 150,
            output_tokens: 40,
            cache_read_tokens: 25,
            cache_write_tokens: 5,
            cost_usd: Some(0.12),
            ..Default::default()
        }],
    };
    fs::write(
        ag.join("1.usage.json"),
        serde_json::to_string(&ag_usage).unwrap(),
    )
    .unwrap();

    // Engine 2: claude with `<N>.usage.json` AND deliberately bogus `.jsonl`
    // to prove `burrow-probe` does NOT read provider streams when normalized records exist.
    let cl = jobs.join("cl-job");
    fs::create_dir_all(&cl).unwrap();
    fs::write(cl.join("role"), "repair").unwrap();
    fs::write(cl.join("project"), "/src/web").unwrap();
    fs::write(cl.join("1.started"), (now - 50).to_string()).unwrap();
    fs::write(cl.join("1.engine"), "claude").unwrap();
    fs::write(
        cl.join("1.jsonl"),
        "{\"type\":\"result\",\"usage\":{\"input_tokens\":999999},\"total_cost_usd\":999.0}\n",
    )
    .unwrap();
    let cl_usage = crate::usage_record::TurnUsage {
        version: crate::usage_record::VERSION,
        engine: "claude".into(),
        model: Some("model-a".into()),
        runs: vec![crate::usage_record::RunUsage {
            engine: "claude".into(),
            started_at: now - 50,
            model: Some("model-a".into()),
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 10,
            cache_write_tokens: 0,
            cost_usd: Some(0.25),
            ..Default::default()
        }],
    };
    fs::write(
        cl.join("1.usage.json"),
        serde_json::to_string(&cl_usage).unwrap(),
    )
    .unwrap();

    // Meter log with both claude and antigravity readings.
    let meter_log = state.join("usage-reset/meters/log");
    fs::create_dir_all(meter_log.parent().unwrap()).unwrap();
    fs::write(
        &meter_log,
        format!(
            "{}\n{}\n",
            json!({"engine":"claude","read_at":now - 30,"windows":{"five_hour":{"used":42.0,"resets_at":now + 3600}}}),
            json!({"engine":"antigravity","read_at":now - 20,"windows":{"daily":{"used":15.0,"resets_at":now + 3600}}}),
        ),
    )
    .unwrap();

    let mut cfg = config(&state);
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now,
        uid: 1,
        run: &fake,
    };

    let got = m
        .answer(&Query::parse(&format!("usage @{}", now - 200)).unwrap())
        .unwrap();
    assert!(got.contains("antigravity: 1 turns"), "{got}");
    assert!(got.contains("claude: 1 turns"), "{got}");
    assert!(got.contains("Cost: $0.37 recorded over 2 turns;"), "{got}");
    assert!(
        !got.contains("999999"),
        "stream file must not be read when normalized usage exists: {got}"
    );
    assert!(got.contains("claude five_hour: 42%"), "{got}");
    assert!(got.contains("antigravity daily: 15%"), "{got}");

    let rows: Vec<_> = got
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert!(
        rows.contains(&"antigravity all all 1 150 40 25 5 $0.12 0 0 0 0 0".into()),
        "{got}"
    );
    assert!(
        rows.contains(&"claude all all 1 100 20 10 0 $0.25 0 0 0 0 0".into()),
        "{got}"
    );
}

#[test]
fn credit_meter_reads_normalized_usage_records_without_stream_files() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let jobs = tmp.path().join("jobs");
    let since = 1_795_132_800;
    let now = since + 36 * 3600;
    let job = jobs.join("norm-api");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("1.engine"), "claude-api").unwrap();
    fs::write(job.join("1.started"), (now - 600).to_string()).unwrap();
    let usage = crate::usage_record::TurnUsage {
        version: crate::usage_record::VERSION,
        engine: "claude-api".into(),
        model: Some("model-a".into()),
        runs: vec![crate::usage_record::RunUsage {
            engine: "claude-api".into(),
            started_at: now - 600,
            budget_micros: Some(50_000_000),
            input_tokens: 50,
            output_tokens: 10,
            cost_usd: Some(4.25),
            ..Default::default()
        }],
    };
    fs::write(
        job.join("1.usage.json"),
        serde_json::to_string(&usage).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(state.join("settings")).unwrap();
    let credit_file = state.join(burrow_settings::credit::FILE);
    fs::write(
        &credit_file,
        json!({"credit": {"budgetUsd": 100.0, "renews": "2026-11-25", "monthlyUsd": 100, "reserveUsd": 5, "since": since}, "receipts": {}}).to_string(),
    )
    .unwrap();
    let meter = crate::credit::meter(&credit_file, Some(&jobs), now).unwrap();
    assert_eq!(meter.spent, 4_250_000);
    assert!(meter.room());
}

#[test]
fn normalized_turn_usage_round_trips_multi_turn_claude_cost_recovery_without_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let job = jobs.join("roundtrip");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("role"), "repair").unwrap();
    fs::write(job.join("project"), "/src/web").unwrap();

    let result = |cost| json!({"type":"result","session_id":"s","total_cost_usd":cost,"usage":{"input_tokens":10,"output_tokens":3}});
    let crash = json!({"type":"result","session_id":"s","subtype":"error_during_execution","total_cost_usd":0,"usage":{"input_tokens":0}});
    for (n, val) in [result(1.5), crash, result(2.25)].iter().enumerate() {
        let turn = (n + 1) as u64;
        fs::write(
            job.join(format!("{turn}.started")),
            (turn * 100).to_string(),
        )
        .unwrap();
        fs::write(job.join(format!("{turn}.engine")), "claude").unwrap();
        fs::write(job.join(format!("{turn}.jsonl")), format!("{val}\n")).unwrap();
        let norm = crate::usage_record::TurnUsage::from_job_turn(
            &job,
            "roundtrip",
            turn,
            "claude",
            Some("model-a".into()),
            (turn * 100) as i64,
        )
        .unwrap();
        fs::write(
            job.join(format!("{turn}.usage.json")),
            serde_json::to_string(&norm).unwrap(),
        )
        .unwrap();
    }
    // Delete all provider stream files to prove `burrow-probe` relies purely on `.usage.json`.
    for turn in 1..=3 {
        fs::remove_file(job.join(format!("{turn}.jsonl"))).unwrap();
    }

    let mut cfg = config(&tmp.path().join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    let got = m
        .answer(&Query::parse("usage @0 --totals").unwrap())
        .unwrap();
    assert!(
        got.contains("Cost: $2.25 recorded over 3 turns; 0 runs with cost still missing; 1 runs with cost recovered in this span; 0 earlier runs outside"),
        "{got}"
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn usage_groups_normalized_failover_runs_by_invocation_engine_keeping_turn_counts_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let job = jobs.join("failover");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("role"), "repair").unwrap();
    fs::write(job.join("project"), "/src/web").unwrap();
    fs::write(job.join("1.started"), "100").unwrap();
    // After a refusal triggers failover from claude to claude-api, N.engine records the final engine.
    fs::write(job.join("1.engine"), "claude-api").unwrap();
    let norm = crate::usage_record::TurnUsage {
        version: crate::usage_record::VERSION,
        engine: "claude-api".into(),
        model: Some("model-a".into()),
        runs: vec![
            crate::usage_record::RunUsage {
                engine: "claude".into(),
                started_at: 100,
                model: Some("model-a".into()),
                session: Some("sess-1".into()),
                budget_micros: None,
                input_tokens: 120,
                output_tokens: 30,
                cache_read_tokens: 10,
                cache_write_tokens: 5,
                cost_usd: Some(0.40),
                cumulative_cost_usd: Some(0.40),
                tokens_partial: true,
                cost_partial: false,
                prior_gap: false,
                missing: Vec::new(),
                recovered: Vec::new(),
            },
            crate::usage_record::RunUsage {
                engine: "claude-api".into(),
                started_at: 120,
                model: Some("model-a".into()),
                session: Some("sess-1".into()),
                budget_micros: Some(5_000_000),
                input_tokens: 340,
                output_tokens: 70,
                cache_read_tokens: 20,
                cache_write_tokens: 15,
                cost_usd: Some(1.10),
                cumulative_cost_usd: Some(1.50),
                tokens_partial: true,
                cost_partial: false,
                prior_gap: false,
                missing: Vec::new(),
                recovered: Vec::new(),
            },
        ],
    };
    fs::write(
        job.join("1.usage.json"),
        serde_json::to_string(&norm).unwrap(),
    )
    .unwrap();

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
    assert!(got.contains("claude: 0 turns\n"), "{got}");
    assert!(got.contains("claude-api: 1 turns\n"), "{got}");
    assert!(
        got.contains("Cost: $1.50 recorded over 1 turns; 0 runs with cost still missing; 0 runs with cost recovered in this span; 0 earlier runs outside"),
        "{got}"
    );
    assert!(
        got.contains("Tokens: 1 turns with partial counts (output counts completed runs only)."),
        "{got}"
    );
    let by_engine = got
        .split("By engine:\n")
        .nth(1)
        .and_then(|s| s.split("By engine and role:\n").next())
        .unwrap_or("");
    let rows: Vec<Vec<&str>> = by_engine
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec![
                "claude", "all", "all", "0", "120", "30", "10", "5", "$0.40", "0", "0", "0", "0",
                "1"
            ],
            vec![
                "claude-api",
                "all",
                "all",
                "1",
                "340",
                "70",
                "20",
                "15",
                "$1.10",
                "0",
                "0",
                "0",
                "0",
                "1"
            ],
        ],
        "{got}"
    );
}

#[test]
fn normalized_turn_usage_records_per_invocation_models_without_stamping_final_model_onto_earlier_runs()
 {
    let tmp = tempfile::tempdir().unwrap();
    let job = tmp.path().join("rerouted");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("1.started"), "100").unwrap();
    fs::write(job.join("1.engine"), "claude-api").unwrap();

    // Run 0: claude invocation with its own model recorded on burrow.run.
    // Run 1: claude invocation whose model comes from the stream's system/init record.
    // Run 2: claude invocation whose model is unknown (neither marker nor stream names one).
    // Run 3: final claude-api invocation after failover, whose model comes from the final turn choice.
    let lines = [
        json!({"type":"burrow.run","at":100,"engine":"claude","model":"model-a"}),
        json!({"type":"result","session_id":"s1","total_cost_usd":0.25,"usage":{"input_tokens":50,"output_tokens":10}}),
        json!({"type":"burrow.run","at":105,"engine":"claude"}),
        json!({"type":"system","subtype":"init","session_id":"s1","model":"model-stream"}),
        json!({"type":"result","session_id":"s1","total_cost_usd":0.40,"usage":{"input_tokens":30,"output_tokens":5}}),
        json!({"type":"burrow.run","at":110,"engine":"claude"}),
        json!({"type":"result","session_id":"s1","subtype":"error_during_execution","total_cost_usd":0,"usage":{"input_tokens":0}}),
        json!({"type":"burrow.run","at":120,"engine":"claude-api","budget":5_000_000}),
        json!({"type":"result","session_id":"s1","total_cost_usd":0.85,"usage":{"input_tokens":90,"output_tokens":20}}),
    ];
    let stream = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(job.join("1.jsonl"), stream).unwrap();

    let norm = crate::usage_record::TurnUsage::from_job_turn(
        &job,
        "rerouted",
        1,
        "claude-api",
        Some("model-b".into()),
        100,
    )
    .unwrap();
    assert_eq!(norm.engine, "claude-api");
    assert_eq!(norm.model.as_deref(), Some("model-b"));
    assert_eq!(norm.runs.len(), 4);
    assert_eq!(norm.runs[0].engine, "claude");
    assert_eq!(
        norm.runs[0].model.as_deref(),
        Some("model-a"),
        "earlier invocation must keep its own recorded marker model"
    );
    assert_eq!(norm.runs[1].engine, "claude");
    assert_eq!(
        norm.runs[1].model.as_deref(),
        Some("model-stream"),
        "earlier invocation must keep its own stream-reported model"
    );
    assert_eq!(norm.runs[2].engine, "claude");
    assert_eq!(
        norm.runs[2].model, None,
        "earlier invocation with unknown model must not inherit the final engine's model"
    );
    assert_eq!(norm.runs[3].engine, "claude-api");
    assert_eq!(norm.runs[3].model.as_deref(), Some("model-b"));
}
