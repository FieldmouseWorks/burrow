//! One-time enrolment codes. `burrow-dashboard enroll`, run on the machine,
//! prints one and keeps only its digest; a browser that types it may create
//! one passkey. A code lasts `CODE_TTL`, and `TRIES` wrong guesses burn it.
//! A new code replaces the last.

use std::io;
use std::path::Path;

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

/// A fresh code, ten characters (fifty bits) in two groups of five.
pub fn issue(path: &Path, now: u64) -> io::Result<String> {
    let bytes = random::<10>();
    let raw: String = bytes
        .iter()
        .map(|b| ALPHABET[usize::from(b % 32)] as char)
        .collect();
    store::save(
        path,
        &Pending {
            digest: digest(&raw),
            expires: now + CODE_TTL,
            tries: TRIES,
        },
    )?;
    Ok(format!("{}-{}", &raw[..5], &raw[5..]))
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

fn pending(path: &Path, now: u64) -> io::Result<Option<Pending>> {
    match store::load::<Pending>(path)? {
        Some(p) if now < p.expires && p.tries > 0 => Ok(Some(p)),
        Some(_) => {
            store::remove(path)?;
            Ok(None)
        }
        None => Ok(None),
    }
}

/// Whether `typed` is the pending code. Right, it gives the code's digest,
/// to `redeem` once the passkey is made; the code stays until then. Wrong,
/// it costs a try.
pub fn check(path: &Path, typed: &str, now: u64) -> io::Result<Result<String, Refused>> {
    let Some(mut p) = pending(path, now)? else {
        return Ok(Err(Refused::NoCode));
    };
    let given = digest(&normalize(typed));
    if same(&given, &p.digest) {
        return Ok(Ok(given));
    }
    p.tries -= 1;
    if p.tries == 0 {
        store::remove(path)?;
    } else {
        store::save(path, &p)?;
    }
    Ok(Err(Refused::Wrong { left: p.tries }))
}

/// Use the code `check` passed: whether it was still the pending one, and
/// so whether the passkey it was for may be kept.
pub fn redeem(path: &Path, code_digest: &str, now: u64) -> io::Result<bool> {
    match pending(path, now)? {
        Some(p) if same(&p.digest, code_digest) => {
            store::remove(path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enroll.json");
        (dir, path)
    }

    #[test]
    fn the_right_code_passes_until_redeemed_once() {
        let (_d, path) = fresh();
        let code = issue(&path, 100).unwrap();
        assert_eq!(code.len(), 11);
        let d = check(&path, &code, 101).unwrap().unwrap();
        // Typed loosely, it's the same code.
        let loose = code.to_lowercase().replace('-', " ");
        assert_eq!(check(&path, &loose, 102).unwrap(), Ok(d.clone()));
        assert!(redeem(&path, &d, 103).unwrap());
        assert!(!redeem(&path, &d, 104).unwrap());
        assert_eq!(check(&path, &code, 105).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn a_wrong_code_is_refused_and_enough_of_them_burn_it() {
        let (_d, path) = fresh();
        let code = issue(&path, 0).unwrap();
        for left in (1..TRIES).rev() {
            assert_eq!(
                check(&path, "WRONG-CODE0", 1).unwrap(),
                Err(Refused::Wrong { left })
            );
        }
        assert_eq!(
            check(&path, "WRONG-CODE0", 1).unwrap(),
            Err(Refused::Wrong { left: 0 })
        );
        // Burnt: the right one is no good now either.
        assert_eq!(check(&path, &code, 2).unwrap(), Err(Refused::NoCode));
    }

    #[test]
    fn a_code_runs_out() {
        let (_d, path) = fresh();
        let code = issue(&path, 0).unwrap();
        let d = check(&path, &code, CODE_TTL - 1).unwrap().unwrap();
        assert_eq!(check(&path, &code, CODE_TTL).unwrap(), Err(Refused::NoCode));
        assert!(!redeem(&path, &d, CODE_TTL).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn a_new_code_replaces_the_last() {
        let (_d, path) = fresh();
        let old = issue(&path, 0).unwrap();
        let d = check(&path, &old, 1).unwrap().unwrap();
        let new = issue(&path, 2).unwrap();
        assert!(matches!(
            check(&path, &old, 3).unwrap(),
            Err(Refused::Wrong { .. })
        ));
        // A ceremony the old code began can't keep its passkey.
        assert!(!redeem(&path, &d, 3).unwrap());
        assert!(check(&path, &new, 3).unwrap().is_ok());
    }

    #[test]
    fn no_code_issued_is_no_code() {
        let (_d, path) = fresh();
        assert_eq!(
            check(&path, "ABCDE-FGHJK", 0).unwrap(),
            Err(Refused::NoCode)
        );
    }

    #[test]
    fn normalizing() {
        assert_eq!(normalize(" ab0o1-iL z "), "AB00111Z");
    }
}
