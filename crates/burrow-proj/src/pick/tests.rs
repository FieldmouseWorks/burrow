// proj's choice of who takes a slot, on what a waiting turn would see:
// pipeline jobs' slots stay theirs, one project's jobs stop at its cap while
// the others go by, pipeline ones count against no cap, and lead ones go
// ahead of everyone; and which jobs are pipeline ones: only those in the
// pipeline's own repos and the machine's flake, and lead ones those in its
// lead repos.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Class, Slot, Waiter, class, demoted, github_repo, pick, pipeline_project, place};
use crate::config::Config;
use crate::config::tests::example;

fn config() -> Config {
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.tools.git = "git".into();
    cfg.pipeline_repos = vec!["Sam/Burrow".into(), "sam/box".into()];
    cfg.lead_repos = vec![];
    cfg.flake = "/nowhere".into();
    cfg
}

/// The slots, then the line: "free", or a holder's "CLASS PROJECT", for
/// slots 1 to 4 in order; then "NAME CLASS PROJECT" for each waiting turn.
/// What's picked is "NAME N", or "" for nobody.
fn case(cfg: &Config, want: &str, specs: &[&str]) {
    let (mut slots, mut line) = (Vec::new(), Vec::new());
    for (i, s) in specs.iter().enumerate() {
        if i < cfg.max_jobs {
            slots.push(match s.split_once(' ') {
                None => Slot::Free,
                Some((c, project)) => Slot::Held {
                    class: Class::parse(c),
                    project: project.into(),
                },
            });
        } else {
            // Fields split at their first spaces only: a project's own stay in it.
            let (name, rest) = s.split_once(' ').unwrap();
            let (c, project) = rest.split_once(' ').unwrap();
            line.push(Waiter {
                name: name.into(),
                class: Class::parse(c).unwrap(),
                project: project.into(),
            });
        }
    }
    let got = pick(cfg, &slots, &line)
        .map(|(i, n)| format!("{} {n}", line[i].name))
        .unwrap_or_default();
    assert_eq!(got, want, "from {specs:?}");
}

#[test]
fn picks() {
    let cfg = config();
    // The head of the line takes the lowest free slot.
    case(
        &cfg,
        "a 1",
        &[
            "free",
            "free",
            "free",
            "free",
            "a normal /src/web",
            "b normal /src/lok",
        ],
    );
    case(
        &cfg,
        "a 3",
        &[
            "normal /src/x",
            "normal /src/y",
            "free",
            "free",
            "a normal /src/web",
            "b first /src/lok",
        ],
    );
    // Nothing free: nobody goes.
    case(
        &cfg,
        "",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "pipeline /src/b",
            "a pipeline /src/b",
        ],
    );
    // The last slot is the pipeline's: nobody else takes it, and a pipeline
    // turn behind them does.
    case(
        &cfg,
        "",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "free",
            "a normal /src/web",
        ],
    );
    case(
        &cfg,
        "p 4",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "free",
            "a normal /src/web",
            "p pipeline /src/b",
        ],
    );
    // Holders proj can't name (from before it named them) are someone else's.
    case(&cfg, "", &["- -", "- -", "- -", "free", "a first /src/web"]);
    // A pipeline turn takes the lowest free slot, leaving its own for the next.
    case(
        &cfg,
        "p 2",
        &["normal /src/x", "free", "free", "free", "p pipeline /src/b"],
    );
    // A project holding its cap's slots waits, and lets the others by.
    case(
        &cfg,
        "b 3",
        &[
            "normal /src/psiv",
            "first /src/psiv",
            "free",
            "free",
            "a normal /src/psiv",
            "b normal /src/lok",
        ],
    );
    case(
        &cfg,
        "",
        &[
            "normal /src/psiv",
            "normal /src/psiv",
            "free",
            "free",
            "a first /src/psiv",
        ],
    );
    // A pipeline repo's pipeline jobs don't count against it, and aren't held by it.
    case(
        &cfg,
        "a 3",
        &[
            "pipeline /src/remi",
            "normal /src/remi",
            "free",
            "free",
            "a normal /src/remi",
        ],
    );
    case(
        &cfg,
        "p 3",
        &[
            "normal /src/remi",
            "normal /src/remi",
            "free",
            "free",
            "p pipeline /src/remi",
        ],
    );
    // Projects are told apart whole, spaces and all; a turn whose project
    // can't be read is in none.
    case(
        &cfg,
        "b 3",
        &[
            "normal /src/my web",
            "normal /src/my web",
            "free",
            "free",
            "a normal /src/my web",
            "b normal /src/my",
        ],
    );
    case(
        &cfg,
        "a 3",
        &[
            "normal /src/x",
            "normal /src/x",
            "free",
            "free",
            "a normal ",
        ],
    );
    // A lead turn goes first, from anywhere in the line: ahead of pipeline
    // turns that came before it, and of everyone else.
    case(
        &cfg,
        "l 1",
        &[
            "free",
            "free",
            "free",
            "free",
            "p pipeline /src/remi",
            "a first /src/web",
            "l lead /src/burrow",
        ],
    );
    case(
        &cfg,
        "l 4",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "free",
            "p pipeline /src/remi",
            "l lead /src/burrow",
        ],
    );
    // It takes the pipeline's slot as a pipeline turn does, counts against
    // no cap, and holds none of its project's.
    case(
        &cfg,
        "l 4",
        &[
            "normal /src/burrow",
            "normal /src/burrow",
            "normal /src/z",
            "free",
            "a normal /src/web",
            "l lead /src/burrow",
        ],
    );
    case(
        &cfg,
        "a 3",
        &[
            "lead /src/burrow",
            "normal /src/burrow",
            "free",
            "free",
            "a normal /src/burrow",
        ],
    );
    // With no slot free nobody goes, lead or not; of two lead turns, the
    // first in line goes first.
    case(
        &cfg,
        "",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "pipeline /src/b",
            "l lead /src/burrow",
        ],
    );
    case(
        &cfg,
        "l 2",
        &[
            "lead /src/burrow",
            "free",
            "free",
            "free",
            "p pipeline /src/remi",
            "l lead /src/burrow",
            "m lead /src/burrow",
        ],
    );
}

