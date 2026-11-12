//! One-time enrolment codes. `burrow-dashboard enroll`, run on the machine,
//! prints one and keeps only its digest; a browser that types it may create
//! one passkey. A code lasts `CODE_TTL`, and `TRIES` wrong guesses burn it.
//! A new code replaces the last, and a code is the owner's it was issued
//! for: another owner configured since finds none.
//!
//! The command and the service are two processes, so every reading and
//! writing of the code is one transaction under an exclusive lock on the
//! state directory itself (no lock file, so none to plant a link at): a
//! wrong guess can't write back a code a new one replaced,
//! and using a code can't remove the one issued after it.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{digest, random, same, store};

/// Seconds a code lasts.
pub const CODE_TTL: u64 = 15 * 60;
/// Wrong guesses a code survives, less one.
pub const TRIES: u32 = 5;
/// Crockford's base32: no I, L, O or U to misread.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Pending {
    /// The GitHub account id it was issued for.
    owner: u64,
    digest: String,
    expires: u64,
    tries: u32,
}

/// Why a code was turned away.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// There's no code, or it ran out, was burnt or was used.
    NoCode,
    /// Not the code; this many more tries before it burns.
    Wrong { left: u32 },
}

/// The pending code in a state directory, for one owner.
#[derive(Clone)]
pub struct Codes {
    file: PathBuf,
    dir: PathBuf,
    owner: u64,
}

impl Codes {
    pub fn new(state: &Path, owner: u64) -> Codes {
        Codes {
            file: state.join("enroll.json"),
            dir: state.to_owned(),
            owner,
        }
    }

    /// Hold the lock until what's returned goes.
    fn lock(&self) -> io::Result<File> {
        let dir = File::open(&self.dir)?;
        dir.lock()?;
        Ok(dir)
    }

