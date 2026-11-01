//! What a pass keeps, in plain files in the config's `state`:
//!
//! - `seen`: a key a line, one for each finding handed to a job or recorded
//!   by its source's first pass. A key in it is never reported again.
//! - `baseline-SOURCE`: there once SOURCE's first pass has recorded what it
//!   found (kev, vulnix, or osv-PROJECT for each project).
//! - `pending.json`: what's been found and not yet handed to a job, in the
//!   order it was found. Each pass hands it over again until it's taken.
//! - `log`: what it found and each hand-over, dated.
//! - `vulnix-day`: the day vulnix last ran.
//! - `lock`: held by the pass that's running.
//!
//! All but `pending.json` and `lock` are as the shell version kept them, so
//! a machine's state carries on from it.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::Finding;

/// Lines of the log a report shows.
const REPORTED: usize = 50;

pub struct State {
    dir: PathBuf,
    seen: HashSet<String>,
    pending: Vec<Finding>,
}

fn io(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

impl State {
    /// The state in `dir`, made if it isn't there yet.
    pub fn open(dir: &Path) -> Result<State, String> {
        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        State::read(dir)
    }

    /// The state in `dir` as it is, none if there's none yet.
    pub fn read(dir: &Path) -> Result<State, String> {
        let path = dir.join("seen");
        let seen: HashSet<String> = match fs::read_to_string(&path) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            Err(e) if e.kind() == ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(io(&path, e)),
        };
        let path = dir.join("pending.json");
        let pending: Vec<Finding> = match fs::read(&path) {
            Ok(text) => {
                serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io(&path, e)),
        };
        // What a pass recorded as handed over before it could take it off
        // the list.
        let pending = pending
            .into_iter()
            .filter(|f| !seen.contains(&f.key))
            .collect();
        Ok(State {
            dir: dir.to_owned(),
            seen,
            pending,
        })
    }

    /// What waits for a job, in the order it was found.
    pub fn pending(&self) -> &[Finding] {
        &self.pending
    }

    /// Takes in what `source` found. What's seen, or waiting already, is
    /// left out. The source's first pass records the rest as seen; any later
    /// pass has it wait for a job, and returns it.
    pub fn offer(&mut self, source: &str, found: Vec<Finding>) -> Result<Vec<Finding>, String> {
        let quiet = !self.dir.join(format!("baseline-{source}")).exists();
        let mut new: Vec<Finding> = Vec::new();
        let mut recorded = Vec::new();
        for f in found {
            if self.seen.contains(&f.key)
                || recorded.contains(&f.key)
                || self.pending.iter().any(|p| p.key == f.key)
                || new.iter().any(|p| p.key == f.key)
            {
                continue;
            }
            if quiet {
                recorded.push(f.key);
            } else {
                new.push(f);
            }
        }
        self.record(&recorded)?;
        if !new.is_empty() {
            self.pending.extend(new.iter().cloned());
            self.save()?;
        }
        Ok(new)
    }

    /// Marks `source`'s first pass done.
    pub fn baselined(&self, source: &str) -> Result<(), String> {
        let path = self.dir.join(format!("baseline-{source}"));
        File::create(&path).map(drop).map_err(|e| io(&path, e))
    }

    /// Whether vulnix hasn't run yet on `today`.
    pub fn vulnix_due(&self, today: &str) -> bool {
        fs::read_to_string(self.dir.join("vulnix-day"))
            .map_or(true, |day| day.trim_end_matches('\n') != today)
    }

    pub fn vulnix_ran(&self, today: &str) -> Result<(), String> {
        let path = self.dir.join("vulnix-day");
        fs::write(&path, format!("{today}\n")).map_err(|e| io(&path, e))
    }

    /// `project`'s job has what waited for it: seen, and waiting no more.
    pub fn handed(&mut self, project: &str) -> Result<(), String> {
        let keys: Vec<String> = self
            .pending
            .iter()
            .filter(|f| f.project == project)
            .map(|f| f.key.clone())
            .collect();
        self.record(&keys)?;
        self.dropped(project)
    }

    /// What waited for `project`'s job waits no more, and isn't seen: found
    /// again, it's new again.
    pub fn dropped(&mut self, project: &str) -> Result<(), String> {
        self.pending.retain(|f| f.project != project);
        self.save()
    }

    fn record(&mut self, keys: &[String]) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        let path = self.dir.join("seen");
        let text: String = keys.iter().map(|k| format!("{k}\n")).collect();
        append(&path, &text)?;
        self.seen.extend(keys.iter().cloned());
        Ok(())
    }

    /// Writes what's waiting whole, so a pass that stops halfway leaves the
    /// list as it was or as it is, never half of it.
    fn save(&self) -> Result<(), String> {
        let path = self.dir.join("pending.json");
        let new = self.dir.join("pending.json.new");
        let text = serde_json::to_vec_pretty(&self.pending).map_err(|e| e.to_string())?;
        fs::write(&new, text).map_err(|e| io(&new, e))?;
        fs::rename(&new, &path).map_err(|e| io(&path, e))
    }

    /// Adds the lines to the log.
    pub fn log(&self, lines: &[String]) -> Result<(), String> {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        append(&self.dir.join("log"), &text)
    }
}

