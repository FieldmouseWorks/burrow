use super::Request;

#[test]
fn each_undo_cn_takes() {
    let p = Request::parse;
    assert_eq!(
        p("undo sam/web#12").unwrap(),
        Request::Change {
            repo: "sam/web".into(),
            pr: 12
        }
    );
    assert_eq!(
        p("undo --actor https://claude.ai/code/session_01Ab_c")
            .unwrap()
            .args(),
        ["undo", "--actor", "https://claude.ai/code/session_01Ab_c"]
    );
    let since = Request::Since {
        since: 1_793_750_400_000,
        repo: "SAM-ORG/web.site".into(),
    };
    assert_eq!(
        p("undo --since 2026-11-04T00:00:00Z --repo SAM-ORG/web.site").unwrap(),
        since
    );
    assert_eq!(
        p("undo --repo SAM-ORG/web.site --since 2026-11-03T16:00:00-08:00").unwrap(),
        since
    );
    // A repo's name may start with a dot; which repos it reaches is cn's
    // to say, from the ledger.
    assert_eq!(
        p("undo sam/.github#3").unwrap(),
        Request::Change {
            repo: "sam/.github".into(),
            pr: 3
        }
    );
    assert_eq!(
        p("undo --since 2026-11-04T00:00:00Z --repo other/.github").unwrap(),
        Request::Since {
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
        assert!(Request::parse(line).is_err(), "{line}");
    }
}
