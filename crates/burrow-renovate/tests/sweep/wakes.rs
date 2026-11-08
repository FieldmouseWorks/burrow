//! Failed scans and withdrawn requests retain their deadlines.
use super::*;

fn booked(m: &Machine) -> Value {
    serde_json::from_slice(&fs::read(m.path("state/wake.json")).unwrap()).unwrap()
}

#[test]
fn failed_listings_preserve_every_deadline_and_fail_the_pass() {
    for response in ["command failure", "not json", "{}", "[null]", "cap"] {
        let m = Machine::new();
        let before = json!({REPO: now() + 90, "sam/other": now() + 120});
        fs::create_dir_all(m.path("state")).unwrap();
        fs::write(m.path("state/wake.json"), before.to_string()).unwrap();
        if response == "command failure" {
            m.set("list-fails", "");
        } else if response == "cap" {
            m.prs(&vec![pr_with(20, &[], "FAILURE"); 1000]);
        } else {
            m.set("prs", response);
        }
        let got = m.sweep(&[]);
        assert_eq!(got.code, Some(1), "{}", got.err);
        assert!(got.jobs.is_empty());
        assert_eq!(booked(&m), before);
    }
}

#[test]
fn withdrawal_guards_book_wakes_regardless_of_red_checks() {
    for guard in ["title", "local", "timeline", "unreadable", "changed head"] {
        let m = Machine::new();
        let mut pr = pr_with(27, &[], "FAILURE");
        match guard {
            "title" => pr["title"] = "rebase!Update foo27 to v2".into(),
            "local" => m.keep(27, ".released", &format!("{} 1 {}\n", sha(27), now() - 60)),
            "timeline" | "changed head" => {
                let at = jiff::Timestamp::from_second(now() as i64 - 60).unwrap();
                m.set("timeline-27", &json!([[{"event": "unlabeled", "created_at": at.to_string(), "label": {"name": "rebase"}}]]).to_string());
                if guard == "changed head" {
                    pr["headRefOid"] = "b".repeat(40).into();
                }
            }
            _ => m.set("timeline-27", "not json"),
        }
        m.prs(&[pr]);
        let got = m.pass();
        assert!(got.jobs.is_empty(), "{guard}");
        let retry = booked(&m)[REPO].as_u64().unwrap();
        assert!(retry >= now() + 3590 && retry <= now() + 3600, "{guard}");
        // Once the deferred withdrawal finishes, hand it over and clear
        // the deadline even though its CI still fails.
        m.set("timeline-27", "[[]]");
        if guard == "local" {
            m.keep(
                27,
                ".released",
                &format!("{} 1 {}\n", sha(27), now() - 7300),
            );
        }
        m.prs(&[pr_with(27, &[], "FAILURE")]);
        assert!(m.pass().task().contains("#27 "));
        assert!(booked(&m).get(REPO).is_none());
    }
}

#[test]
fn a_scan_that_stops_midway_preserves_the_previous_deadlines() {
    let m = Machine::new();
    let before = json!({REPO: now() + 90, "sam/other": now() + 120});
    fs::create_dir_all(m.path("state/sam_r-21.released")).unwrap();
    fs::write(m.path("state/wake.json"), before.to_string()).unwrap();
    m.prs(&[
        m.refresh(20, now() + 30, "me", LOCK),
        pr_with(21, &["rebase"], "FAILURE"),
    ]);
    let got = m.sweep(&[]);
    assert_eq!(got.code, Some(1));
    assert!(got.jobs.is_empty());
    assert_eq!(booked(&m), before);
}
