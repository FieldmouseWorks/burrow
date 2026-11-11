// proj's choice of who takes a slot, on what a waiting turn would see:
// pipeline jobs' slots stay theirs, one project's jobs stop at its cap while
// the others go by, pipeline ones count against no cap, and lead ones go
// ahead of everyone but the slot owed to the others, every free slot going in
// one look. Which jobs are pipeline ones is in [`repos`].

use super::{Class, Slot, Waiter, pick};
use crate::config::Config;
use crate::config::tests::example;

mod repos;

fn config() -> Config {
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.tools.git = "git".into();
    cfg.pipeline_repos = vec!["Sam/Burrow".into(), "sam/box".into()];
    cfg.lead_repos = vec![];
    cfg.flake = "/nowhere".into();
    cfg
}

/// The slots, then the line: "free", or a holder's "CLASS PROJECT", for
/// slots 1 to maxJobs in order; then "NAME CLASS PROJECT" for each waiting
/// turn. What's picked is a line "NAME N" for each turn that goes, in the
/// order they're chosen, or "" for nobody.
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
    let got: Vec<String> = pick(cfg, &slots, &line)
        .into_iter()
        .map(|(i, n)| format!("{} {n}", line[i].name))
        .collect();
    let got = got.join("\n");
    assert_eq!(got, want, "from {specs:?}");
}

// A backlog of pipeline work doesn't starve the others: with none of theirs
// running, a pipeline or lead turn leaves them the last free ordinary slot.
#[test]
fn a_backlog_leaves_the_others_a_slot() {
    let mut cfg = config();
    case(
        &cfg,
        "a 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "p pipeline /src/remi",
            "l lead /src/burrow",
            "a first /src/web",
        ],
    );
    case(
        &cfg,
        "a 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "l lead /src/burrow",
            "a normal /src/web",
        ],
    );
    // It takes the pipeline's own slot instead, where one's free.
    case(
        &cfg,
        "p 4\na 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "free",
            "p pipeline /src/remi",
            "a first /src/web",
        ],
    );
    // With more free than that it takes the lowest as ever, and the others
    // still have theirs next.
    case(
        &cfg,
        "l 2\na 3",
        &[
            "pipeline /src/remi",
            "free",
            "free",
            "lead /src/burrow",
            "l lead /src/burrow",
            "a first /src/web",
        ],
    );
    // Both rules at once: a lead turn can't have the slot kept for the others,
    // nor the pipeline's own while a pipeline turn waits, so that turn takes
    // it; and with none waiting, the lead turn takes it.
    case(
        &cfg,
        "p 4\na 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "free",
            "l lead /src/burrow",
            "p pipeline /src/remi",
            "a first /src/web",
        ],
    );
    case(
        &cfg,
        "l 4\na 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "free",
            "l lead /src/burrow",
            "a first /src/web",
        ],
    );
    // Nobody of theirs waiting: nothing's kept, and the pipeline takes the last.
    case(
        &cfg,
        "p 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "p pipeline /src/remi",
        ],
    );
    // One of theirs already running: that's their share, and the pipeline
    // goes first.
    case(
        &cfg,
        "p 3",
        &[
            "normal /src/x",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "p pipeline /src/remi",
            "a first /src/web",
        ],
    );
    // A holder proj can't name is someone else's here too.
    case(
        &cfg,
        "p 3",
        &[
            "- -",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "p pipeline /src/remi",
            "a first /src/web",
        ],
    );
    // A turn its project's cap holds back can't use what's kept, so nothing is.
    cfg.max_jobs_per_project = 0;
    case(
        &cfg,
        "p 3",
        &[
            "pipeline /src/remi",
            "lead /src/burrow",
            "free",
            "lead /src/burrow",
            "a first /src/web",
            "p pipeline /src/remi",
        ],
    );
    // In one look too, whoever is ahead in the line: pipeline turns stop at
    // what the slot owed leaves them, the pipeline's own included, and the
    // others go.
    cfg.max_jobs_per_project = 2;
    cfg.max_jobs = 6;
    case(
        &cfg,
        "p 1\nq 6\na 2",
        &[
            "free",
            "free",
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "free",
            "p pipeline /src/remi",
            "q pipeline /src/remi",
            "r pipeline /src/remi",
            "a normal /src/web",
        ],
    );
    case(
        &cfg,
        "l 1\na 2",
        &[
            "free",
            "free",
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "pipeline /src/remi",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "a normal /src/web",
        ],
    );
}

