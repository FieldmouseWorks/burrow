//! robot: a Hetzner dedicated server's out-of-band controls, through the
//! Robot webservice. It works from the server or from any machine with the
//! credentials, so it's also the way back in when the server itself is down.
//!
//!   robot status                 the server, and its rescue and reset state
//!   robot rescue on [KEY_FP]     boot the Linux rescue system on the next
//!                                reset, with that Robot SSH key (default:
//!                                every key stored in Robot)
//!   robot rescue off             cancel that
//!   robot reset [sw|hw|power]    reset the box (default hw)
//!   robot wol                    send wake-on-LAN
//!
//! The server is $ROBOT_SERVER_IP, or the config's. The credentials are
//! ROBOT_USER and ROBOT_PASSWORD, or "user:password" for a Robot webservice
//! user in $ROBOT_CREDS_FILE or the config's file, read through sudo when
//! this user can't read it. Cancelling the server and reinstalling it are
//! deliberately not here: both are one-way doors, and the guard asks before
//! any raw call to them.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

/// What modules/robot.nix builds in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The server's main IPv4 address, which is how Robot names it.
    pub server_ip: String,
    /// Where "user:password" is kept.
    pub credentials_file: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

pub const USAGE: &str =
    "usage: robot status | rescue on [KEY_FP] | rescue off | reset [sw|hw|power] | wol";

/// One call, as the command line says it.
#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    Status,
    /// With this key, or every key Robot has.
    RescueOn(Option<String>),
    RescueOff,
    Reset(&'static str),
    Wol,
}

/// The call, or the usage line that says what's wrong with it.
pub fn parse(args: &[String]) -> Result<Call, &'static str> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["status"] => Ok(Call::Status),
        ["rescue", "on"] => Ok(Call::RescueOn(None)),
        ["rescue", "on", key] => Ok(Call::RescueOn(Some((*key).to_owned()))),
        ["rescue", "off"] => Ok(Call::RescueOff),
        ["rescue", ..] => Err("usage: robot rescue on [KEY_FP] | off"),
        ["reset"] => Ok(Call::Reset("hw")),
        ["reset", kind] => match *kind {
            "sw" => Ok(Call::Reset("sw")),
            "hw" => Ok(Call::Reset("hw")),
            "power" => Ok(Call::Reset("power")),
            _ => Err("reset type: sw, hw or power"),
        },
        ["wol"] => Ok(Call::Wol),
        _ => Err(USAGE),
    }
}

/// What robot reads from its environment.
#[derive(Debug, Default)]
pub struct Env {
    pub user: Option<String>,
    pub password: Option<String>,
    pub creds_file: Option<String>,
    pub server_ip: Option<String>,
    pub url: Option<String>,
}

impl Env {
    pub fn get() -> Env {
        let var = |name| std::env::var(name).ok();
        Env {
            user: var("ROBOT_USER"),
            password: var("ROBOT_PASSWORD"),
            creds_file: var("ROBOT_CREDS_FILE"),
            server_ip: var("ROBOT_SERVER_IP"),
            url: var("ROBOT_URL"),
        }
    }
}

