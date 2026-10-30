//! rust-first: a PR that adds Python, or grows Python and shell past a fix,
//! says why it isn't Rust.

use crate::*;

const PY: &str = "#!/usr/bin/env python3\nprint()\n";
const SH: &str = "#!/usr/bin/env bash\necho\n";

fn new_tool() -> Vec<serde_json::Value> {
    vec![change("added", "tools/new_tool.py", 120, 0)]
}

// No Python or shell in the change: it passes, body or none, and reads the
// PR as it is now, and its files as the comparison of its base and the
// event's head, which never change.
#[test]
fn none() {
    let o = passes(&case(vec![
        change("modified", "src/main.rs", 300, 10),
        change("added", "docs/x.md", 40, 0),
    ]));
    assert_eq!(o.calls[0], "pull 7");
    assert_eq!(o.calls[1], "compare ba5e...4ead");
    // The code file it grows is read at its head, for its length.
    assert_eq!(o.calls[2], "content \"src/main.rs\"@4ead");
    assert_eq!(o.calls[3..8], ["post 4ead"; 5]);
    assert_eq!(o.calls[8..], ["pull 7"]);
}

// The case it's for: a new Python file and no reason. It fails, and names
// neither the files nor anything from the body in the error; nothing from
// the body is printed or posted at all.
#[test]
fn new_python() {
    let error = fails(
        &case(new_tool()).body("Adds a tool.\n\n::warning::from the body"),
        "rust-first",
    );
    assert!(
        !error.contains("new_tool") && !error.contains("from the body"),
        "{error}"
    );
    let o = case(new_tool()).body("::warning::from the body").run();
    assert!(!o.out.contains("from the body"), "{}", o.out);
    assert!(
        !o.posted
            .iter()
            .any(|p| p.status.description.contains("from the body"))
    );
}

// With the line it passes, however the line is written: in the middle of
// the body, in any case, bold, in a list or a quote, after a CRLF from
// GitHub's editor.
#[test]
fn the_line() {
    for body in [
        REASON.to_owned(),
        format!("Adds a tool.\n\n{REASON}\n\nRefs #1"),
        "why not rust: still tuning it, ported after".into(),
        "**Why not Rust:** pytest is the harness here".into(),
        "- Why not Rust: a one-off migration script".into(),
        "> Why not Rust: a one-off migration script".into(),
        "x\r\nWhy not Rust: a one-off migration script\r\n".into(),
    ] {
        passes(&case(new_tool()).body(&body));
    }
    // But not with nothing after it, or the words somewhere else in a line.
    for body in [
        "Why not Rust:",
        "Why not Rust: **",
        "Why not Rust:\r\nnext line",
        "We said why not Rust: no",
        "Why not Rust",
    ] {
        fails(&case(new_tool()).body(body), "rust-first");
    }
}

// A reason says why in a sentence: a word or three isn't one, and the
// status says the line is there but gives no reason.
#[test]
fn a_reason() {
    for body in [
        "Why not Rust: one-off",
        "Why not Rust: n/a",
        "Why not Rust: coin toss",
        "Why not Rust: - it is",
    ] {
        let c = case(new_tool()).body(body);
        let error = fails(&c, "rust-first");
        assert!(
            error.contains("gives no reason") && error.contains("4 words or more"),
            "{error}"
        );
        let o = c.run();
        let p = &o.posted[1].status;
        assert!(
            p.description
                .ends_with("the body's 'Why not Rust:' line gives no reason"),
            "{p:?}"
        );
    }
    let o = case(new_tool()).run();
    assert!(
        o.posted[1]
            .status
            .description
            .ends_with("the body has no 'Why not Rust:' line")
    );
}

// Stubs, a file copied to a new one, and one renamed from something else
// into Python are new Python too: shell into Python as well.
#[test]
fn into_python() {
    fails(
        &case(vec![change("added", "pkg/types.pyi", 10, 0)]),
        "rust-first",
    );
    fails(
        &case(vec![from(change("copied", "pkg/b.py", 0, 0), "pkg/a.py")]),
        "rust-first",
    );
    fails(
        &case(vec![from(
            change("renamed", "tools/run.py", 3, 1),
            "tools/run.sh",
        )]),
        "rust-first",
    );
    fails(
        &case(vec![from(
            change("renamed", "tools/run.py", 3, 1),
            "tools/run.txt",
        )]),
        "rust-first",
    );
    // A Python file renamed within Python is growth, like an edit.
    passes(&case(vec![from(
        change("renamed", "tools/b.py", 5, 2),
        "tools/a.py",
    )]));
    fails(
        &case(vec![from(
            change("renamed", "tools/b.py", GROWTH as u64 + 1, 0),
            "tools/a.py",
        )]),
        "rust-first",
    );
}