#[test]
fn picks() {
    let mut cfg = config();
    // The head of the line takes the lowest free slot.
    case(
        &cfg,
        "a 1\nb 2",
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
    // The last slot is the pipeline's: nobody else takes it, and a pipeline turn
    // behind them does.
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
    // A project holding PER_PROJECT slots waits, and lets the others by.
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
            "a steward /src/remi",
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
    // Projects are told apart whole, spaces and all; a turn whose project can't be
    // read is in none.
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
    // A lead turn goes first, from anywhere in the line: ahead of pipeline turns
    // that came before it, and of everyone else.
    case(
        &cfg,
        "l 1\np 2\na 3",
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
        "l 3\np 4",
        &[
            "normal /src/x",
            "normal /src/y",
            "free",
            "free",
            "p pipeline /src/remi",
            "l lead /src/burrow",
        ],
    );
    // But the pipeline's own slot stays the other pipeline turns' while one waits,
    // so a lead repo's backlog can't stop the work on what deploys it: the lead
    // turn waits for the next slot.
    case(
        &cfg,
        "p 4",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "free",
            "p pipeline /src/remi",
            "l lead /src/burrow",
        ],
    );
    case(
        &cfg,
        "p 4",
        &[
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "free",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "p pipeline /src/remi",
        ],
    );
    // With none of those in line it takes the pipeline's slot as a pipeline turn
    // does, counts against no cap, and holds none of its project's.
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
            "a steward /src/burrow",
        ],
    );
    // With no slot free nobody goes, lead or not; of two lead turns, the first in
    // line goes first.
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
        "l 2\nm 3\np 4",
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
    case(
        &cfg,
        "l 4",
        &[
            "lead /src/burrow",
            "normal /src/x",
            "normal /src/y",
            "free",
            "l lead /src/burrow",
            "m lead /src/burrow",
        ],
    );

    // Every free slot goes in one look, in line order, under the same rules. Lead
    // first, then the line; a pipeline turn gives up a slot anyone may take when a
    // turn behind it needs that one.
    cfg.max_jobs = 6;
    case(
        &cfg,
        "l 1\np 2\nq 6\na 3",
        &[
            "free",
            "free",
            "free",
            "lead /src/burrow",
            "lead /src/burrow",
            "free",
            "l lead /src/burrow",
            "p pipeline /src/remi",
            "q pipeline /src/remi",
            "a normal /src/web",
        ],
    );
    // Slots ahead of what the line can fill stay free, the pipeline's above all.
    case(
        &cfg,
        "a 1\np 2",
        &[
            "free",
            "free",
            "free",
            "free",
            "free",
            "free",
            "a normal /src/web",
            "p pipeline /src/remi",
        ],
    );
    // More waiting than free: the head of the line goes, and those behind wait.
    case(
        &cfg,
        "a 1\nb 2",
        &[
            "free",
            "free",
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "pipeline /src/b",
            "a normal /src/web",
            "b normal /src/lok",
            "c normal /src/psiv",
        ],
    );
    // Nobody but pipeline turns takes the pipeline's slot, however long the line.
    case(
        &cfg,
        "a 5",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "first /src/z",
            "free",
            "free",
            "a normal /src/web",
            "b normal /src/lok",
        ],
    );
    // A pipeline turn ahead of them takes it when that lets one of them go too.
    case(
        &cfg,
        "p 6\na 5",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "first /src/z",
            "free",
            "free",
            "p pipeline /src/remi",
            "a normal /src/web",
            "b normal /src/lok",
        ],
    );
    // PER_PROJECT counts the slots this look hands out: a project stops at its cap
    // and those behind it go by.
    cfg.max_jobs_per_project = 3;
    case(
        &cfg,
        "a 1\nb 2\nd 3",
        &[
            "free",
            "free",
            "free",
            "free",
            "free",
            "free",
            "a normal /src/web",
            "b first /src/web",
            "c normal /src/web",
            "d normal /src/lok",
        ],
    );
    case(
        &cfg,
        "a 1\nd 2",
        &[
            "free",
            "free",
            "normal /src/web",
            "free",
            "free",
            "free",
            "a normal /src/web",
            "c normal /src/web",
            "d normal /src/lok",
        ],
    );
    cfg.max_jobs_per_project = 2;
    // The pipeline's slot stays the other pipeline turns' whoever is ahead of them:
    // a turn behind the lead one gets no slot that would push it onto theirs.
    cfg.max_jobs = 6;
    case(
        &cfg,
        "l 3\np 6",
        &[
            "normal /src/x",
            "normal /src/y",
            "free",
            "normal /src/z",
            "first /src/z",
            "free",
            "l lead /src/burrow",
            "a normal /src/web",
            "p pipeline /src/remi",
        ],
    );
    // With two of them and one such turn, a lead one has the other in the same look.
    cfg.pipeline_slots = 2;
    case(
        &cfg,
        "l 5\np 6",
        &[
            "normal /src/x",
            "normal /src/y",
            "normal /src/z",
            "first /src/z",
            "free",
            "free",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "p pipeline /src/remi",
        ],
    );
    cfg.pipeline_slots = 1;
    // And one of the slots anyone may take stays everyone else's while one of
    // theirs waits and none runs, so a lead repo's backlog can't stop all the
    // rest: here the turn behind four lead ones takes the last one.
    case(
        &cfg,
        "l 1\nm 2\nn 3\nk 4\na 5",
        &[
            "free",
            "free",
            "free",
            "free",
            "free",
            "pipeline /src/remi",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "n lead /src/burrow",
            "k lead /src/burrow",
            "j lead /src/burrow",
            "a normal /src/web",
        ],
    );
    case(
        &cfg,
        "a 5",
        &[
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "free",
            "pipeline /src/remi",
            "l lead /src/burrow",
            "a first /src/web",
        ],
    );
    // With one of theirs running, none of theirs waiting, or none of those slots
    // free, the lead ones take every slot they're let.
    case(
        &cfg,
        "l 2\nm 3\nn 4\nk 5",
        &[
            "normal /src/web",
            "free",
            "free",
            "free",
            "free",
            "pipeline /src/remi",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "n lead /src/burrow",
            "k lead /src/burrow",
            "a normal /src/lok",
        ],
    );
    case(
        &cfg,
        "l 1\nm 2\nn 3\nk 4\nj 5",
        &[
            "free",
            "free",
            "free",
            "free",
            "free",
            "pipeline /src/remi",
            "l lead /src/burrow",
            "m lead /src/burrow",
            "n lead /src/burrow",
            "k lead /src/burrow",
            "j lead /src/burrow",
        ],
    );
    case(
        &cfg,
        "l 6",
        &[
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "lead /src/burrow",
            "free",
            "l lead /src/burrow",
            "a normal /src/web",
        ],
    );
}