    /// The code that's pending, if there is one; one that ran out, was
    /// burnt or is another owner's goes.
    fn pending(&self, now: u64) -> io::Result<Option<Pending>> {
        match store::load::<Pending>(&self.file)? {
            Some(p) if now < p.expires && p.tries > 0 && p.owner == self.owner => Ok(Some(p)),
            Some(_) => {
                store::remove(&self.file)?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// A fresh code, ten characters (fifty bits) in two groups of five.
    pub fn issue(&self, now: u64) -> io::Result<String> {
        let raw: String = random::<10>()
            .iter()
            .map(|b| ALPHABET[usize::from(b % 32)] as char)
            .collect();
        let _held = self.lock()?;
        store::save(
            &self.file,
            &Pending {
                owner: self.owner,
                digest: digest(&raw),
                expires: now + CODE_TTL,
                tries: TRIES,
            },
        )?;
        Ok(format!("{}-{}", &raw[..5], &raw[5..]))
    }

    /// Whether `typed` is the pending code. Right, it gives the code's
    /// digest, to `redeem` once the passkey is made; the code stays until
    /// then. Wrong, it costs a try.
    pub fn check(&self, typed: &str, now: u64) -> io::Result<Result<String, Refused>> {
        let _held = self.lock()?;
        let Some(mut p) = self.pending(now)? else {
            return Ok(Err(Refused::NoCode));
        };
        let given = digest(&normalize(typed));
        if same(&given, &p.digest) {
            return Ok(Ok(given));
        }
        p.tries -= 1;
        if p.tries == 0 {
            store::remove(&self.file)?;
        } else {
            store::save(&self.file, &p)?;
        }
        Ok(Err(Refused::Wrong { left: p.tries }))
    }

    /// Use the code `check` passed, if it's still the pending one: `keep`
    /// keeps what it was for, and only once that's done is the code used
    /// up. None when the code is gone (used, replaced, run out), and
    /// `keep` never ran.
    pub fn redeem<T, E: From<io::Error>>(
        &self,
        code_digest: &str,
        now: u64,
        keep: impl FnOnce() -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        let _held = self.lock()?;
        match self.pending(now)? {
            Some(p) if same(&p.digest, code_digest) => {
                let kept = keep()?;
                store::remove(&self.file)?;
                Ok(Some(kept))
            }
            _ => Ok(None),
        }
    }
}

/// The code as typed, as it was issued: case, dashes and spaces don't
/// matter, and the letters Crockford leaves out read as the digits they
/// look like.
pub fn normalize(typed: &str) -> String {
    typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn fresh() -> (tempfile::TempDir, Codes) {
        let dir = tempfile::tempdir().unwrap();
        let codes = Codes::new(dir.path(), 7);
        (dir, codes)
    }

    fn redeem(c: &Codes, d: &str, now: u64) -> bool {
        c.redeem(d, now, || Ok::<_, io::Error>(()))
            .unwrap()
            .is_some()
    }

    #[test]
    fn the_right_code_passes_until_redeemed_once() {
        let (_d, c) = fresh();
        let code = c.issue(100).unwrap();
        assert_eq!(code.len(), 11);
        let d = c.check(&code, 101).unwrap().unwrap();
        // Typed loosely, it's the same code.
        let loose = code.to_lowercase().replace('-', " ");
        assert_eq!(c.check(&loose, 102).unwrap(), Ok(d.clone()));
        assert!(redeem(&c, &d, 103));
        assert!(!redeem(&c, &d, 104));
        assert_eq!(c.check(&code, 105).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn a_code_is_used_up_only_once_what_it_was_for_is_kept() {
        let (_d, c) = fresh();
        let code = c.issue(0).unwrap();
        let d = c.check(&code, 1).unwrap().unwrap();
        let failed = c.redeem(&d, 2, || Err::<(), _>(io::Error::other("disk full")));
        assert!(failed.is_err());
        // Still pending: the owner can try again.
        assert_eq!(c.redeem(&d, 3, || Ok::<_, io::Error>(5)).unwrap(), Some(5));
        assert!(!redeem(&c, &d, 4));
    }

    #[test]
    fn a_wrong_code_is_refused_and_enough_of_them_burn_it() {
        let (_d, c) = fresh();
        let code = c.issue(0).unwrap();
        for left in (0..TRIES).rev() {
            assert_eq!(
                c.check("WRONG-CODE0", 1).unwrap(),
                Err(Refused::Wrong { left })
            );
        }
        // Burnt: the right one is no good now either.
        assert_eq!(c.check(&code, 2).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn a_code_runs_out() {
        let (dir, c) = fresh();
        let code = c.issue(0).unwrap();
        let d = c.check(&code, CODE_TTL - 1).unwrap().unwrap();
        assert_eq!(c.check(&code, CODE_TTL).unwrap(), Err(Refused::NoCode));
        assert!(!redeem(&c, &d, CODE_TTL));
        assert!(!dir.path().join("enroll.json").exists());
    }

    #[test]
    fn a_new_code_replaces_the_last() {
        let (_d, c) = fresh();
        let old = c.issue(0).unwrap();
        let d = c.check(&old, 1).unwrap().unwrap();
        let new = c.issue(2).unwrap();
        assert!(matches!(
            c.check(&old, 3).unwrap(),
            Err(Refused::Wrong { .. })
        ));
        // A ceremony the old code began can't keep its passkey.
        assert!(!redeem(&c, &d, 3));
        assert!(c.check(&new, 3).unwrap().is_ok());
    }

    #[test]
    fn a_code_is_its_owners() {
        let (dir, c) = fresh();
        let code = c.issue(0).unwrap();
        let other = Codes::new(dir.path(), 8);
        assert_eq!(other.check(&code, 1).unwrap(), Err(Refused::NoCode));
        // And it's gone, so changing the owner back doesn't revive it.
        assert_eq!(c.check(&code, 1).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn a_code_issued_while_another_is_used_up_survives() {
        let (dir, c) = fresh();
        let old = c.issue(0).unwrap();
        let d = c.check(&old, 1).unwrap().unwrap();
        let (tx, rx) = mpsc::channel();
        let state = dir.path().to_owned();
        let kept = c
            .redeem(&d, 2, || {
                // The command, in a process of its own, issues a new code
                // while the old one is being used up.
                let issuer = thread::spawn(move || {
                    let code = Codes::new(&state, 7).issue(2).unwrap();
                    tx.send(code).unwrap();
                });
                // It waits for the lock rather than write meanwhile.
                assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
                Ok::<_, io::Error>(issuer)
            })
            .unwrap()
            .unwrap();
        kept.join().unwrap();
        let new = rx.recv().unwrap();
        // Using up the old code didn't take the new one with it.
        assert!(c.check(&new, 3).unwrap().is_ok());
    }

    #[test]
    fn a_wrong_guess_never_writes_back_a_replaced_code() {
        let (dir, c) = fresh();
        let old = c.issue(0).unwrap();
        let state = dir.path().to_owned();
        let held = c.lock().unwrap();
        // A wrong guess waits for the lock while a new code is issued...
        let guess = thread::spawn(move || Codes::new(&state, 7).check("WRONG-CODE0", 1).unwrap());
        thread::sleep(Duration::from_millis(100));
        drop(held);
        let new = c.issue(1).unwrap();
        let _ = guess.join().unwrap();
        // ...and whichever went first, the old code never comes back.
        assert!(c.check(&old, 2).unwrap().is_err());
        assert!(c.check(&new, 2).unwrap().is_ok());
    }

    #[test]
    fn no_code_issued_is_no_code() {
        let (_d, c) = fresh();
        assert_eq!(c.check("ABCDE-FGHJK", 0).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn normalizing() {
        assert_eq!(normalize(" ab0o1-iL z "), "AB00111Z");
    }
}
