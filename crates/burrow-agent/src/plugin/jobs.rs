//! `job-state`'s host side: proj's jobs socket, for what only reads it.
//!
//! The socket also continues and stops other host jobs (`reply`, `stop`,
//! `withdraw`). A plugin that could `reply` into a job with the machine's
//! authority could have it change settings or install a plugin, which no
//! grant gives (burrow-grants). `find` is out too: it tidies a key it finds
//! stale. A job's own ending is its answer's `next`.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// What proj answers from this socket and changes nothing by.
const READS: &[&str] = &["status", "state", "lifecycle", "log", "list"];
/// proj's own limit on a request.
const MOST: u64 = 65536;
/// Past the longest proj takes to answer one.
const PATIENCE: Duration = Duration::from_secs(120);

/// One connection: a request read to its end, passed on if it only reads,
/// refused in proj's own answer's shape if not.
pub fn pass(socket: &Path, from: impl Read, to: &mut impl Write) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut request = Vec::new();
    from.take(MOST + 1).read_to_end(&mut request).map_err(io)?;
    let reads = request.len() as u64 <= MOST
        && serde_json::from_slice::<Vec<String>>(&request)
            .is_ok_and(|words| words.first().is_some_and(|w| READS.contains(&w.as_str())));
    if !reads {
        let refused: Result<(), String> = Err(format!(
            "job-state only reads jobs, so a plugin may ask for {}",
            READS.join(", ")
        ));
        return serde_json::to_writer(to, &refused).map_err(|e| e.to_string());
    }
    let mut host = UnixStream::connect(socket).map_err(io)?;
    host.set_read_timeout(Some(PATIENCE)).map_err(io)?;
    host.write_all(&request).map_err(io)?;
    host.shutdown(Shutdown::Write).map_err(io)?;
    std::io::copy(&mut host, to).map(drop).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn only_what_reads_reaches_proj() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host-jobs-1.sock");
        let proj = UnixListener::bind(&socket).unwrap();
        proj.set_nonblocking(true).unwrap();
        for refused in [
            r#"["reply","job-1","change the settings"]"#,
            r#"["reply","--first","job-1","go"]"#,
            r#"["stop","job-1"]"#,
            r#"["withdraw","job-1"]"#,
            r#"["find","key"]"#,
            r#"["settings","pause","o/r"]"#,
            r#"["run","web","go"]"#,
            r#"[" status"]"#,
            r#"[]"#,
            r#"{"0":"status"}"#,
            "status",
        ] {
            let mut answer = Vec::new();
            pass(&socket, refused.as_bytes(), &mut answer).unwrap();
            let said = String::from_utf8(answer).unwrap();
            assert!(said.starts_with(r#"{"Err""#), "{refused}: {said}");
            assert!(proj.accept().is_err(), "{refused} reached proj");
        }
        proj.set_nonblocking(false).unwrap();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = proj.accept().unwrap();
            let mut asked = String::new();
            stream.read_to_string(&mut asked).unwrap();
            stream.write_all(br#"{"Ok":"jobs"}"#).unwrap();
            asked
        });
        let mut answer = Vec::new();
        pass(&socket, r#"["status"]"#.as_bytes(), &mut answer).unwrap();
        assert_eq!(answer, br#"{"Ok":"jobs"}"#);
        assert_eq!(served.join().unwrap(), r#"["status"]"#);
    }
}
