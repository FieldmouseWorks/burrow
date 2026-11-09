//! Publish execution metadata before any engine (including a VM command) starts.
use super::Job;
use burrow_process::state_reads::Dir;
use std::io::{self, Write};

impl Job<'_> {
    /// There is no child until both records are written. A failed spawn rolls
    /// back only this attempt, preserving earlier invocations and their start.
    /// After a successful spawn there are no fallible recording operations.
    pub(crate) fn start_run<T>(
        &self,
        n: &str,
        at: i64,
        start: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let dir = Dir::open(&self.dir)?;
        let mut stream = dir.append_file(&format!("{n}.jsonl"))?;
        let from = stream.metadata()?.len();
        let name = format!("{n}.started");
        let mut created = false;
        let result = (|| {
            match dir.create_new(&name) {
                Ok(mut file) => {
                    created = true;
                    writeln!(file, "{at}")?;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // An existing observation must still be a confined regular file.
                    dir.file(std::path::Path::new(&name))?;
                }
                Err(e) => return Err(e),
            }
            writeln!(stream, "{{\"type\":\"burrow.run\",\"at\":{at}}}")?;
            start()
        })();
        if let Err(error) = result {
            let undo = stream
                .set_len(from)
                .and_then(|()| if created { dir.remove(&name) } else { Ok(()) });
            return Err(match undo {
                Ok(()) => error,
                Err(undo) => io::Error::other(format!("{error}; cannot undo failed start: {undo}")),
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, tests::example};
    use std::{
        fs,
        process::{Command, Stdio},
    };

    fn fixture() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        fs::create_dir_all(cfg.jobs.join("job")).unwrap();
        fs::write(cfg.jobs.join("job/1.jsonl"), "").unwrap();
        (tmp, cfg)
    }

    #[test]
    fn boundary_precedes_even_output_emitted_before_spawn_returns() {
        let (_tmp, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        for at in [100, 200] {
            let mut child = job
                .start_run("1", at, || {
                    // Enforce the ordering even if the child completes before its
                    // handle reaches the caller; no scheduler timing assumption.
                    let before = fs::read_to_string(job.turn("1", "jsonl"))?;
                    assert!(
                        before.ends_with(&format!("{{\"type\":\"burrow.run\",\"at\":{at}}}\n"))
                    );
                    let mut child = Command::new("sh")
                        .args(["-c", "printf '%s\\n' '{\"type\":\"result\"}'"])
                        .stdout(Stdio::from(
                            fs::OpenOptions::new()
                                .append(true)
                                .open(job.turn("1", "jsonl"))?,
                        ))
                        .spawn()?;
                    assert!(child.wait()?.success());
                    Ok(child)
                })
                .unwrap();
            assert!(child.wait().unwrap().success());
        }
        let lines = fs::read_to_string(job.turn("1", "jsonl")).unwrap();
        let kinds: Vec<_> = lines
            .lines()
            .map(|s| {
                serde_json::from_str::<serde_json::Value>(s).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(kinds, ["burrow.run", "result", "burrow.run", "result"]);
        assert_eq!(
            fs::read_to_string(job.turn("1", "started")).unwrap(),
            "100\n"
        );
    }

    #[test]
    fn failed_spawns_leave_no_execution_and_preserve_earlier_runs() {
        let (_tmp, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        let fail = || Command::new("/no-such-proj-engine").spawn();
        assert!(job.start_run("1", 100, fail).is_err());
        assert!(!job.turn("1", "started").exists());
        assert_eq!(fs::read_to_string(job.turn("1", "jsonl")).unwrap(), "");
        job.start_run("1", 200, || Ok(())).unwrap();
        let before = fs::read_to_string(job.turn("1", "jsonl")).unwrap();
        assert!(job.start_run("1", 300, fail).is_err());
        assert_eq!(fs::read_to_string(job.turn("1", "jsonl")).unwrap(), before);
        assert_eq!(
            fs::read_to_string(job.turn("1", "started")).unwrap(),
            "200\n"
        );
    }

    #[test]
    fn recording_failure_never_starts_a_child() {
        use std::{cell::Cell, os::unix::fs::symlink};
        let (_tmp, cfg) = fixture();
        let job = Job::new(&cfg, "job");
        let called = Cell::new(false);
        fs::remove_file(job.turn("1", "jsonl")).unwrap();
        symlink("/dev/null", job.turn("1", "jsonl")).unwrap();
        assert!(
            job.start_run("1", 100, || {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
        assert!(!job.turn("1", "started").exists());
        fs::remove_file(job.turn("1", "jsonl")).unwrap();
        fs::write(job.turn("1", "jsonl"), "").unwrap();
        symlink("/dev/null", job.turn("1", "started")).unwrap();
        assert!(
            job.start_run("1", 100, || {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
        assert_eq!(fs::read_to_string(job.turn("1", "jsonl")).unwrap(), "");
    }
}