// Growth: lines added to the Python and shell it had, net of all it takes
// out. Up to the limit is a fix; past it needs the reason; across files and
// languages it adds up.
#[test]
fn growth() {
    let g = GROWTH as u64;
    passes(&case(vec![change("modified", "m/a.py", g, 0)]));
    fails(
        &case(vec![change("modified", "m/a.py", g + 1, 0)]),
        "rust-first",
    );
    fails(
        &case(vec![
            change("modified", "m/a.py", 30, 0),
            change("modified", "m/b.py", 30, 5),
        ]),
        "rust-first",
    );
    passes(&case(vec![
        change("modified", "m/a.py", 30, 0),
        change("modified", "m/b.py", 30, 10),
    ]));
    passes(&case(vec![change("modified", "m/a.sh", g, 0)]));
    fails(
        &case(vec![change("modified", "m/a.sh", g + 1, 0)]),
        "rust-first",
    );
    fails(
        &case(vec![
            change("modified", "m/a.py", 30, 0),
            change("modified", "m/b.bash", 30, 5),
        ]),
        "rust-first",
    );
    // Lines taken out offset it, a deleted file's whole length included, so
    // a port that patches a caller as it deletes what it ported passes.
    passes(&case(vec![
        change("modified", "m/a.py", 200, 0),
        change("removed", "m/old.py", 0, 400),
    ]));
    passes(&case(vec![
        change("modified", "m/a.sh", 200, 0),
        change("removed", "m/old.sh", 0, 400),
    ]));
    passes(&case(vec![change("modified", "m/a.py", g + 1, 0)]).body(REASON));
    // A repo can set its own limit.
    let mut c = case(vec![change("modified", "m/a.py", 11, 0)]);
    c.growth = 10;
    fails(&c, "rust-first");
}

// Shell is held to the same bar: a new script counts its lines, so glue
// passes and a feature in bash needs the reason, as does one renamed into
// shell from something else.
#[test]
fn shell() {
    let g = GROWTH as u64;
    for name in ["tools/run.sh", "tools/run.bash", "tools/rc.zsh"] {
        passes(&case(vec![change("added", name, 30, 0)]));
        passes(&case(vec![change("added", name, g, 0)]));
        fails(&case(vec![change("added", name, g + 1, 0)]), "rust-first");
        passes(
            &case(vec![change("added", name, g + 1, 0)])
                .body("Why not Rust: Gradle generates this wrapper"),
        );
    }
    fails(
        &case(vec![from(
            change("renamed", "tools/run.sh", 80, 0),
            "tools/run.txt",
        )]),
        "rust-first",
    );
    // Two small scripts add up like one big one.
    fails(
        &case(vec![
            change("added", "a.sh", 30, 0),
            change("added", "b.sh", 30, 0),
        ]),
        "rust-first",
    );
}

// A file with no extension is Python or shell by its shebang at the head,
// and was by its shebang at the merge base (3e7b), the old side of the PR's
// diff: never the base branch's tip (ba5e), which may have moved on. Others
// aren't, and aren't read.
#[test]
fn by_shebang() {
    fails(
        &case(vec![change("added", "bin/wend", 80, 0)]).tree("4ead", "bin/wend", PY),
        "rust-first",
    );
    fails(
        &case(vec![change("added", "bin/tool", 80, 0)]).tree("4ead", "bin/tool", SH),
        "rust-first",
    );
    passes(&case(vec![change("added", "bin/tool", 30, 0)]).tree("4ead", "bin/tool", SH));
    passes(&case(vec![change("added", "bin/data", 80, 0)]).tree("4ead", "bin/data", "just text\n"));
    passes(
        &case(vec![
            change("modified", "bin/wend", 200, 0),
            change("removed", "bin/old", 0, 300),
        ])
        .tree("4ead", "bin/wend", "#!/usr/bin/python\n")
        .tree("3e7b", "bin/wend", PY)
        .tree("3e7b", "bin/old", "#!/usr/bin/python3\n")
        .tree("ba5e", "bin/wend", PY),
    );
    let o = passes(&case(vec![
        change("added", "README.md", 80, 0),
        change("added", "x/y.toml", 9, 0),
    ]));
    assert!(
        !o.calls.iter().any(|c| c.starts_with("content")),
        "{:?}",
        o.calls
    );
    // The comparison, and with it the merge base, is asked for once, however
    // many paths need it, and each file's content once, however many rules
    // read it.
    let o = passes(
        &case(vec![
            change("modified", "bin/a", 3, 1),
            change("modified", "bin/b", 3, 1),
        ])
        .tree("4ead", "bin/a", SH)
        .tree("4ead", "bin/b", SH)
        .tree("3e7b", "bin/a", SH)
        .tree("3e7b", "bin/b", SH),
    );
    assert_eq!(
        o.calls.iter().filter(|c| c.starts_with("compare")).count(),
        1,
        "{:?}",
        o.calls
    );
    assert_eq!(
        o.calls.iter().filter(|c| c.starts_with("content")).count(),
        4,
        "{:?}",
        o.calls
    );
    // Paths are read whole: a space, a percent sign, a backslash, a tab, a
    // newline (github.rs sends them so).
    for odd in [
        "my dir/run 100%",
        "bin/we\\ird",
        "bin/tab\there",
        "bin/new\nline",
    ] {
        fails(
            &case(vec![change("added", odd, 5, 0)]).tree("4ead", odd, "#! /usr/bin/env python\n"),
            "rust-first",
        );
        passes(&case(vec![change("removed", odd, 0, 5)]).tree(
            "3e7b",
            odd,
            "#! /usr/bin/env python\n",
        ));
    }
    // And printed quoted: a name can't start a line of its own, where the
    // runner would take it for a workflow command.
    let o = case(vec![change("added", "tools/a\n::warning::b.py", 5, 0)]).run();
    assert!(
        !o.out.lines().any(|l| l.starts_with("::warning")),
        "{}",
        o.out
    );
    status(&o.posted, &["rust-first"], &RULES);
}

