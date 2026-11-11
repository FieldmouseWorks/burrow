//! The owner's passkeys, their sessions and the WebAuthn ceremonies in
//! flight, with no HTTP: the routes (web.rs) hold one of these behind a
//! lock. What has to outlive the process (passkeys, sessions) is one file,
//! state.json, stamped with the owner's account id: state of any other
//! owner is never loaded. Every change is made to a copy, saved, and only
//! then taken as the state, so a save that fails changes nothing and the
//! same request can be tried again. A ceremony lasts minutes, and lives
//! only here.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::{
    CreationChallengeResponse, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse, Url, Uuid,
    Webauthn, WebauthnBuilder,
};

use crate::enroll::{Codes, Refused};
use crate::session::{Activity, Limits, Session, Sessions, Use};
use crate::{Config, Owner, digest, store, token};

/// Seconds a ceremony may take, from its options to its answer.
pub const CEREMONY_TTL: u64 = 5 * 60;
/// Ceremonies in flight at once; past it the oldest goes. Anyone on the
/// tailnet can start one, so they can't be allowed to pile up.
pub const MAX_PENDING: usize = 32;
/// The longest name a passkey gets.
pub const MAX_NAME: usize = 40;

/// What's saved: the owner's passkeys and sessions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    /// The GitHub account id it all belongs to.
    owner: u64,
    passkeys: Vec<Enrolled>,
    sessions: Sessions,
}

/// A passkey the owner enrolled.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Enrolled {
    /// What the owner called the device.
    pub name: String,
    pub added: u64,
    pub used: Option<u64>,
    pub passkey: Passkey,
}

enum Ceremony {
    Enrol {
        state: PasskeyRegistration,
        /// The digest of the code that let it start.
        code: String,
        name: String,
    },
    Login(PasskeyAuthentication),
}

/// Why something was turned away: each is said to the browser as it is.
#[derive(Debug)]
pub enum Error {
    /// No enrolment code is pending: it ran out, was burnt, used or never
    /// issued.
    NoCode,
    WrongCode {
        left: u32,
    },
    /// There's no passkey to sign in with yet.
    NoPasskeys,
    /// The ceremony isn't one in flight: it ran out, or was answered already.
    NoCeremony,
    /// The authenticator's answer didn't verify.
    Rejected(String),
    /// The passkey is enrolled already.
    Enrolled,
    /// The state directory failed us.
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NoCode => write!(
                f,
                "No enrolment code is waiting. Print a new one on the machine."
            ),
            Error::WrongCode { left: 0 } => {
                write!(
                    f,
                    "That isn't the code, and it's used up now. Print a new one on the machine."
                )
            }
            Error::WrongCode { left } => write!(
                f,
                "That isn't the code. {left} more {} before it's used up.",
                if *left == 1 { "try" } else { "tries" }
            ),
            Error::NoPasskeys => write!(f, "No passkey is enrolled yet. Add this device first."),
            Error::NoCeremony => write!(f, "That took too long. Try again."),
            Error::Rejected(why) => write!(f, "The passkey wasn't accepted ({why})."),
            Error::Enrolled => write!(f, "This passkey is enrolled already."),
            Error::Io(e) => write!(f, "The dashboard couldn't save its state ({e})."),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A session, as the routes see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    pub session: Session,
    /// When it ends if it isn't used again.
    pub ends: u64,
}

pub struct Core {
    webauthn: Webauthn,
    owner: Owner,
    limits: Limits,
    file: PathBuf,
    codes: Codes,
    state: State,
    /// Session uses not saved yet.
    activity: Activity,
    pending: HashMap<String, (Ceremony, u64)>,
}

