//! Timers: a line the PR adds that sets one up, outside Markdown and
//! comments. Lines it keeps or takes out don't count. A file GitHub gives no
//! patch for is read whole.

use crate::*;

const REASON: &str = "Why a timer: the heartbeat a watchdog waits for.";

fn adding(path: &str, line: &str) -> Vec<serde_json::Value> {
    vec![patched(
        change("modified", path, 1, 0),
        adds(&[line], &["{"], &[]),
    )]
}

#[test]
fn timers() {
    for (path, line) in [
        ("modules/x.nix", "    systemd.timers.x = {"),
        ("modules/x.nix", "  systemd.user.timers.x.wantedBy = [ ];"),
        ("modules/x.nix", "      OnCalendar = \"hourly\";"),
        ("modules/x.nix", "    startAt = \"daily\";"),
        (
            "modules/x.nix",
            "    timerConfig.OnUnitActiveSec = \"5min\";",
        ),
        ("deploy/x.timer", "OnBootSec=1min"),
        (".github/workflows/x.yml", "    - cron: '0 * * * *'"),
        ("hosts/x.nix", "  services.cron.systemCronJobs = ["),
        ("src/x.rs", "let unit = \"OnCalendar=daily\";"),
    ] {
        fails(&case(adding(path, line)), "timers");
        passes(&case(adding(path, line)).body(REASON));
        passes(&case(adding(path, line)).body("Why a timer: **the watchdog's** heartbeat ping"));
        passes(&case(vec![patched(
            change("modified", path, 0, 1),
            adds(&[], &["{"], &[line]),
        )]));
        passes(&case(vec![patched(
            change("modified", path, 1, 1),
            adds(&["x = 1;"], &[line], &[]),
        )]));
    }
}

#[test]
fn not_timers() {
    for line in [
        "# no OnCalendar here: a push starts it",
        "  // startAt = \"daily\" was the old way",
        "  -- OnBootSec",
        "; OnCalendar=daily",
        "  * OnCalendar, once",
        "    startAtBoot = true;",
        "  restartAt = 3;",
        "  description = \"cron jobs, gone\";",
    ] {
        passes(&case(adding("modules/x.nix", line)));
    }
    passes(&case(adding(
        "docs/timers.md",
        "Set `OnCalendar = \"hourly\"`.",
    )));
    passes(&case(adding(
        "docs/TIMERS.Markdown",
        "Set `OnCalendar = \"hourly\"`.",
    )));
}

// A file GitHub gives no patch for (a diff too big to show), or a patch
// short of its lines: the lines it adds are the ones its head has more of than its old side, the merge base
// at its old path. A new file has no old side, and a binary one adds none.
#[test]
fn no_patch() {
    let timer = "OnCalendar = \"hourly\";";
    let x = "modules/x.nix";
    let c = |status: &str, path: &str| case(vec![unpatched(change(status, path, 1, 0))]);
    for (now, was) in [
        (format!("{{\n{timer}\n}}\n"), "{\n}\n".to_owned()),
        // One more of a line it had.
        (format!("a\n{timer}\n{timer}\n"), format!("{timer}\na\n")),
    ] {
        fails(
            &c("modified", x).tree("4ead", x, &now).tree("3e7b", x, &was),
            "timers",
        );
    }
    // Or a patch cut short: fewer added lines than GitHub counts.
    let short = patched(change("modified", x, 2, 0), adds(&["a"], &[], &[]));
    fails(
        &case(vec![short])
            .tree("4ead", x, &format!("a\n{timer}\n"))
            .tree("3e7b", x, ""),
        "timers",
    );
    // Kept, moved about.
    passes(
        &c("modified", x)
            .tree("4ead", x, &format!("a\n{timer}\n"))
            .tree("3e7b", x, &format!("{timer}\nb\n")),
    );
    fails(
        &c("added", "deploy/x.timer").tree("4ead", "deploy/x.timer", "OnBootSec=1min\n"),
        "timers",
    );
    let renamed = || {
        case(vec![unpatched(from(
            change("renamed", "m/b.nix", 1, 0),
            "m/a.nix",
        ))])
    };
    let now = format!("{timer}\nx\n");
    fails(
        &renamed()
            .tree("4ead", "m/b.nix", &now)
            .tree("3e7b", "m/a.nix", "x\n"),
        "timers",
    );
    passes(
        &renamed()
            .tree("4ead", "m/b.nix", &now)
            .tree("3e7b", "m/a.nix", &now),
    );
    // A comment, Markdown, a binary file: none.
    passes(
        &c("modified", x)
            .tree("4ead", x, "# OnCalendar is gone\n")
            .tree("3e7b", x, ""),
    );
    let md = "docs/x.md";
    passes(
        &c("modified", md)
            .tree("4ead", md, &format!("{timer}\n"))
            .tree("3e7b", md, ""),
    );
    let bin = "assets/x.bin";
    passes(&c("added", bin).tree("4ead", bin, &format!("\0{timer}\n")));
    // Nothing added: nothing read.
    let o = passes(&case(vec![unpatched(from(
        change("renamed", "m/b.nix", 0, 0),
        "m/a.nix",
    ))]));
    assert!(
        !o.calls.iter().any(|c| c.starts_with("content")),
        "{:?}",
        o.calls
    );
    // No answer for it is no verdict.
    let o = c("modified", x).tree("3e7b", x, "").unread("4ead", x).run();
    assert!(o.result.is_err() && o.posted.is_empty(), "{}", o.out);
}

// Paths are printed quoted.
#[test]
fn quoted() {
    let o = case(vec![patched(
        change("added", "x\n::warning::y.nix", 1, 0),
        adds(&["startAt = \"daily\";"], &[], &[]),
    )])
    .run();
    assert!(
        !o.out.lines().any(|l| l.starts_with("::warning")),
        "{}",
        o.out
    );
    status(&o.posted, &["timers"], &RULES);
}

#[test]
fn fork() {
    let mut c = case(adding("x.nix", "startAt = \"daily\";"));
    c.fork = Fork::Yes;
    fails(&c, "timers");
}
