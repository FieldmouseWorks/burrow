//! Working through the queue: one look at a time per session, `parallel`
//! at a time per reviewer, the pipeline's own first.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::queue::{self, Item};
use crate::scrub::{head, said, scrub};
use crate::{Failed, Service, codex, now};

/// A look that resumes the PR's session: what it already read may be stale,
/// and its answer is still the verdict on the whole change.
const AGAIN: &str = "This continues your review of this PR. {since} The checkout is in the same place; anything you read there before may have changed, so read it again before you rely on it. Review the whole change as it stands at HEAD, not only what changed since your last look: your answer is the verdict on all of it, and the PR merges on it. Here is the brief again, with the replies your findings have had since.\n\n";

/// How often the queue is read again while looks are under way, for what
/// passes add meanwhile.
const AGAIN_EVERY: Duration = Duration::from_secs(5);

/// One queued look: its answer, or why there's none, left in done/ for the
/// repo's pass, which it kicks. Gone from the queue either way. One the
/// sweep took off the queue first (its PR moved on before it began) leaves
/// nothing to do.
pub fn review(s: &Service, name: &str) {
    let path = s.cfg.queue.join(name);
    let held = match queue::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            // One it can't open is answered, or it'd be handed out forever.
            let why = head(&format!("its queue entry can't be opened: {e}"), 300, &[]);
            s.say(&format!("{name}: review failed: {why}"));
            let _ = queue::save(
                &s.cfg.done.join(name),
                &json!({"result": "error", "error": why, "at": now()}),
            );
            let _ = queue::remove(&path);
            return;
        }
    };
    // Held while it runs: a sweep leaves a look that's under way, and its
    // snapshot, be (reviewers.py `taken`), and takes one that isn't under
    // its own lock, which this waits out.
    if held.lock_shared().is_err() || held.metadata().is_ok_and(|m| m.nlink() == 0) {
        return;
    }
    let raw = queue::load(&path);
    let item = match &raw {
        None => Err("its queue entry is unreadable".to_owned()),
        Some(v) => serde_json::from_value::<Item>(v.clone())
            .map_err(|e| format!("its queue entry is unreadable: {e}")),
    };
    let out = match &item {
        Err(e) => Err(Failed::Error(e.clone())),
        Ok(item) => look(s, item),
    };
    // Read once it's done: a refresh while it ran is scrubbed too.
    let keys = s.keys();
    let item = item.ok();
    let tag = |v: &Option<Value>, k: &str| {
        v.as_ref()
            .and_then(|v| v.get(k))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let pr = format!("{}#{}", fmt(&tag(&raw, "repo")), fmt(&tag(&raw, "number")));
    let mut back = match out {
        Ok(answer) => json!({"result": "review", "answer": scrub(&answer, &keys),
                             "description": item.as_ref().map(|i| i.description.clone())}),
        Err(Failed::Out { why, until }) => {
            let why = said(&why, &keys);
            let who = item
                .as_ref()
                .and_then(|i| s.cfg.reviewer(&i.reviewer))
                .map_or("its reviewer", |r| r.0);
            s.say(&format!("{pr}: {who} is out: {why}"));
            json!({"result": "out", "why": why, "until": until})
        }
        Err(Failed::Error(e)) => {
            let e = head(&e, 300, &keys);
            s.say(&format!("{pr}: review failed: {e}"));
            json!({"result": "error", "error": e})
        }
    };
    back["at"] = json!(now());
    // The head it read, for a PR that has moved on since (the steward's
    // `overtaken`).
    if raw.as_ref().is_some_and(Value::is_object) {
        back["head"] = tag(&raw, "headRefOid");
        back["base"] = tag(&raw, "baseRefName");
    }
    // Out of the queue even if the answer can't be kept: with nothing back
    // the steward counts it lost and queues it again.
    if let Err(e) = queue::save(&s.cfg.done.join(name), &back) {
        s.say(&format!("{name}: couldn't keep its answer: {e}"));
    }
    let _ = fs::remove_file(&path);
    if let Some(repo) = tag(&raw, "repo").as_str() {
        let url = format!("{}/{repo}", s.cfg.kick);
        if let Err(e) = burrow_events::kick::kick(&url, Duration::from_secs(5)) {
            s.say(&format!("couldn't kick {url}: {e}"));
        }
    }
}