/// Clones and jobs in a scratch dir.
struct Scratch(tempfile::TempDir);

impl Scratch {
    fn new() -> Scratch {
        Scratch(tempfile::tempdir().unwrap())
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.path().join(rel)
    }

    fn at(&self, rel: &str) -> String {
        self.path(rel).to_str().unwrap().to_owned()
    }

    fn clone(&self, name: &str, origin: &str) {
        let dir = self.path(name);
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        fs::create_dir_all(&dir).unwrap();
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", origin]);
    }

    /// A job's dir: run with --pipeline or not, in a project.
    fn job(&self, name: &str, pipeline: bool, project: Option<&str>) -> PathBuf {
        let d = self.path(&format!("jobs/{name}"));
        fs::create_dir_all(&d).unwrap();
        if pipeline {
            fs::write(d.join("pipeline"), "").unwrap();
        }
        if let Some(p) = project {
            fs::write(d.join("project"), format!("{p}\n")).unwrap();
        }
        d
    }
}

fn cls(cfg: &Config, dir: &Path, given: Class, want: Class) {
    assert_eq!(
        class(cfg, dir, given),
        want,
        "class {} {given:?}",
        dir.display()
    );
}

fn plc(cfg: &Config, name: &str, dir: &Path, want: Class, renamed: &str) {
    assert_eq!(
        place(cfg, name, dir),
        (want, renamed.to_owned()),
        "place {name} {}",
        dir.display()
    );
}

fn dem(cfg: &Config, dir: &Path, want: &str) {
    assert_eq!(
        demoted(cfg, dir).unwrap_or_default(),
        want,
        "demoted {}",
        dir.display()
    );
}

