//! Timers: a line the PR adds that sets one up, outside Markdown and
//! comments. Lines it keeps or takes out don't count, nor does a file GitHub
//! gives no patch for.

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
    passes(&case(vec![change("modified", "modules/x.nix", 1, 0)]));
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
