use super::{Request, Undo};

const WHY: &str = " --reason it broke the site";

fn undo(line: &str) -> Undo {
    Request::parse(&format!("{line}{WHY}")).unwrap().undo
}

#[test]
fn each_undo_cn_takes() {
    assert_eq!(
        undo("undo sam/web#12"),
        Undo::Change {
            repo: "sam/web".into(),
            pr: 12
        }
    );
    assert_eq!(
        Request::parse(&format!(
            "undo --actor https://claude.ai/code/session_01Ab_c{WHY}"
        ))
        .unwrap()
        .args(),
        [
            "undo",
            "--actor",
            "https://claude.ai/code/session_01Ab_c",
            "--reason=it broke the site"
        ]
    );
    let since = Undo::Since {
        since: 1_793_750_400_000,
        repo: "SAM-ORG/web.site".into(),
    };
    assert_eq!(
        undo("undo --since 2026-11-04T00:00:00Z --repo SAM-ORG/web.site"),
        since
    );
    assert_eq!(
        undo("undo --repo SAM-ORG/web.site --since 2026-11-03T16:00:00-08:00"),
        since
    );
    // A repo's name may start with a dot; which repos it reaches is cn's
    // to say, from the ledger.
    assert_eq!(
        undo("undo sam/.github#3"),
        Undo::Change {
            repo: "sam/.github".into(),
            pr: 3
        }
    );
    assert_eq!(
        undo("undo --since 2026-11-04T00:00:00Z --repo other/.github"),
        Undo::Since {
            since: 1_793_750_400_000,
            repo: "other/.github".into()
        }
    );
    assert_eq!(
        since.args(),
        [
            "undo",
            "--since",
            "1793750400000",
            "--repo",
            "SAM-ORG/web.site"
        ]
    );
}

#[test]
fn its_reason_is_the_rest_of_the_line() {
    let reason = |line: &str| Request::parse(line).unwrap().reason;
    // As written, its own spacing and words kept, `--reason` among them.
    assert_eq!(
        reason(
            "undo sam/web#1 --reason  Red run https://github.com/sam/web/actions/runs/9:  check / gate failed --reason  "
        ),
        "Red run https://github.com/sam/web/actions/runs/9:  check / gate failed --reason"
    );
    assert_eq!(reason("undo sam/web#1 --reason=it broke"), "it broke");
    // No shell reads it: its quotes are its own.
    assert_eq!(reason("undo sam/web#1 --reason 'it broke'"), "'it broke'");
    // One argument to cn, whatever it opens with.
    let r = Request::parse("undo sam/web#1 --reason --actor x").unwrap();
    assert_eq!(r.args(), ["undo", "sam/web#1", "--reason=--actor x"]);
    // A comment shows it in no backticks of its own.
    let r = Request::parse("undo sam/web#1 --reason `rm` broke it").unwrap();
    assert_eq!(r.to_string(), "undo sam/web#1 --reason='rm' broke it");
}

#[test]
fn nothing_else() {
    for line in [
        "undo",
        "undo sam/web",
        "undo sam/web#0",
        "undo sam/web#-1",
        "undo sam/web#1 now",
        "undo sam/.#1",
        "undo sam/..#1",
        "undo .sam/web#1",
        "undo sa_m/web#1",
        "undo sam/web/x#1",
        "undo sam/w$b#1",
        "undo --actor",
        "undo --actor https://claude.ai/code/session_",
        "undo --actor https://claude.ai/code/session_01;x",
        "undo --since yesterday --repo sam/web",
        "undo --since 2026-11-04T00:00:00Z",
        "undo --since 2026-11-04T00:00:00Z --repo sam/..",
        "Undo sam/web#1",
        "revert sam/web#1",
    ] {
        let why = format!("{line}{WHY}");
        assert!(Request::parse(&why).is_err(), "{why}");
    }
    // Every undo says why: one with no reason, or a blank one, is none.
    for line in [
        "undo sam/web#1",
        "undo sam/web#1 --reason",
        "undo sam/web#1 --reason   ",
        "undo sam/web#1 --reason=",
        "undo --actor https://claude.ai/code/session_01Ab_c",
        "undo --since 2026-11-04T00:00:00Z --repo sam/web",
        "undo sam/web#1 x--reason it broke",
        "undo sam/web#1 --reasons it broke",
    ] {
        let err = Request::parse(line).unwrap_err();
        assert!(
            err.contains("doesn't say why") || err.contains("isn't one undo"),
            "{line}: {err}"
        );
    }
    assert!(
        Request::parse("undo sam/web#1")
            .unwrap_err()
            .contains("doesn't say why")
    );
}
