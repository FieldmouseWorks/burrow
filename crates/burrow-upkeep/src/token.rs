//! The App's token for an owner, which the steward's GitHub calls go as:
//! minted with the App's key, systemd's credential app-key, which only the
//! steward's passes are given. It's kept nowhere: the steward runs as the
//! machine user, which every Claude session on the machine shares, so a
//! token in a file would let any of them act as the App for its hour. The
//! pass keeps what it's handed in memory (common.py's token), and nothing
//! said of it names more than whose it is.

use std::path::Path;

use gh_token::app::{App, Minted};
use gh_token::error::Error;
use gh_token::target::Target;

use crate::config::Config;

/// What the App has for an owner: a token, or none because it isn't
/// installed there, which is GitHub's answer rather than a failure to ask.
pub enum Token {
    Minted(Minted),
    NotInstalled,
}

/// A token for `owner`'s repos, with the key in `creds` (systemd's
/// CREDENTIALS_DIRECTORY), at `now` (Unix seconds, the clock's: a token is
/// judged by when it's used, and a pass's own time can be minutes old).
pub fn token(
    cfg: &Config,
    owner: &str,
    creds: &Path,
    proxy: Option<ureq::Proxy>,
    now: i64,
) -> Result<Token, Error> {
    let target = Target::parse(owner)
        .filter(|t| t.repo.is_none())
        .ok_or_else(|| Error::refused(format!("{owner:?} isn't an owner")))?;
    let key = creds.join("app-key");
    let pem = std::fs::read(&key)
        .map_err(|e| Error::refused(format!("the App's key, {}: {e}", key.display())))?;
    let app = App::new(&cfg.api, proxy, cfg.app.id, &pem)?;
    Ok(match app.mint(&target, now)? {
        Some(m) => Token::Minted(m),
        None => Token::NotInstalled,
    })
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod tests;
