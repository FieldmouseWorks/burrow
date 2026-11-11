//! The owner's passkeys, their sessions and the WebAuthn ceremonies in
//! flight, with no HTTP: the routes (web.rs) hold one of these behind a
//! lock. What has to outlive the process (passkeys, sessions) is written to
//! the state directory as it changes; a ceremony lasts minutes, and lives
//! only here.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::{
    CreationChallengeResponse, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse, Url, Uuid,
    Webauthn, WebauthnBuilder,
};

use crate::session::{Limits, Session, Sessions, Use};
use crate::{Config, Owner, digest, enroll, store, token};

/// Seconds a ceremony may take, from its options to its answer.
pub const CEREMONY_TTL: u64 = 5 * 60;
/// Ceremonies in flight at once; past it the oldest goes. Anyone on the
/// tailnet can start one, so they can't be allowed to pile up.
pub const MAX_PENDING: usize = 32;
/// The longest name a passkey gets.
pub const MAX_NAME: usize = 40;

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
    state: PathBuf,
    passkeys: Vec<Enrolled>,
    sessions: Sessions,
    pending: HashMap<String, (Ceremony, u64)>,
}

impl Core {
    /// The dashboard's state, read from its directory.
    pub fn open(cfg: &Config) -> std::result::Result<Core, String> {
        let origin = Url::parse(&cfg.origin).map_err(|e| format!("origin {}: {e}", cfg.origin))?;
        let webauthn = WebauthnBuilder::new(cfg.rp_id()?, &origin)
            .and_then(|b| {
                b.rp_name(&cfg.title)
                    .timeout(Duration::from_secs(CEREMONY_TTL))
                    .build()
            })
            .map_err(|e| format!("WebAuthn for {}: {e}", cfg.origin))?;
        let file = |name: &str| cfg.state.join(name);
        let passkeys = store::load(&file("passkeys.json"))
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let sessions = store::load(&file("sessions.json"))
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        Ok(Core {
            webauthn,
            owner: cfg.owner.clone(),
            limits: Limits {
                idle: cfg.idle_timeout,
                max: cfg.max_age,
            },
            state: cfg.state.clone(),
            passkeys,
            sessions,
            pending: HashMap::new(),
        })
    }

    pub fn code_file(&self) -> PathBuf {
        self.state.join("enroll.json")
    }

    pub fn passkeys(&self) -> &[Enrolled] {
        &self.passkeys
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn save_passkeys(&self) -> io::Result<()> {
        store::save(&self.state.join("passkeys.json"), &self.passkeys)
    }

    fn save_sessions(&self) -> io::Result<()> {
        store::save(&self.state.join("sessions.json"), &self.sessions)
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

    /// Begin a session for the passkey named `passkey`: its token, for the
    /// cookie.
    fn begin(&mut self, passkey: String, now: u64) -> Result<String> {
        let token = token();
        self.sessions.sweep(now, self.limits);
        self.sessions.begin(digest(&token), passkey, now);
        self.save_sessions()?;
        Ok(token)
    }

    /// Start enrolling a device, if `code` is the pending code: the
    /// ceremony's id and the options for `navigator.credentials.create`.
    pub fn enrol_start(
        &mut self,
        code: &str,
        name: &str,
        now: u64,
    ) -> Result<(String, CreationChallengeResponse)> {
        let code = match enroll::check(&self.code_file(), code, now)? {
            Ok(d) => d,
            Err(enroll::Refused::NoCode) => return Err(Error::NoCode),
            Err(enroll::Refused::Wrong { left }) => return Err(Error::WrongCode { left }),
        };
        let name = self.name(name);
        let exclude = self
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

    /// Finish enrolling: keep the passkey, use up the code, and begin a
    /// session with it (the device has just proven its owner is there).
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
            .passkeys
            .iter()
            .any(|p| p.passkey.cred_id() == passkey.cred_id())
        {
            return Err(Error::Enrolled);
        }
        if !enroll::redeem(&self.code_file(), &code, now)? {
            return Err(Error::NoCode);
        }
        self.passkeys.push(Enrolled {
            name: name.clone(),
            added: now,
            used: Some(now),
            passkey,
        });
        self.save_passkeys()?;
        self.begin(name, now)
    }

    /// Start signing in: the ceremony's id and the options for
    /// `navigator.credentials.get`.
    pub fn login_start(&mut self, now: u64) -> Result<(String, RequestChallengeResponse)> {
        if self.passkeys.is_empty() {
            return Err(Error::NoPasskeys);
        }
        let keys: Vec<Passkey> = self.passkeys.iter().map(|p| p.passkey.clone()).collect();
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
        let Some(key) = self
            .passkeys
            .iter_mut()
            .find(|p| p.passkey.cred_id() == result.cred_id())
        else {
            return Err(Error::Rejected("not an enrolled passkey".into()));
        };
        key.passkey.update_credential(&result);
        key.used = Some(now);
        let name = key.name.clone();
        self.save_passkeys()?;
        self.begin(name, now)
    }

    /// The session the cookie's token names, if it's live.
    pub fn session(&mut self, token: &str, now: u64) -> Result<Option<Live>> {
        match self.sessions.use_(&digest(token), now, self.limits) {
            Use::None => Ok(None),
            Use::Live { session, dirty } => {
                if dirty {
                    self.save_sessions()?;
                }
                let ends = session.ends(self.limits);
                Ok(Some(Live { session, ends }))
            }
        }
    }

    pub fn logout(&mut self, token: &str) -> Result<()> {
        if self.sessions.end(&digest(token)) {
            self.save_sessions()?;
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
        let taken = |n: &str| self.passkeys.iter().any(|p| p.name == n);
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

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