/// A variable that's set and not empty.
fn set(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

/// The webservice robot asks.
pub const URL: &str = "https://robot-ws.your-server.de";

/// "user:password": from ROBOT_USER and ROBOT_PASSWORD, or else the
/// credentials file, read with `read` or, when that's refused, through
/// `sudo cat`. Its newlines are dropped.
pub fn credentials(
    env: &Env,
    cfg: &Config,
    read: impl Fn(&Path) -> io::Result<String>,
    sudo: &OsStr,
) -> Result<String, String> {
    if let Some(user) = set(&env.user) {
        return Ok(format!("{user}:{}", env.password.as_deref().unwrap_or("")));
    }
    let file = set(&env.creds_file).map_or_else(|| cfg.credentials_file.clone(), PathBuf::from);
    let text = match read(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => sudo_cat(sudo, &file)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "no Robot credentials: put user:password in {}",
                file.display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let creds: String = text.chars().filter(|&c| c != '\n').collect();
    if !creds.contains(':') {
        return Err(format!("{}: not user:password", file.display()));
    }
    Ok(creds)
}

/// The file through sudo, which may ask for a password on the terminal.
fn sudo_cat(sudo: &OsStr, file: &Path) -> Result<String, String> {
    let out = Command::new(sudo)
        .arg("cat")
        .arg(file)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("sudo cat {}: {e}", file.display()))?;
    if !out.status.success() {
        return Err(format!(
            "sudo cat {} failed ({})",
            file.display(),
            out.status
        ));
    }
    String::from_utf8(out.stdout).map_err(|_| format!("{}: not text", file.display()))
}

/// A call's method.
#[derive(Clone, Copy, Debug)]
enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// How long a call may take, as curl's --max-time 30 had it.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The webservice, for one server, as one user.
pub struct Robot {
    agent: ureq::Agent,
    url: String,
    ip: String,
    authorization: String,
}

impl Robot {
    /// `agent` makes the calls; [`agent`] is the real one.
    pub fn new(agent: ureq::Agent, env: &Env, cfg: &Config, creds: &str) -> Robot {
        let basic = base64::engine::general_purpose::STANDARD.encode(creds);
        Robot {
            agent,
            url: set(&env.url)
                .unwrap_or(URL)
                .trim_end_matches('/')
                .to_owned(),
            ip: set(&env.server_ip).unwrap_or(&cfg.server_ip).to_owned(),
            authorization: format!("Basic {basic}"),
        }
    }

    /// A call's JSON answer. A refusal is an error that says Robot's own
    /// reason; the credentials are never in one.
    fn call(&self, method: Method, path: &str, form: Option<&str>) -> Result<Value, String> {
        let url = format!("{}{path}", self.url);
        let auth = ("Authorization", self.authorization.as_str());
        let sent = match (method, form) {
            (Method::Get, _) => self.agent.get(&url).header(auth.0, auth.1).call(),
            (Method::Delete, _) => self.agent.delete(&url).header(auth.0, auth.1).call(),
            (Method::Post, Some(body)) => self
                .agent
                .post(&url)
                .header(auth.0, auth.1)
                .content_type("application/x-www-form-urlencoded")
                .send(body),
            (Method::Post, None) => self.agent.post(&url).header(auth.0, auth.1).send_empty(),
        };
        let said = |why: String| format!("{} {path}: {why}", method.name());
        let mut response = sent.map_err(|e| said(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| said(e.to_string()))?;
        let body: Option<Value> = serde_json::from_str(&text).ok();
        if !(200..300).contains(&status) {
            let error = body.as_ref().map(|b| &b["error"]);
            let reason = match error.map(|e| (&e["code"], &e["message"])) {
                Some((Value::String(code), Value::String(message))) => {
                    format!(" {code}: {message}")
                }
                _ => String::new(),
            };
            return Err(said(format!("HTTP {status}{reason}")));
        }
        body.ok_or_else(|| said("an answer that isn't JSON".to_owned()))
    }

    /// Runs the call; what it prints goes to `out`, what it says to `note`.
    pub fn run(&self, call: &Call, out: &mut String, note: &mut String) -> Result<(), String> {
        let ip = &self.ip;
        match call {
            Call::Status => {
                let server = self.call(Method::Get, &format!("/server/{ip}"), None)?;
                print(
                    out,
                    &pick(
                        &server["server"],
                        &["server_name", "server_ip", "product", "dc", "status"],
                    ),
                );
                let rescue = self.call(Method::Get, &format!("/boot/{ip}/rescue"), None)?;
                print(
                    out,
                    &pick(&rescue["rescue"], &["active", "os", "authorized_key"]),
                );
                let reset = self.call(Method::Get, &format!("/reset/{ip}"), None)?;
                print(out, &pick(&reset["reset"], &["type", "operating_status"]));
            }
            Call::RescueOn(key) => {
                let keys = match key {
                    Some(key) => vec![key.clone()],
                    None => fingerprints(&self.call(Method::Get, "/key", None)?),
                };
                let mut form = String::from("os=linux");
                for key in &keys {
                    let _ = write!(form, "&authorized_key[]={}", escape(key));
                }
                let armed = self.call(Method::Post, &format!("/boot/{ip}/rescue"), Some(&form))?;
                print(out, &pick(&armed["rescue"], &["active", "os", "password"]));
                note.push_str("rescue armed: it boots on the next reset (robot reset)\n");
            }
            Call::RescueOff => {
                let off = self.call(Method::Delete, &format!("/boot/{ip}/rescue"), None)?;
                print(out, &pick(&off["rescue"], &["active"]));
            }
            Call::Reset(kind) => {
                let form = format!("type={}", escape(kind));
                let reset = self.call(Method::Post, &format!("/reset/{ip}"), Some(&form))?;
                print(out, &json(&reset["reset"]));
            }
            Call::Wol => {
                let wol = self.call(Method::Post, &format!("/wol/{ip}"), None)?;
                print(out, &json(&wol["wol"]));
            }
        }
        Ok(())
    }
}

/// The agent robot calls with: the system's roots, curl's 30 seconds, and
/// every status an answer to read.
pub fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Every key's fingerprint in Robot's list of SSH keys.
fn fingerprints(keys: &Value) -> Vec<String> {
    keys.as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k["key"]["fingerprint"].as_str().map(str::to_owned))
        .collect()
}

/// A form value as curl's --data-urlencode sends it: everything but
/// letters, digits and `-._~` percent-encoded.
fn escape(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn print(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

fn json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a Value prints")
}

/// These fields of `value`, in this order, as jq's `{a, b}` picks them: a
/// field it lacks is null.
fn pick(value: &Value, fields: &[&str]) -> String {
    if fields.is_empty() {
        return "{}".to_owned();
    }
    let lines: Vec<String> = fields
        .iter()
        .map(|f| {
            let v = json(value.get(f).unwrap_or(&Value::Null)).replace('\n', "\n  ");
            format!("  {}: {v}", Value::from(*f))
        })
        .collect();
    format!("{{\n{}\n}}", lines.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn calls_read_as_the_usage_says() {
        for (line, call) in [
            ("", Call::Status),
            ("status", Call::Status),
            ("rescue on", Call::RescueOn(None)),
            ("rescue on aa:bb", Call::RescueOn(Some("aa:bb".to_owned()))),
            ("rescue off", Call::RescueOff),
            ("reset", Call::Reset("hw")),
            ("reset sw", Call::Reset("sw")),
            ("reset hw", Call::Reset("hw")),
            ("reset power", Call::Reset("power")),
            ("wol", Call::Wol),
        ] {
            assert_eq!(parse(&words(line)), Ok(call), "{line:?}");
        }
    }

    #[test]
    fn a_call_it_cant_read_is_refused() {
        for (line, why) in [
            ("rescue", "usage: robot rescue on [KEY_FP] | off"),
            ("rescue maybe", "usage: robot rescue on [KEY_FP] | off"),
            ("rescue on aa bb", "usage: robot rescue on [KEY_FP] | off"),
            ("rescue off now", "usage: robot rescue on [KEY_FP] | off"),
            ("reset cold", "reset type: sw, hw or power"),
            ("reset hw now", USAGE),
            ("status now", USAGE),
            ("wol again", USAGE),
            ("cancel", USAGE),
            ("reinstall", USAGE),
        ] {
            assert_eq!(parse(&words(line)), Err(why), "{line:?}");
        }
    }

    #[test]
    fn a_form_value_is_escaped_as_curl_escapes_it() {
        assert_eq!(escape("linux"), "linux");
        assert_eq!(escape("aa:bb:cc"), "aa%3Abb%3Acc");
        assert_eq!(escape("a b+c/d=e&f"), "a%20b%2Bc%2Fd%3De%26f");
        assert_eq!(escape("-._~"), "-._~");
        assert_eq!(escape("é"), "%C3%A9");
    }

    #[test]
    fn picked_fields_come_in_order_and_null_when_missing() {
        let v = serde_json::json!({"b": 1, "a": {"x": [1, 2]}, "z": "s"});
        assert_eq!(
            pick(&v, &["z", "a", "missing"]),
            "{\n  \"z\": \"s\",\n  \"a\": {\n    \"x\": [\n      1,\n      2\n    ]\n  },\n  \"missing\": null\n}"
        );
        assert_eq!(pick(&Value::Null, &["a"]), "{\n  \"a\": null\n}");
        assert_eq!(pick(&v, &[]), "{}");
    }

    #[test]
    fn every_fingerprint_in_robots_list() {
        let keys = serde_json::json!([
            {"key": {"name": "laptop", "fingerprint": "aa:bb"}},
            {"key": {"name": "nameless"}},
            {"key": {"name": "remi", "fingerprint": "cc:dd"}},
        ]);
        assert_eq!(fingerprints(&keys), ["aa:bb", "cc:dd"]);
        assert!(fingerprints(&serde_json::json!({"error": {}})).is_empty());
    }
}