fn append(path: &Path, text: &str) -> Result<(), String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| io(path, e))
}

/// The pass's lock, in `dir` (made if it isn't there yet), held until it's
/// dropped; none while another pass holds it.
pub fn lock(dir: &Path) -> Result<Option<File>, String> {
    use rustix::fs::{FlockOperation, flock};
    fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io(&path, e))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(e) => Err(io(&path, e.into())),
    }
}

/// The last of the log, and what waits for a job.
pub fn report(dir: &Path) -> Result<String, String> {
    let path = dir.join("log");
    let mut out = match fs::read_to_string(&path) {
        Ok(log) => {
            let lines: Vec<&str> = log.lines().collect();
            let from = lines.len().saturating_sub(REPORTED);
            lines[from..].iter().map(|l| format!("{l}\n")).collect()
        }
        Err(e) if e.kind() == ErrorKind::NotFound => "nothing seen yet\n".to_owned(),
        Err(e) => return Err(io(&path, e)),
    };
    let state = State::read(dir)?;
    if !state.pending.is_empty() {
        out.push_str("\nNot handed to a job yet (each pass tries again):\n");
        for f in &state.pending {
            out.push_str(&format!("- {}: {}\n", f.project, f.text));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(keys: &[&str]) -> Vec<Finding> {
        keys.iter()
            .map(|k| Finding::new("box", k, &format!("{k} text")))
            .collect()
    }

    fn keys(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|f| f.key.as_str()).collect()
    }

    #[test]
    fn a_first_pass_records_and_a_later_one_waits_for_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::open(dir.path()).unwrap();
        assert!(st.offer("kev", found(&["a", "b", "a"])).unwrap().is_empty());
        st.baselined("kev").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("seen")).unwrap(),
            "a\nb\n"
        );
        let new = st.offer("kev", found(&["a", "c", "c", "d"])).unwrap();
        assert_eq!(keys(&new), ["c", "d"]);
        // Another source's first pass is its own.
        assert!(st.offer("vulnix", found(&["e", "c"])).unwrap().is_empty());
        let mut again = State::read(dir.path()).unwrap();
        assert_eq!(keys(again.pending()), ["c", "d"]);
        assert!(again.offer("kev", found(&["c", "e", "f"])).unwrap().len() == 1);
        assert_eq!(keys(again.pending()), ["c", "d", "f"]);
    }

    #[test]
    fn what_was_handed_over_is_seen_and_what_was_dropped_isnt() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::open(dir.path()).unwrap();
        st.baselined("osv-app").unwrap();
        let mut f = found(&["a", "b"]);
        f.push(Finding::new("app", "c", "c text"));
        f.push(Finding::new("gone", "d", "d text"));
        st.offer("osv-app", f).unwrap();
        st.handed("box").unwrap();
        st.dropped("gone").unwrap();
        let mut again = State::read(dir.path()).unwrap();
        assert_eq!(keys(again.pending()), ["c"]);
        let new = again
            .offer(
                "osv-app",
                vec![Finding::new("box", "a", ""), Finding::new("gone", "d", "")],
            )
            .unwrap();
        assert_eq!(keys(&new), ["d"]);
    }

    #[test]
    fn a_hand_over_recorded_but_not_taken_off_the_list_is_off_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::open(dir.path()).unwrap();
        st.baselined("kev").unwrap();
        st.offer("kev", found(&["a", "b"])).unwrap();
        st.record(&["a".to_owned()]).unwrap();
        assert_eq!(keys(State::read(dir.path()).unwrap().pending()), ["b"]);
    }

    #[test]
    fn vulnix_runs_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let st = State::open(dir.path()).unwrap();
        assert!(st.vulnix_due("2026-11-01"));
        st.vulnix_ran("2026-11-01").unwrap();
        assert!(!st.vulnix_due("2026-11-01"));
        assert!(st.vulnix_due("2026-11-02"));
        // As the shell version wrote it, and with no newline.
        fs::write(dir.path().join("vulnix-day"), "2026-11-02").unwrap();
        assert!(!st.vulnix_due("2026-11-02"));
    }

    #[test]
    fn a_report_is_the_end_of_the_log_and_what_waits() {
        let dir = tempfile::tempdir().unwrap();
        let none = dir.path().join("none");
        assert_eq!(report(&none).unwrap(), "nothing seen yet\n");
        assert!(!none.exists());
        let mut st = State::open(dir.path()).unwrap();
        let lines: Vec<String> = (1..=60).map(|n| format!("line {n}")).collect();
        st.log(&lines).unwrap();
        let out = report(dir.path()).unwrap();
        assert!(
            out.starts_with("line 11\n") && out.ends_with("line 60\n"),
            "{out}"
        );
        st.baselined("kev").unwrap();
        st.offer("kev", found(&["a"])).unwrap();
        let out = report(dir.path()).unwrap();
        assert!(
            out.ends_with(
                "line 60\n\nNot handed to a job yet (each pass tries again):\n- box: a text\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn one_pass_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap();
        assert!(held.is_some());
        assert!(lock(dir.path()).unwrap().is_none());
        drop(held);
        assert!(lock(dir.path()).unwrap().is_some());
    }
}
