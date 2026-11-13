//! What the starter authorized a job to run as: its project, pinned engine,
//! role, GitHub App, sandbox and ownership. The record lives in the job's
//! host control directory, which no sandbox reaches, and is the only
//! authority for every turn and every engine run within one. The job
//! directory's files of the same names are its published copy, for readers
//! such as the probe and `proj status`: a job may write those, and nothing
//! proj runs it as follows them.
use super::Job;
use crate::{engine::Engine, read};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Write `bytes` to a staging file created exclusively beside `path`: a name
/// that exists already (another writer's, or an orphan a crash left, which
/// may alias the live record) is never opened, and a fresh one is tried.
fn stage(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    use io::Write;
    static N: AtomicU64 = AtomicU64::new(0);
    loop {
        let staged = path.with_file_name(format!(
            "{}.new.{}.{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes) {
                    let _ = fs::remove_file(&staged);
                    return Err(e);
                }
                return Ok(staged);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub project: String,
    pub engine: Option<Engine>,
    pub role: Option<String>,
    pub app: Option<String>,
    pub agent: Option<String>,
    pub owned: bool,
}

impl Request {
    fn path(job: &Job) -> PathBuf {
        crate::ask::host_path(job, "request")
    }

    /// The job's request. A job started before the record existed adopts
    /// what was last captured for it on the host (a retained retry's
    /// identity), else its published files, once: from then on the record
    /// holds, as `policy`'s admission does.
    pub fn load(job: &Job) -> Result<Self, String> {
        match crate::state_reads::bytes(&Self::path(job)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let request = match crate::retry::Identity::legacy_files(job)? {
                    Some(files) => Self::from(|key| files.get(key).cloned().flatten())?,
                    None => Self::from(|key| read(&job.path(key)))?,
                };
                request.adopt(job)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The record as the starter (or an earlier adoption) wrote it, if there
    /// is one: never adopted here from what the job wrote of itself.
    pub fn recorded(job: &Job) -> Option<Self> {
        let bytes = crate::state_reads::bytes(&Self::path(job)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Initialize the record if no one has, and return the winner: a
    /// reader holding stale values never overwrites a record, or an
    /// enrollment's update, that landed after it looked.
    fn adopt(self, job: &Job) -> Result<Self, String> {
        crate::ask::ensure(job)?;
        let path = Self::path(job);
        let staged = stage(
            &path,
            serde_json::to_string(&self)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let linked = fs::hard_link(&staged, &path);
        let _ = fs::remove_file(&staged);
        match linked {
            Ok(()) => {
                self.publish(job).map_err(|e| e.to_string())?;
                Ok(self)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let bytes = crate::state_reads::bytes(&path).map_err(|e| e.to_string())?;
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn from(read: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let named = |key| read(key).filter(|s: &String| !s.is_empty());
        Ok(Self {
            project: read("project").unwrap_or_default(),
            engine: named("engine").map(|e| Engine::parse(&e)).transpose()?,
            role: named("role"),
            app: named("app"),
            agent: named("agent"),
            owned: read("owned").is_some(),
        })
    }

    /// Only the starter and host-side changes it authorizes (an inbox
    /// label's role) call this; it publishes the copy too.
    pub fn save(&self, job: &Job) -> Result<(), String> {
        crate::ask::ensure(job)?;
        let path = Self::path(job);
        let staged = stage(
            &path,
            serde_json::to_string(self)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&staged, &path).map_err(|e| e.to_string())?;
        self.publish(job).map_err(|e| e.to_string())
    }

    /// The machine's label maps the job to `role`: the role's own engine,
    /// not the pin its old label gave it.
    pub fn enroll(job: &Job, role: String) -> Result<(), String> {
        let mut request = Self::load(job)?;
        if request.role.as_deref() == Some(role.as_str()) && request.engine.is_none() {
            return Ok(());
        }
        request.role = Some(role);
        request.engine = None;
        request.save(job)
    }

    /// The record as an earlier proj captured a job's files: the values its
    /// `read` returned, so an older reader restores exactly what this says.
    pub fn captured_files(&self) -> BTreeMap<String, Option<String>> {
        let some = |v: &Option<String>| v.clone();
        BTreeMap::from([
            (
                "project".to_owned(),
                (!self.project.is_empty()).then(|| self.project.clone()),
            ),
            (
                "engine".to_owned(),
                self.engine.map(|e| e.name().to_owned()),
            ),
            ("role".to_owned(), some(&self.role)),
            ("app".to_owned(), some(&self.app)),
            ("agent".to_owned(), some(&self.agent)),
            ("owned".to_owned(), self.owned.then(String::new)),
        ])
    }

    /// The request a turn runs as, its published copy put back first so
    /// nothing reads what the job wrote of itself since.
    pub fn restore(job: &Job) -> Result<Self, String> {
        let request =
            Self::load(job).map_err(|e| format!("couldn't read the job's request: {e}"))?;
        request
            .publish(job)
            .map_err(|e| format!("couldn't publish the job's request: {e}"))?;
        Ok(request)
    }

    /// The published copy, put back as the record has it.
    pub fn publish(&self, job: &Job) -> io::Result<()> {
        let files: BTreeMap<&str, Option<String>> = BTreeMap::from([
            (
                "project",
                (!self.project.is_empty()).then(|| format!("{}\n", self.project)),
            ),
            ("engine", self.engine.map(|e| e.name().to_owned())),
            ("role", self.role.as_ref().map(|r| format!("{r}\n"))),
            ("app", self.app.as_ref().map(|a| format!("{a}\n"))),
            ("agent", self.agent.as_ref().map(|a| format!("{a}\n"))),
            ("owned", self.owned.then(String::new)),
        ]);
        for (key, value) in files {
            let path = job.path(key);
            if read(&path).as_deref() == value.as_deref().map(|v| v.trim_end_matches('\n')) {
                continue;
            }
            match value {
                Some(value) => crate::state_reads::write_public(&path, value)?,
                None => match crate::state_reads::remove(&path) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, retry::Identity};

    fn rig() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        (tmp, serde_json::from_value(v).unwrap())
    }

    fn legacy_capture(job: &Job, n: &str, project: &str) {
        crate::ask::ensure(job).unwrap();
        fs::write(
            job.turn(n, "execution"),
            serde_json::json!({
                "choice": crate::engine::Choice::default(),
                "files": {"project": project, "owned": ""},
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_legacy_capture_survives_the_turn_advancing_or_being_rewritten() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "legacy");
        fs::create_dir(&job.dir).unwrap();
        legacy_capture(&job, "1", "trusted");
        // The job rewrote its files and the turn moved on before any read.
        fs::write(job.path("project"), "evil\n").unwrap();
        job.set_turns(2).unwrap();
        let request = Request::load(&job).unwrap();
        assert_eq!(request.project, "trusted");
        assert!(request.owned);
        assert_eq!(Request::load(&job).unwrap(), request);
        assert!(Identity::load(&job, "1").unwrap().is_some());
    }

    #[test]
    fn adoption_never_overwrites_a_record_that_landed_first() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "race");
        fs::create_dir(&job.dir).unwrap();
        fs::write(job.path("project"), "p\n").unwrap();
        let stale = Request::from(|key| read(&job.path(key))).unwrap();
        // Enrollment initializes and updates while the reader paused.
        Request::enroll(&job, "judge".into()).unwrap();
        let winner = stale.adopt(&job).unwrap();
        assert_eq!(winner.role.as_deref(), Some("judge"));
        assert_eq!(Request::load(&job).unwrap().role.as_deref(), Some("judge"));
    }

    #[test]
    fn concurrent_first_reads_agree_and_leave_no_staging_files() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "many");
        fs::create_dir(&job.dir).unwrap();
        fs::write(job.path("project"), "p\n").unwrap();
        let seen: Vec<Request> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| Request::load(&job).unwrap()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(seen.iter().all(|r| r == &seen[0]));
        let parent = Request::path(&job).parent().unwrap().to_owned();
        let stray = fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".new."))
            .count();
        assert_eq!(stray, 0);
    }

    #[test]
    fn project_paths_keep_every_byte_but_the_framing_newline() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "spaced");
        fs::create_dir(&job.dir).unwrap();
        legacy_capture(&job, "1", "/projects/repo \u{3000}\t");
        let request = Request::load(&job).unwrap();
        assert_eq!(request.project, "/projects/repo \u{3000}\t");
        assert_eq!(read(&job.path("project")).unwrap(), request.project);
        let fresh = Job::new(&cfg, "published");
        fs::create_dir(&fresh.dir).unwrap();
        fs::write(fresh.path("project"), "/projects/repo \n").unwrap();
        assert_eq!(Request::load(&fresh).unwrap().project, "/projects/repo ");
    }

    #[test]
    fn padded_turn_captures_are_found_and_later_enrollment_is_kept() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "padded");
        fs::create_dir(&job.dir).unwrap();
        crate::ask::ensure(&job).unwrap();
        let write = |n: &str, files: &str| {
            fs::write(
                job.turn(n, "execution"),
                format!(
                    r#"{{"choice":{},"files":{}}}"#,
                    serde_json::to_string(&crate::engine::Choice::default()).unwrap(),
                    files
                ),
            )
            .unwrap()
        };
        write("001", r#"{"project":"p","engine":"codex","role":null}"#);
        write("2", r#"{"project":"evil","engine":null,"role":"judge"}"#);
        let request = Request::load(&job).unwrap();
        assert_eq!(request.project, "p");
        assert_eq!(request.role.as_deref(), Some("judge"));
        assert!(request.engine.is_none());
    }

    #[test]
    fn staging_never_opens_an_existing_file() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "orphan");
        fs::create_dir(&job.dir).unwrap();
        let request = Request {
            project: "p".into(),
            ..Request::default()
        };
        request.save(&job).unwrap();
        let path = Request::path(&job);
        // An orphan staging file aliasing the live record, at the very name
        // the next writer would pick.
        let next = std::process::id();
        for i in 0..64 {
            let orphan = path.with_file_name(format!("request.new.{next}.{i}"));
            let _ = fs::hard_link(&path, orphan);
        }
        request.save(&job).unwrap();
        assert_eq!(Request::load(&job).unwrap(), request);
        let bytes = fs::read(&path).unwrap();
        assert!(serde_json::from_slice::<Request>(&bytes).is_ok());
    }

    #[test]
    fn identity_keeps_the_earlier_files_format_for_a_rolled_back_consumer() {
        let (_tmp, cfg) = rig();
        let job = Job::new(&cfg, "rollback");
        fs::create_dir(&job.dir).unwrap();
        Request {
            project: "p".into(),
            role: Some("judge".into()),
            owned: true,
            ..Request::default()
        }
        .save(&job)
        .unwrap();
        Identity::capture_choice(&job, "1", &crate::engine::Choice::default()).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(job.turn("1", "execution")).unwrap()).unwrap();
        assert_eq!(v["files"]["project"], "p");
        assert_eq!(v["files"]["role"], "judge");
        assert_eq!(v["files"]["owned"], "");
        assert!(v["files"]["app"].is_null());
    }
}