impl Core {
    /// The dashboard's state, read from its directory. State saved for
    /// another owner is dropped, on disk too, so a change of owner (and
    /// back) never lets the other's passkeys or sessions in.
    pub fn open(cfg: &Config) -> std::result::Result<Core, String> {
        let origin = Url::parse(&cfg.origin).map_err(|e| format!("origin {}: {e}", cfg.origin))?;
        let webauthn = WebauthnBuilder::new(cfg.rp_id()?, &origin)
            .and_then(|b| {
                b.rp_name(&cfg.title)
                    .timeout(Duration::from_secs(CEREMONY_TTL))
                    .build()
            })
            .map_err(|e| format!("WebAuthn for {}: {e}", cfg.origin))?;
        let file = cfg.state.join("state.json");
        let state = load(&file, cfg.owner.id).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Core {
            webauthn,
            owner: cfg.owner.clone(),
            limits: Limits {
                idle: cfg.idle_timeout,
                max: cfg.max_age,
            },
            file,
            codes: Codes::new(&cfg.state, cfg.owner.id),
            state,
            activity: Activity::default(),
            pending: HashMap::new(),
        })
    }

    /// The enrolment codes, for this owner.
    pub fn codes(&self) -> &Codes {
        &self.codes
    }

    pub fn passkeys(&self) -> &[Enrolled] {
        &self.state.passkeys
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Save `next`, then take it as the state; failing, nothing changes.
    fn commit(&mut self, next: State) -> io::Result<()> {
        store::save(&self.file, &next)?;
        self.state = next;
        Ok(())
    }

    /// Hold a ceremony for its answer, under a fresh id.
    fn hold(&mut self, ceremony: Ceremony, now: u64) -> String {
        self.pending.retain(|_, (_, expires)| now < *expires);
        while self.pending.len() >= MAX_PENDING {
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(_, (_, expires))| *expires)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.pending.remove(&id);
            }
        }
        let id = token();
        self.pending
            .insert(id.clone(), (ceremony, now + CEREMONY_TTL));
        id
    }

    /// The ceremony `id`, once: answered or not, it's gone after this.
    fn take(&mut self, id: &str, now: u64) -> Result<Ceremony> {
        match self.pending.remove(id) {
            Some((c, expires)) if now < expires => Ok(c),
            _ => Err(Error::NoCeremony),
        }
    }

    /// `next` with a session begun for the passkey named `passkey`, and the
    /// session's token, for the cookie.
    fn begin(&self, next: &mut State, passkey: String, now: u64) -> String {
        let token = token();
        next.sessions.sweep(now, self.limits);
        next.sessions.begin(digest(&token), passkey, now);
        token
    }

    /// Start enrolling a device, if `code` is the pending code: the
    /// ceremony's id and the options for `navigator.credentials.create`.
    pub fn enrol_start(
        &mut self,
        code: &str,
        name: &str,
        now: u64,
    ) -> Result<(String, CreationChallengeResponse)> {
        let code = match self.codes.check(code, now)? {
            Ok(d) => d,
            Err(Refused::NoCode) => return Err(Error::NoCode),
            Err(Refused::Wrong { left }) => return Err(Error::WrongCode { left }),
        };
        let name = self.name(name);
        let exclude = self
            .state
            .passkeys
            .iter()
            .map(|p| p.passkey.cred_id().clone())
            .collect::<Vec<_>>();
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                Uuid::from_u128(u128::from(self.owner.id)),
                &self.owner.login,
                &self.owner.login,
                Some(exclude),
            )
            .map_err(|e| Error::Rejected(e.to_string()))?;
        let id = self.hold(Ceremony::Enrol { state, code, name }, now);
        Ok((id, options))
    }

    /// Finish enrolling: keep the passkey and begin a session with it (the
    /// device has just proven its owner is there), and only once that's
    /// saved use up the code.
    pub fn enrol_finish(
        &mut self,
        id: &str,
        answer: &RegisterPublicKeyCredential,
        now: u64,
    ) -> Result<String> {
        let Ceremony::Enrol { state, code, name } = self.take(id, now)? else {
            return Err(Error::NoCeremony);
        };
        let passkey = self
            .webauthn
            .finish_passkey_registration(answer, &state)
            .map_err(|e| Error::Rejected(e.to_string()))?;
        if self
            .state
            .passkeys
            .iter()
            .any(|p| p.passkey.cred_id() == passkey.cred_id())
        {
            return Err(Error::Enrolled);
        }
        let mut next = self.state.clone();
        next.passkeys.push(Enrolled {
            name: name.clone(),
            added: now,
            used: Some(now),
            passkey,
        });
        let token = self.begin(&mut next, name, now);
        let codes = self.codes.clone();
        codes
            .redeem(&code, now, || self.commit(next).map_err(Error::Io))?
            .ok_or(Error::NoCode)?;
        Ok(token)
    }

    /// Start signing in: the ceremony's id and the options for
    /// `navigator.credentials.get`.
    pub fn login_start(&mut self, now: u64) -> Result<(String, RequestChallengeResponse)> {
        if self.state.passkeys.is_empty() {
            return Err(Error::NoPasskeys);
        }
        let keys: Vec<Passkey> = self
            .state
            .passkeys
            .iter()
            .map(|p| p.passkey.clone())
            .collect();
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&keys)
            .map_err(|e| Error::Rejected(e.to_string()))?;
        let id = self.hold(Ceremony::Login(state), now);
        Ok((id, options))
    }

    /// Finish signing in: a new session's token.
    pub fn login_finish(
        &mut self,
        id: &str,
        answer: &PublicKeyCredential,
        now: u64,
    ) -> Result<String> {
        let Ceremony::Login(state) = self.take(id, now)? else {
            return Err(Error::NoCeremony);
        };
        let result = self
            .webauthn
            .finish_passkey_authentication(answer, &state)
            .map_err(|e| Error::Rejected(e.to_string()))?;
        let mut next = self.state.clone();
        let Some(key) = next
            .passkeys
            .iter_mut()
            .find(|p| p.passkey.cred_id() == result.cred_id())
        else {
            return Err(Error::Rejected("not an enrolled passkey".into()));
        };
        key.passkey.update_credential(&result);
        key.used = Some(now);
        let name = key.name.clone();
        let token = self.begin(&mut next, name, now);
        self.commit(next)?;
        Ok(token)
    }

    /// The session the cookie's token names, if it's live. A use a grain
    /// past what's saved is saved now; if that fails, so does the request,
    /// and the use is held to be saved on the next.
    pub fn session(&mut self, token: &str, now: u64) -> Result<Option<Live>> {
        let key = digest(token);
        match self
            .activity
            .use_(&self.state.sessions, &key, now, self.limits)
        {
            Use::None => Ok(None),
            Use::Live { session, save } => {
                if save {
                    let mut next = self.state.clone();
                    next.sessions.touch(&key, session.seen);
                    self.commit(next)?;
                    self.activity.saved(&key);
                }
                let ends = session.ends(self.limits);
                Ok(Some(Live { session, ends }))
            }
        }
    }

    /// End the session `token` names. Failing to save, it isn't ended
    /// (here or on disk), and signing out again tries again.
    pub fn logout(&mut self, token: &str) -> Result<()> {
        let key = digest(token);
        let mut next = self.state.clone();
        if next.sessions.end(&key) {
            self.commit(next)?;
            self.activity.saved(&key);
        }
        Ok(())
    }

    /// The name to keep a new passkey under: what the owner typed, trimmed
    /// and cut short, numbered when another passkey has it (a session says
    /// which passkey began it by name), or just a number when they typed
    /// nothing.
    fn name(&self, typed: &str) -> String {
        let base: String = typed
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME)
            .collect();
        let taken = |n: &str| self.state.passkeys.iter().any(|p| p.name == n);
        let (base, first) = match base.is_empty() {
            true => ("Passkey".to_owned(), 1),
            false if !taken(&base) => return base,
            false => (base, 2),
        };
        (first..)
            .map(|i| format!("{base} {i}"))
            .find(|n| !taken(n))
            .unwrap_or(base)
    }
}

/// The state saved in `file`, if it's `owner`'s. Another owner's is
/// replaced on disk with none at once.
fn load(file: &Path, owner: u64) -> io::Result<State> {
    let fresh = State {
        owner,
        ..State::default()
    };
    match store::load::<State>(file)? {
        Some(s) if s.owner == owner => Ok(s),
        Some(s) => {
            eprintln!(
                "burrow-dashboard: the saved passkeys and sessions were account {}'s, not {owner}'s: dropped",
                s.owner
            );
            store::save(file, &fresh)?;
            Ok(fresh)
        }
        None => Ok(fresh),
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