/// A JSON value as the journal shows it: a string bare.
fn fmt(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
}

/// The look itself, in the PR's session: every look at it resumes the one
/// before's.
fn look(s: &Service, item: &Item) -> Result<Value, Failed> {
    let Some((who, _)) = s.cfg.reviewer(&item.reviewer) else {
        return Err(Failed::Error(format!(
            "there's no reviewer {} here",
            item.reviewer
        )));
    };
    if !on(s, &item.reviewer, &item.repo) {
        // It, or the repo, was taken off since the look was queued.
        return Err(Failed::Error(format!(
            "{} isn't one {who} reviews",
            item.repo
        )));
    }
    let snap = s.cfg.snapshots.join(&item.snap);
    if !snap.is_dir() {
        return Err(Failed::Error("its snapshot is gone".into()));
    }
    let session = queue::session_name(&item.reviewer, &item.snap)
        .map(|n| s.cfg.sessions.join(n))
        .ok_or_else(|| Failed::Error(format!("{} isn't a snapshot's name", item.snap)))?;
    let last = queue::load(&session.join("look.json")).unwrap_or(Value::Null);
    let (last_id, last_head) = (
        last["id"].as_str().filter(|x| !x.is_empty()),
        last["head"].as_str(),
    );
    let head = &item.head_ref_oid;
    let short = |h: &str| h.chars().take(12).collect::<String>();
    let prompt = match last_id {
        None => item.prompt.clone(),
        Some(_) => {
            let since = if last_head == Some(head.as_str()) {
                "The PR's head is the one you last looked at.".to_owned()
            } else {
                format!(
                    "Your last look was at {}, and the PR's head is now {}.",
                    short(last_head.unwrap_or("")),
                    short(head)
                )
            };
            AGAIN.replace("{since}", &since) + &item.prompt
        }
    };
    let pr = format!("{}#{}", item.repo, item.number);
    let seven: String = head.chars().take(7).collect();
    s.say(&format!(
        "{pr}: {who} is reviewing {seven}{}",
        if last_id.is_some() {
            " in its session"
        } else {
            ""
        }
    ));
    let began = now();
    let (answer, sid) = match codex::look(s, &snap, &prompt, &session.join("home"), last_id) {
        Ok(got) => got,
        Err(e) => {
            // A session that won't resume (lost, or broken) isn't asked
            // again: the look's next try starts the PR's next one.
            if matches!(e, Failed::Error(_)) && last_id.is_some() {
                let _ = fs::remove_file(session.join("look.json"));
            }
            return Err(e);
        }
    };
    queue::save(
        &session.join("look.json"),
        &json!({"id": sid, "head": head}),
    )
    .map_err(|e| Failed::Error(format!("couldn't keep its session: {e}")))?;
    let took = now() - began;
    s.say(&format!(
        "{pr}: {who} took {took:.0}s{}",
        if last_id.is_some() {
            " in its session"
        } else {
            ", starting its session"
        }
    ));
    Ok(answer)
}

/// Whether the repo has this reviewer: it's on, and the repo isn't one it
/// leaves alone (reviewers.py `on`).
fn on(s: &Service, reviewer: &str, repo: &str) -> bool {
    s.cfg
        .reviewer(reviewer)
        .is_some_and(|(_, r)| r.enable && !r.off.iter().any(|x| x == repo))
}

