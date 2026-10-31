//! The title: `area: plain sentence`, the house commit style, since the
//! steward squash-merges it into main's subject. An area is a module,
//! directory or tool, up to three words, several joined by commas; a proper
//! noun can start the sentence. Renovate's rebase mark doesn't count.

use crate::*;

fn rust() -> Vec<serde_json::Value> {
    vec![change("modified", "src/main.rs", 1, 1)]
}

#[test]
fn good() {
    for title in [
        "autoland: merge only the head that was checked",
        "lib/project.nix: build the docs",
        "merge train: land the head it checked",
        "autoland, updates: say what the sweep did",
        ".github: pin the actions",
        "deps: update dependency vite to v7",
        "deps: lock file maintenance (nix)",
        "rebase!deps: update dependency vite to v7",
        "autoland: Codex looks again after a push",
        "tests/autoland: cover the land",
    ] {
        passes(&case(rust()).title(title));
    }
}

#[test]
fn bad() {
    for (title, why) in [
        ("Update dependency vite to v7", "an area and a colon"),
        (
            "chore(deps): update dependency vite to v7",
            "an area and a colon",
        ),
        ("build(web)!: require Node 24", "an area and a colon"),
        ("fix: merge the head", "a commit type"),
        ("Feat: a sentence", "a commit type"),
        ("autoland, chore: tidy", "a commit type"),
        ("autoland: merge the head.", "a period"),
        ("autoland:", "an area and a colon"),
        ("autoland: ", "no sentence"),
        ("autoland:  merge", "no sentence"),
        ("autoland:merge", "an area and a colon"),
        ("a long area of four words: merge", "an area and a colon"),
    ] {
        let c = case(rust()).title(title);
        let o = c.run();
        assert_eq!(
            said(&o.posted, "title"),
            [("4ead".into(), "failure", "this run")],
            "{title}"
        );
        let p = o
            .posted
            .iter()
            .find(|p| p.status.context == "house/title (#7)")
            .unwrap();
        assert!(p.status.description.contains(why), "{title}: {p:?}");
        fails(&c, "title");
    }
}

// The title is never printed: it's the author's text.
#[test]
fn never_printed() {
    let o = case(rust()).title("::warning::from the title").run();
    assert!(!o.out.contains("from the title"), "{}", o.out);
    assert!(
        !o.posted
            .iter()
            .any(|p| p.status.description.contains("from the title"))
    );
}

// An edited title is judged again, like the body.
#[test]
fn edited() {
    let o = case(rust())
        .title("Update things")
        .change(
            1,
            Change {
                title: Some("tools: update things"),
                ..Change::default()
            },
        )
        .run();
    assert_eq!(o.result, Ok(true));
    assert_eq!(
        said(&o.posted, "title"),
        [
            ("4ead".into(), "failure", "this run"),
            ("4ead".into(), "success", "this run")
        ]
    );
}

// A fork's PR gets the title as the run's result too.
#[test]
fn fork() {
    let mut c = case(rust()).title("Update things");
    c.fork = Fork::Yes;
    fails(&c, "title");
}
