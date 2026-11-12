//! The snapshot's change stream. proj has no daemon, so the "socket" is a
//! directory of Unix datagram sockets, one per subscriber (`Config::stream`).
//! Whatever writes proj's records (a slot taken, or a turn's unit ended) calls [`notify`],
//! which sends one datagram to each subscriber; no timer is involved. A
//! subscriber's first read is the current snapshot, then one snapshot per
//! batch of changes: datagrams that arrived together are one event.

use std::fs;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::Snapshot;
use crate::config::Config;
use crate::{Fail, now_nanos};

/// How many recent jobs each event's snapshot lists.
const JOBS: usize = super::JOBS;

/// Tell every subscriber that something changed. Subscribers that are gone
/// are cleared out; nothing else here can fail a caller.
pub fn notify(cfg: &Config) {
    let Some(dir) = &cfg.stream else { return };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let Ok(me) = UnixDatagram::unbound() else {
        return;
    };
    let _ = me.set_nonblocking(true);
    for entry in entries.flatten() {
        let path = entry.path();
        match me.send_to(b"!", &path) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                let _ = fs::remove_file(&path);
            }
            _ => {}
        }
    }
}

/// A reader of the stream.
pub struct Subscriber<'a> {
    cfg: &'a Config,
    sock: UnixDatagram,
    path: PathBuf,
    first: bool,
}

impl Subscriber<'_> {
    /// The next event: the snapshot at once the first time, then one for each
    /// batch of changes. None when `wait` passes with no change.
    pub fn next(&mut self, wait: Duration) -> Result<Option<Snapshot>, Fail> {
        if !std::mem::take(&mut self.first) {
            let end = Instant::now() + wait;
            let mut buf = [0u8; 8];
            self.sock.set_nonblocking(false).map_err(fail)?;
            loop {
                let left = end
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1));
                self.sock.set_read_timeout(Some(left)).map_err(fail)?;
                match self.sock.recv(&mut buf) {
                    Ok(_) => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        return Ok(None);
                    }
                    Err(e) => return Err(fail(e)),
                }
            }
            // Everything already queued is the same batch.
            let mut buf = [0u8; 8];
            self.sock.set_nonblocking(true).map_err(fail)?;
            while self.sock.recv(&mut buf).is_ok() {}
        }
        Snapshot::take(self.cfg, JOBS).map(Some)
    }
}

impl Drop for Subscriber<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn fail(e: io::Error) -> Fail {
    Fail::new(1, format!("proj: stream: {e}"))
}

/// Subscribe: bind a socket in the stream's directory, before reading the
/// snapshot, so no change falls between the two.
pub fn subscribe(cfg: &Config) -> Result<Subscriber<'_>, Fail> {
    let dir = cfg
        .stream
        .as_ref()
        .ok_or_else(|| Fail::new(1, "proj: no stream directory is configured"))?;
    let path = dir.join(format!("{}-{}.sock", std::process::id(), now_nanos()));
    let sock = UnixDatagram::bind(&path).map_err(fail)?;
    Ok(Subscriber {
        cfg,
        sock,
        path,
        first: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::example;

    fn cfg_with_stream(dir: &std::path::Path) -> Config {
        let mut cfg = example();
        cfg["jobs"] = serde_json::json!(dir.join("jobs"));
        cfg["stream"] = serde_json::json!(dir.join("stream"));
        fs::create_dir_all(dir.join("jobs")).unwrap();
        fs::create_dir_all(dir.join("stream")).unwrap();
        serde_json::from_value(cfg).unwrap()
    }

    #[test]
    fn stream_a_subscriber_gets_one_event_per_change_batch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_stream(dir.path());
        let mut sub = subscribe(&cfg).unwrap();
        assert!(
            sub.next(Duration::from_secs(1)).unwrap().is_some(),
            "the snapshot first"
        );
        assert!(sub.next(Duration::from_millis(100)).unwrap().is_none());
        notify(&cfg);
        notify(&cfg);
        assert!(
            sub.next(Duration::from_secs(1)).unwrap().is_some(),
            "an event within a second"
        );
        assert!(
            sub.next(Duration::from_millis(100)).unwrap().is_none(),
            "coalesced"
        );
    }

    #[test]
    fn stream_a_reconnecting_reader_gets_the_snapshot_first() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_stream(dir.path());
        drop(subscribe(&cfg).unwrap());
        notify(&cfg);
        let mut again = subscribe(&cfg).unwrap();
        let first = again.next(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(first.version, super::super::VERSION);
        assert!(again.next(Duration::from_millis(100)).unwrap().is_none());
    }

    #[test]
    fn stream_taking_a_slot_notifies_a_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_stream(dir.path());
        let mut sub = subscribe(&cfg).unwrap();
        sub.next(Duration::from_secs(1)).unwrap();
        let job = crate::job::Job::new(&cfg, "held");
        fs::create_dir_all(&job.dir).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Manual,
        )
        .unwrap();
        let _slot =
            crate::slot::take(&cfg, "held", "1", crate::pick::Class::Normal, false).unwrap();
        let started = Instant::now();
        assert!(
            sub.next(Duration::from_secs(1)).unwrap().is_some(),
            "one event within a second"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(sub.next(Duration::from_millis(100)).unwrap().is_none());
    }

    #[test]
    fn stream_a_class_change_under_one_prefix_is_news_and_a_repeat_is_not() {
        use crate::pick::Class;
        let mut shown = None;
        assert!(!crate::slot::class_is_news(&mut shown, Class::Lead));
        assert!(!crate::slot::class_is_news(&mut shown, Class::Lead));
        assert!(crate::slot::class_is_news(&mut shown, Class::Pipeline));
        assert!(!crate::slot::class_is_news(&mut shown, Class::Pipeline));
    }

    #[test]
    fn stream_leaving_the_claude_line_for_codex_notifies_a_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_with_stream(dir.path());
        let queue = cfg.jobs.join(".queue");
        fs::create_dir_all(&queue).unwrap();
        fs::write(
            queue.join("2-1700000000000000000-held"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let job = crate::job::Job::new(&cfg, "held");
        fs::create_dir_all(&job.dir).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Manual,
        )
        .unwrap();
        let mut sub = subscribe(&cfg).unwrap();
        sub.next(Duration::from_secs(1)).unwrap();
        let _slot = crate::slot::take_engine(
            &cfg,
            "held",
            "1",
            crate::pick::Class::Normal,
            false,
            crate::engine::Engine::Codex,
        )
        .unwrap();
        assert!(!queue.join("2-1700000000000000000-held").exists());
        assert!(sub.next(Duration::from_secs(1)).unwrap().is_some());
    }
}