// A file that changes language, by its name or its shebang, counts as the
// language it becomes: into Python is new Python, whatever its lines; out of
// Python and shell gives back the lines its diff takes out.
#[test]
fn changing_language() {
    fails(
        &case(vec![change("modified", "bin/run", 10, 10)])
            .tree("4ead", "bin/run", PY)
            .tree("3e7b", "bin/run", SH),
        "rust-first",
    );
    fails(
        &case(vec![change("changed", "bin/run", 1, 1)])
            .tree("4ead", "bin/run", PY)
            .tree("3e7b", "bin/run", SH),
        "rust-first",
    );
    passes(
        &case(vec![
            change("modified", "m/a.py", 200, 0),
            change("modified", "bin/run", 30, 300),
        ])
        .tree("4ead", "bin/run", SH)
        .tree("3e7b", "bin/run", PY),
    );
    passes(&case(vec![
        change("modified", "m/a.py", 200, 0),
        from(change("renamed", "src/a.rs", 250, 300), "tools/a.py"),
    ]));
    passes(&case(vec![
        change("modified", "m/a.sh", 200, 0),
        from(change("renamed", "src/a.rs", 250, 300), "tools/a.sh"),
    ]));
}

// GitHub lists 300 of a comparison's files at most: one it leaves out could
// break any rule that reads the files, so a PR with that many needs every
// line whatever they hold. The title is judged as ever.
#[test]
fn cut_short() {
    let many: Vec<_> = (0..300)
        .map(|i| change("modified", &format!("src/{i}.rs"), 1, 1))
        .collect();
    let diff_rules = ["rust-first", "timers", "file-size", "model-names"];
    let o = case(many.clone()).run();
    let errors = o.errors();
    assert!(
        o.result == Ok(true)
            && errors.len() == 4
            && errors.iter().all(|e| e.contains("the most GitHub lists")),
        "{}",
        o.out
    );
    status(&o.posted, &diff_rules, &RULES);
    let every = diff_rules
        .map(|r| format!("{} it's a mass rename of everything", wants(r).1))
        .join("\n");
    passes(&case(many.clone()).body(&every));
    fails(
        &case(many.clone()).body(&every.replace("Why a timer:", "Why a clock:")),
        "timers",
    );
    passes(&case(many[1..].to_vec()));
}

// GitHub not answering fails the run, a file's content included: no answer
// is no verdict, never "not a script".
#[test]
fn no_answer() {
    let mut c = case(new_tool()).body(REASON);
    c.down = true;
    let o = c.run();
    assert!(o.result.is_err() && o.posted.is_empty(), "{}", o.out);
    for c in [
        case(vec![change("added", "bin/wend", 80, 0)]).body(REASON),
        case(vec![change("removed", "bin/wend", 0, 80)])
            .body(REASON)
            .tree("ba5e", "bin/wend", PY),
    ] {
        let o = c.run();
        assert!(
            o.result.is_err() && !o.out.contains("the body says") && o.posted.is_empty(),
            "{}",
            o.out
        );
    }
}

// A fork's PR (or one whose fork is gone) runs with a token that can't post
// a status: the run's result is its verdict, and nothing is posted.
#[test]
fn forks() {
    for fork in [Fork::Yes, Fork::Gone] {
        let mut c = case(new_tool());
        c.fork = fork;
        fails(&c, "rust-first");
        passes(&c.clone().body(REASON));
        let mut rust = case(vec![change("modified", "src/main.rs", 1, 1)]);
        rust.fork = fork;
        passes(&rust);
    }
}