/// The sessions of PRs that closed, which the steward's sweep marked gone:
/// deleted for good, nothing of them kept.
pub fn forget(s: &Service) {
    let entries = match fs::read_dir(&s.cfg.sessions) {
        Ok(d) => d,
        Err(e) => return s.say(&format!("couldn't read the sessions: {e}")),
    };
    for x in entries.flatten() {
        let name = x.file_name();
        if name.to_string_lossy().starts_with(queue::GONE)
            && let Err(e) = remove_all(&x.path())
        {
            s.say(&format!(
                "couldn't delete the closed PR's session {}: {e}",
                name.to_string_lossy()
            ));
        }
    }
}

/// path and everything under it. The agents set their own modes on what
/// they write: a directory they left read-only is opened up to empty it.
fn remove_all(path: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() {
        return fs::remove_file(path);
    }
    if m.permissions().mode() & 0o700 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    for x in fs::read_dir(path)? {
        remove_all(&x?.path())?;
    }
    fs::remove_dir(path)
}

/// Every queued look, `parallel` at a time for each reviewer, until the
/// queue is empty (passes may add to it meanwhile). A free place goes to
/// the lead repos' looks first (`leadRepos`), then to the rest of the
/// pipeline's own (`pipelineRepos`), then to the oldest PR's: by when it
/// was opened, not when its head came, so a PR pushed again keeps its
/// place. One look at a time in a session: a PR's next head waits for the
/// look at its last one, and resumes it.
pub fn work(s: Arc<Service>) {
    forget(&s);
    let _watch = s.logins.watch();
    let (done_tx, done) = mpsc::channel::<String>();
    // Only what's in hand: a name done with and queued again (a retry, a
    // second look) is taken again.
    let mut busy: HashSet<String> = HashSet::new();
    loop {
        while let Ok(name) = done.try_recv() {
            busy.remove(&name);
        }
        let mut waiting: BTreeMap<String, Vec<(u8, f64, f64, String)>> = BTreeMap::new();
        let names = match fs::read_dir(&s.cfg.queue) {
            Ok(d) => d
                .flatten()
                .map(|x| x.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            Err(e) => {
                s.say(&format!("couldn't read the queue: {e}"));
                Vec::new()
            }
        };
        for name in names {
            if !name.ends_with(".json") || busy.contains(&name) {
                continue;
            }
            let r = queue::reviewer_of(&name);
            if s.cfg.reviewer(r).is_none() {
                // A reviewer this machine no longer has (one removed since
                // it was queued): nothing would ever take it, and the queue
                // watcher would start this again and again until systemd
                // gave up on it, and on every other review with it. Taken
                // off unanswered, unless a worker from before is still on it.
                if queue::taken(&s.cfg.queue.join(&name)) {
                    s.say(&format!(
                        "{name}: dropped from the queue: there's no reviewer {r} here"
                    ));
                }
                continue;
            }
            let item = queue::load(&s.cfg.queue.join(&name)).unwrap_or(Value::Null);
            let num = |k: &str| item[k].as_f64().unwrap_or(0.0);
            let rank = s.cfg.rank(item["repo"].as_str());
            waiting.entry(r.to_owned()).or_default().push((
                rank,
                num("opened"),
                num("at"),
                name.clone(),
            ));
        }
        let mut sessions: HashSet<String> = busy.iter().map(|n| queue::session_of(n)).collect();
        for (r, mut names) in waiting {
            let parallel = s.cfg.reviewer(&r).map_or(0, |(_, rv)| rv.parallel);
            let mut free =
                parallel.saturating_sub(busy.iter().filter(|n| queue::reviewer_of(n) == r).count());
            names.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for (.., name) in names {
                if free == 0 {
                    break;
                }
                if sessions.insert(queue::session_of(&name)) {
                    let (s, tx, n) = (s.clone(), done_tx.clone(), name.clone());
                    thread::spawn(move || {
                        review(&s, &n);
                        let _ = tx.send(n);
                    });
                    busy.insert(name);
                    free -= 1;
                }
            }
        }
        if busy.is_empty() {
            break;
        }
        // Until one ends, or the queue has had time to grow.
        if let Ok(name) = done.recv_timeout(AGAIN_EVERY) {
            busy.remove(&name);
        }
    }
    forget(&s);
}