// The last of a project's share stays free for its steward turns, whatever else
// is in line and however a turn got to first (run --first, or by waiting).
#[test]
fn the_last_of_a_share_is_for_steward_turns() {
    let mut cfg = config();
    cfg.max_jobs = 6;
    cfg.max_jobs_per_project = 4;
    let n = "normal /src/psiv";
    let s = "steward /src/psiv";
    let p = "pipeline /src/remi";
    let (a, st) = ("a normal /src/psiv", "s steward /src/psiv");
    case(&cfg, "", &[n, n, n, "free", "free", p, a]);
    case(
        &cfg,
        "",
        &[
            "first /src/psiv",
            n,
            n,
            "free",
            "free",
            p,
            "a first /src/psiv",
        ],
    );
    case(&cfg, "s 4", &[n, n, n, "free", "free", p, a, st]);
    // One look holds its own choices to it too, and another project goes by.
    case(
        &cfg,
        "a 1\nb 2\nc 3\ns 4\no 5",
        &[
            "free",
            "free",
            "free",
            "free",
            "free",
            p,
            a,
            "b normal /src/psiv",
            "c normal /src/psiv",
            "d normal /src/psiv",
            st,
            "o normal /src/lok",
        ],
    );
    // Steward turns may use the whole share, and stop there.
    case(&cfg, "s 4", &[s, s, s, "free", "free", p, st]);
    case(&cfg, "s 5", &[n, n, n, p, "free", p, st]);
    case(&cfg, "", &[n, n, n, s, "free", p, st]);
    // Steward turns held leave the others the rest of the share.
    case(&cfg, "a 4", &[s, n, n, "free", "free", p, a]);
    // A share of one still goes to a normal turn with no steward turn in line,
    // and to a steward turn ahead of it when there is one.
    cfg.max_jobs_per_project = 1;
    case(&cfg, "a 1", &["free", "free", "free", "free", "free", p, a]);
    case(
        &cfg,
        "s 1",
        &["free", "free", "free", "free", "free", p, st, a],
    );
    case(&cfg, "", &[n, "free", "free", "free", "free", p, a]);
    // A holder that can't be told counts as one of the others.
    cfg.max_jobs_per_project = 2;
    case(
        &cfg,
        "",
        &["- /src/psiv", "free", "free", "free", "free", p, a],
    );
}
