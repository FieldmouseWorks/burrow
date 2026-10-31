//! The agents' own keys, should one turn up in what they say (an error
//! quoting its request, say): taken out of everything an agent says before
//! it's logged or kept.

use std::collections::BTreeSet;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use serde_json::Value;

use crate::held;

/// Nothing shorter is a key, an empty value least of all.
const SHORTEST: usize = 16;

/// Codex's login, in its directory under the reviewers' home.
const LOGIN: (&str, &str) = (".codex", "auth.json");

/// Every string in Codex's login (`HOME/.codex/auth.json`) long enough to
/// be a key; none if there's no login to read.
pub fn secrets(home: &Path) -> Vec<String> {
    fn walk(x: &Value, found: &mut Vec<String>) {
        match x {
            Value::String(s) if s.chars().count() >= SHORTEST => found.push(s.clone()),
            Value::Object(m) => m.values().for_each(|v| walk(v, found)),
            Value::Array(a) => a.iter().for_each(|v| walk(v, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    if let Some(login) = std::fs::read(home.join(LOGIN.0).join(LOGIN.1))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
        walk(&login, &mut found);
    }
    found
}

/// The login, watched for as long as this is held: each write to it, a
/// refresh by a look or the one beside it, has its keys kept the moment
/// it's done, so a refresh that another replaces before the look that made
/// it ends is still scrubbed from what that look says. Its directory is
/// what's watched: that sees a login saved over it (codex.rs `keep_login`)
/// and one written in place, through a sandbox's bind of it too.
pub struct Watch {
    inotify: Arc<OwnedFd>,
    dir: i32,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    pub fn start(home: &Path, seen: Arc<Mutex<BTreeSet<String>>>) -> io::Result<Watch> {
        let inotify = Arc::new(inotify::init(CreateFlags::CLOEXEC)?);
        let wd = inotify::add_watch(
            &*inotify,
            home.join(LOGIN.0),
            WatchFlags::CLOSE_WRITE | WatchFlags::MOVED_TO,
        )?;
        let (fd, home) = (inotify.clone(), home.to_owned());
        let thread = thread::spawn(move || {
            let mut buf = [MaybeUninit::uninit(); 4096];
            let mut events = inotify::Reader::new(&*fd, &mut buf);
            loop {
                match events.next() {
                    // The watch removed: by drop, or with its directory.
                    Ok(e) if e.events().contains(ReadFlags::IGNORED) => return,
                    Ok(e)
                        if e.file_name()
                            .is_some_and(|n| n.to_bytes() == LOGIN.1.as_bytes()) =>
                    {
                        held(&seen).extend(secrets(&home));
                    }
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(_) => return,
                }
            }
        });
        Ok(Watch {
            inotify,
            dir: wd,
            thread: Some(thread),
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // The watch removed is the thread's last event; one that's gone
        // already took the thread with it.
        let removed = inotify::remove_watch(&*self.inotify, self.dir).is_ok();
        if let Some(t) = self.thread.take()
            && (removed || t.is_finished())
        {
            let _ = t.join();
        }
    }
}

/// x with those keys taken out, wherever they are in it.
pub fn scrub(x: &Value, keys: &[String]) -> Value {
    let mut text = x.to_string();
    for k in keys {
        let quoted = Value::String(k.clone()).to_string();
        text = text.replace(&quoted[1..quoted.len() - 1], "[redacted]");
    }
    serde_json::from_str(&text).unwrap_or_else(|_| Value::String("[redacted]".into()))
}

/// What an agent said, its keys taken out.
pub fn said(text: &str, keys: &[String]) -> String {
    match scrub(&Value::String(text.to_owned()), keys) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// The last n characters of what an agent said, its keys taken out first so
/// none is cut in half.
pub fn tail(text: &str, n: usize, keys: &[String]) -> String {
    let text = said(text, keys);
    let skip = text.chars().count().saturating_sub(n);
    text.chars().skip(skip).collect()
}

/// The first n characters, the same way.
pub fn head(text: &str, n: usize, keys: &[String]) -> String {
    said(text, keys).chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{head, said, scrub, tail};

    /// A key goes wherever it is, escaped as JSON escapes it, and a cut
    /// never leaves half of one.
    #[test]
    fn keys_come_out_whole() {
        let keys = vec!["codexREFRESHtoken\"QWERTY\\zxcvb".to_owned()];
        let k = &keys[0];
        assert_eq!(
            scrub(&json!({"a": [format!("x{k}y")], "b": {"c": k}}), &keys),
            json!({"a": ["x[redacted]y"], "b": {"c": "[redacted]"}})
        );
        assert_eq!(said(&format!("panic: {k}"), &keys), "panic: [redacted]");
        assert_eq!(tail(&format!("{k} end"), 12, &keys), "edacted] end");
        assert_eq!(head(&format!("x {k}"), 5, &keys), "x [re");
        assert_eq!(said("nothing", &[]), "nothing");
    }
}