// A job run with --pipeline is a pipeline one only in a project of one of
// the pipeline's own repos, told by its clone's origin: on GitHub, in any
// form git takes, any case. Anywhere else it goes as --first, so pick holds
// it to its project's cap and off the pipeline's slots: one repo's PRs can't
// fill the machine.
#[test]
fn pipeline_repos() {
    let mut cfg = config();
    let t = Scratch::new();
    for (name, url) in [
        ("https", "https://github.com/sam/burrow.git"),
        ("slash", "https://github.com/sam/burrow/"),
        ("user", "https://sam@github.com/sam/burrow"),
        ("ssh", "ssh://git@github.com/sam/burrow.git"),
        ("port", "ssh://git@github.com:22/sam/burrow"),
        ("scp", "git@github.com:SAM/BOX"),
        ("psiv", "https://github.com/sam/psiv"),
        ("theirs", "https://github.com/kim/burrow"),
        ("gitlab", "https://gitlab.com/sam/burrow.git"),
        ("lookalike", "https://github.com.example.net/sam/burrow"),
        ("suffix", "https://notgithub.com/sam/burrow"),
        ("mirror", "/srv/mirrors/sam/burrow.git"),
        ("longer", "https://github.com/sam/burrow/extra"),
        // Where a token can sit in an origin: what's read from one is a
        // name, or none.
        (
            "token",
            "https://x-access-token:SECRET@github.com/sam/psiv.git",
        ),
        ("query", "https://github.com/sam/psiv?token=SECRET"),
        ("inpath", "https://github.com/tok:SECRET@sam/psiv"),
        ("foreign", "https://sam:SECRET@gitlab.com/sam/psiv.git"),
    ] {
        t.clone(name, url);
    }
    fs::create_dir(t.path("plain")).unwrap();
    let repo =
        |d: &str, want: &str| assert_eq!(github_repo(&cfg, &t.at(d)), want, "github_repo {d}");
    for d in ["https", "slash", "user", "ssh", "port"] {
        repo(d, "sam/burrow");
    }
    repo("scp", "sam/box");
    repo("psiv", "sam/psiv");
    repo("token", "sam/psiv");
    for d in [
        "gitlab",
        "lookalike",
        "suffix",
        "mirror",
        "longer",
        "query",
        "inpath",
        "foreign",
        "plain",
        "missing",
    ] {
        repo(d, "");
    }
    // No path at all is no clone, not the one it's run in (the tests run
    // in this crate's checkout).
    assert_eq!(github_repo(&cfg, ""), "");
    for d in ["https", "ssh", "scp"] {
        assert!(pipeline_project(&cfg, &t.at(d)), "{d} isn't the pipeline's");
    }
    for d in ["psiv", "theirs", "gitlab", "mirror", "plain", "missing"] {
        assert!(!pipeline_project(&cfg, &t.at(d)), "{d} is the pipeline's");
    }

    // What each turn of a job queues as: pipeline for one run with
    // --pipeline in one of the pipeline's projects, first for one anywhere
    // else (asked each time, so a job from before the rule is held to it),
    // whatever the turn asks; otherwise the turn's own.
    let p = t.job("p", true, Some(&t.at("https")));
    let d = t.job("d", true, Some(&t.at("psiv")));
    let o = t.job("o", true, None);
    let n = t.job("n", false, Some(&t.at("https")));
    let tk = t.job("t", true, Some(&t.at("token")));
    let f = t.job("f", true, Some(&t.at("foreign")));
    cls(&cfg, &p, Class::Normal, Class::Pipeline);
    cls(&cfg, &p, Class::First, Class::Pipeline);
    cls(&cfg, &d, Class::Normal, Class::First);
    cls(&cfg, &o, Class::Normal, Class::First);
    cls(&cfg, &n, Class::First, Class::First);
    cls(&cfg, &n, Class::Normal, Class::Normal);

    // A waiting turn moves in line, and is admitted and recorded, as what
    // its job is now: one whose project stopped being the pipeline's while
    // it waited goes as first, and back.
    plc(&cfg, "2-17-p-1-x", &p, Class::Pipeline, "0-17-p-1-x");
    plc(&cfg, "0-17-d-1-x", &d, Class::First, "1-17-d-1-x");
    plc(&cfg, "1-17-p-1-x", &p, Class::Pipeline, "0-17-p-1-x");
    plc(&cfg, "0-17-p-1-x", &p, Class::Pipeline, "0-17-p-1-x");
    plc(&cfg, "2-17-n-1-x", &n, Class::Normal, "2-17-n-1-x");
    plc(&cfg, "1-17-n-1-x", &n, Class::First, "1-17-n-1-x");

    // And proj says so of the ones it demotes, by the repo the origin names
    // and never the origin itself, which can hold a token; and of no other.
    for j in [&tk, &f] {
        let got = demoted(&cfg, j).unwrap_or_default();
        assert!(
            !got.to_lowercase().contains("secret"),
            "demoted shows the origin's token: {got}"
        );
    }
    dem(&cfg, &p, "");
    dem(&cfg, &n, "");
    let not_listed = "asked as --pipeline, but its origin, github.com/sam/psiv, isn't in pipelineRepos: it goes as --first";
    dem(&cfg, &d, not_listed);
    dem(&cfg, &tk, not_listed);
    dem(
        &cfg,
        &f,
        "asked as --pipeline, but its origin isn't a GitHub repo's: it goes as --first",
    );
    dem(
        &cfg,
        &o,
        "asked as --pipeline, but its project has no origin: it goes as --first",
    );

    // A lead repo's pipeline jobs (leadRepos, among the pipeline's, any
    // case) are lead ones, in the same place in line as pipeline ones (pick
    // takes them first); its other jobs, and pipeline ones elsewhere, are as
    // they were.
    cfg.lead_repos = vec!["sam/BURROW".into()];
    let b = t.job("b", true, Some(&t.at("scp")));
    cls(&cfg, &p, Class::Normal, Class::Lead);
    cls(&cfg, &p, Class::First, Class::Lead);
    cls(&cfg, &b, Class::Normal, Class::Pipeline);
    cls(&cfg, &d, Class::Normal, Class::First);
    cls(&cfg, &n, Class::Normal, Class::Normal);
    plc(&cfg, "2-17-p-1-x", &p, Class::Lead, "0-17-p-1-x");
    plc(&cfg, "0-17-b-1-x", &b, Class::Pipeline, "0-17-b-1-x");
    dem(&cfg, &p, "");
}

