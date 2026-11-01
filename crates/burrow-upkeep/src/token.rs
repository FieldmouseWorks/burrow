//! The App's tokens, which the steward's GitHub calls go as: minted with the
//! App's key, systemd's credential app-key, which only the steward's passes
//! are given. They're kept nowhere but in memory: the steward runs as the
//! machine user, which every Claude session on the machine shares, so a
//! token in a file would let any of them act as the App for its hour. The
//! pass keeps what it's handed (common.py's token), and a word that mints
//! its own keeps them for its process (Installations), and nothing said of
//! one names more than whose it is.

use std::collections::HashMap;
use std::path::Path;

use burrow_github::{Api, Gh};
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

/// The App the config names, with its key from `creds` (systemd's
/// CREDENTIALS_DIRECTORY), at the config's API, through `proxy` or straight.
pub fn app(cfg: &Config, creds: &Path, proxy: Option<ureq::Proxy>) -> Result<App, Error> {
    let key = creds.join("app-key");
    let pem = std::fs::read(&key)
        .map_err(|e| Error::refused(format!("the App's key, {}: {e}", key.display())))?;
    App::new(&cfg.api, proxy, cfg.app.id, &pem)
}

/// A token for `owner`'s repos, at `now` (Unix seconds, the clock's: a
/// token is judged by when it's used, and a pass's own time can be minutes
/// old).
pub fn token(app: &App, owner: &str, now: i64) -> Result<Token, Error> {
    let target = Target::parse(owner)
        .filter(|t| t.repo.is_none())
        .ok_or_else(|| Error::refused(format!("{owner:?} isn't an owner")))?;
    Ok(match app.mint(&target, now)? {
        Some(m) => Token::Minted(m),
        None => Token::NotInstalled,
    })
}

/// GitHub as the App's installation on an owner, for a word that's about
/// more than one owner's repos.
pub trait Owners {
    /// None: the App isn't installed on `owner`.
    fn api(&mut self, owner: &str) -> Result<Option<&mut dyn Api>, Error>;
}

/// Each owner's installation, with a token minted the first time it's
/// asked for. A token lasts an hour, longer than a pass may run, so each is
/// minted once a process; what couldn't be minted is asked again.
pub struct Installations<'a> {
    pub app: &'a App,
    /// The clock, as Unix seconds, the tokens are minted at.
    pub now: i64,
    pub gh: HashMap<String, Option<Gh>>,
}

impl Owners for Installations<'_> {
    fn api(&mut self, owner: &str) -> Result<Option<&mut dyn Api>, Error> {
        let owner = owner.to_lowercase();
        if !self.gh.contains_key(&owner) {
            let gh = match token(self.app, &owner, self.now)? {
                Token::Minted(m) => Some(Gh {
                    token: Some(m.token),
                }),
                Token::NotInstalled => None,
            };
            self.gh.insert(owner.clone(), gh);
        }
        Ok(self
            .gh
            .get_mut(&owner)
            .and_then(Option::as_mut)
            .map(|g| g as &mut dyn Api))
    }
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod tests;