// The machine's own flake is the pipeline's whatever its origin, and with no
// list at all: the update job's fixes there take the pipeline's slot with
// the others all taken and the flake's project at its cap. Any other
// project's --pipeline jobs, and the flake's plain ones, still aren't. It's
// the same directory however either side spells it: doubled and trailing
// slashes, dots, a symlink to it.
#[test]
fn the_flake() {
    let mut cfg = config();
    let t = Scratch::new();
    t.clone("homelab", "https://github.com/sam/homelab");
    t.clone("psiv", "https://github.com/sam/psiv");
    std::os::unix::fs::symlink("homelab", t.path("flake-link")).unwrap();
    let v = t.job("v", true, Some(&t.at("psiv")));
    cfg.pipeline_repos = vec![];
    cfg.lead_repos = vec![];
    let root = t.at("");
    let root = root.trim_end_matches('/');
    for flake in [
        "/homelab",
        "//homelab/",
        "/./homelab",
        "/jobs/../homelab",
        "/flake-link",
        "/flake-link/",
    ] {
        for proj in [
            "/homelab",
            "//homelab",
            "/homelab/",
            "/./homelab",
            "/flake-link",
        ] {
            fs::create_dir_all(t.path("jobs")).unwrap();
            let proj = format!("{root}{proj}");
            let u = t.job("u", true, Some(&proj));
            let w = t.job("w", false, Some(&proj));
            cfg.flake = format!("{root}{flake}").into();
            cls(&cfg, &u, Class::Normal, Class::Pipeline);
            cls(&cfg, &v, Class::Normal, Class::First);
            cls(&cfg, &w, Class::Normal, Class::Normal);
            dem(&cfg, &u, "");
            let c = |d: &Path| class(&cfg, d, Class::Normal).name();
            case(
                &cfg,
                "u 4",
                &[
                    &format!("normal {proj}"),
                    &format!("normal {proj}"),
                    &format!("normal {root}/x"),
                    "free",
                    &format!("w {} {proj}", c(&w)),
                    &format!("v {} {}", c(&v), t.at("psiv")),
                    &format!("u {} {proj}", c(&u)),
                ],
            );
        }
    }

    // Nothing that isn't there is the flake: not an empty path on either
    // side, nor one that doesn't exist, even spelled the same.
    let gone = t.at("gone");
    let homelab = t.at("homelab");
    for (flake, proj) in [
        (gone.as_str(), gone.as_str()),
        ("", ""),
        ("", homelab.as_str()),
        (homelab.as_str(), ""),
    ] {
        let e = t.job("e", true, Some(proj));
        cfg.flake = flake.into();
        cls(&cfg, &e, Class::Normal, Class::First);
    }
}
