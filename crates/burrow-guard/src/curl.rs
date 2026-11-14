//! curl, read as curl reads its command line, for codex.rs and first.rs:
//! whether any request it makes is a GitHub API write the caller counts
//! ([`Counts`]).
//!
//! One curl may make many requests: each URL, `--url`'s too, and each
//! operation after `--next` (`-:`) with options of its own. Every request
//! is read with its own operation's method and body, so a harmless one
//! never speaks for another. An option's value (a header, a body) is never
//! taken for a URL. A switch is read as curl leaves it, last word winning
//! (`-G` then `--no-get` sends the body as a POST). What curl would read
//! that the guard can't (a config file, URLs from a file, a URL, method,
//! header or body only an expansion gives, the shell's or curl's own
//! `--expand-…` variables, a glob) is handed on as unknown, for the caller
//! to count as the write it may be.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use crate::codex::endpoint;
use crate::{pattern, shell};

/// Whether a write to GitHub's API counts, for the caller: given its
/// endpoint (no query, no trailing slash; None when the guard can't read
/// where it goes), its [`Body`], and whether some of the body comes from
/// where the guard can't read it.
pub type Counts<'c> = dyn Fn(Option<&str>, &Body, bool) -> bool + 'c;

/// A request's body, every way a caller may need to read it.
pub struct Body {
    /// Each piece as given, then the body curl puts them together into.
    pub pieces: Vec<String>,
    /// Each of those that's JSON, decoded ([`decode`]): what an escape
    /// hides reads as it's sent.
    pub decoded: Vec<String>,
}

impl Body {
    /// Every text of it, as given and decoded.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.pieces.iter().chain(&self.decoded).map(String::as_str)
    }
}

/// curl's short options that take a value: the rest of their word, or the
/// next word (`curl --help all`).
const SHORT_VALUE: &str = "EKCbcdDFPHhmoxUQreXYytzTuAw";

/// curl's long options that take a value, as `--name value` or
/// `--name=value` (`curl --help all`). Any other is a switch, so its
/// following word reads as a URL: a mistake there refuses, never lets
/// through.
const LONG_VALUE: &[&str] = &[
    "abstract-unix-socket",
    "alt-svc",
    "aws-sigv4",
    "cacert",
    "capath",
    "cert",
    "cert-type",
    "ciphers",
    "config",
    "connect-timeout",
    "connect-to",
    "continue-at",
    "cookie",
    "cookie-jar",
    "create-file-mode",
    "crlfile",
    "curves",
    "data",
    "data-ascii",
    "data-binary",
    "data-raw",
    "data-urlencode",
    "delegation",
    "dns-interface",
    "dns-ipv4-addr",
    "dns-ipv6-addr",
    "dns-servers",
    "doh-url",
    "dump-header",
    "ech",
    "egd-file",
    "engine",
    "etag-compare",
    "etag-save",
    "expect100-timeout",
    "form",
    "form-string",
    "ftp-account",
    "ftp-alternative-to-user",
    "ftp-method",
    "ftp-port",
    "ftp-ssl-ccc-mode",
    "happy-eyeballs-timeout-ms",
    "haproxy-clientip",
    "header",
    "help",
    "hostpubmd5",
    "hostpubsha256",
    "hsts",
    "interface",
    "ip-tos",
    "ipfs-gateway",
    "json",
    "keepalive-cnt",
    "keepalive-time",
    "key",
    "key-type",
    "knownhosts",
    "krb",
    "libcurl",
    "limit-rate",
    "local-port",
    "login-options",
    "mail-auth",
    "mail-from",
    "mail-rcpt",
    "max-filesize",
    "max-redirs",
    "max-time",
    "netrc-file",
    "noproxy",
    "oauth2-bearer",
    "output",
    "output-dir",
    "parallel-max",
    "parallel-max-host",
    "pass",
    "pinnedpubkey",
    "preproxy",
    "proto",
    "proto-default",
    "proto-redir",
    "proxy",
    "proxy-cacert",
    "proxy-capath",
    "proxy-cert",
    "proxy-cert-type",
    "proxy-ciphers",
    "proxy-crlfile",
    "proxy-header",
    "proxy-key",
    "proxy-key-type",
    "proxy-pass",
    "proxy-pinnedpubkey",
    "proxy-service-name",
    "proxy-tls13-ciphers",
    "proxy-tlsauthtype",
    "proxy-tlspassword",
    "proxy-tlsuser",
    "proxy-user",
    "proxy1.0",
    "pubkey",
    "quote",
    "random-file",
    "range",
    "rate",
    "referer",
    "request",
    "request-target",
    "resolve",
    "retry",
    "retry-delay",
    "retry-max-time",
    "sasl-authzid",
    "service-name",
    "sigalgs",
    "socks4",
    "socks4a",
    "socks5",
    "socks5-gssapi-service",
    "socks5-hostname",
    "speed-limit",
    "speed-time",
    "ssl-sessions",
    "stderr",
    "telnet-option",
    "tftp-blksize",
    "time-cond",
    "tls-max",
    "tls13-ciphers",
    "tlsauthtype",
    "tlspassword",
    "tlsuser",
    "trace",
    "trace-ascii",
    "trace-config",
    "unix-socket",
    "upload-file",
    "upload-flags",
    "url",
    "url-query",
    "user",
    "user-agent",
    "variable",
    "vlan-priority",
    "write-out",
];

/// What an option's value is, to the guard, once curl expands variables
/// in it (`--expand-NAME` with `{{var}}`): unknown, whatever globbing.
const EXPANDED: &str = "\u{FFFC}";

/// One request's worth of options: an operation, up to `--next`.
#[derive(Default)]
struct Operation<'a> {
    urls: Vec<&'a str>,
    method: Option<&'a str>,
    /// `--request-target`: the path sent, whatever the URL's.
    target: Option<&'a str>,
    body: Vec<&'a str>,
    /// The body curl sends, its `--data` family's and `--json`'s pieces
    /// put together as curl puts them: each after the last, with `&`
    /// between unless it's `--json`'s (`set_data` in curl's
    /// tool_getparam.c).
    data: String,
    /// A body from a file or input, or built from variables.
    body_unread: bool,
    upload: bool,
    get: bool,
    head: bool,
    globoff: bool,
    /// It connects somewhere other than its URL's host names
    /// (`--connect-to`, `--resolve`, a `Host` header).
    rerouted: bool,
}

/// Whether a value comes from where the guard can't read it: a file or
/// input (`@file`, `name=@file`, `name=<file`), or an expansion.
fn unread(value: &str) -> bool {
    value.starts_with('@')
        || value.contains("=@")
        || value.contains("=<")
        || value.contains(['$', shell::UNKNOWN])
}

impl<'a> Operation<'a> {
    fn take(&mut self, name: &str, value: &'a str) {
        match name {
            "url" => self.urls.push(value),
            "request" => self.method = Some(value),
            "request-target" => self.target = Some(value),
            "data" | "data-ascii" | "data-binary" | "data-raw" | "json" | "form"
            | "form-string" => {
                self.body_unread |= name != "data-raw" && name != "form-string" && unread(value)
                    || value.contains(['$', shell::UNKNOWN]);
                self.body.push(value);
                if !name.starts_with("form") {
                    self.append(value, name == "json");
                }
            }
            "data-urlencode" => {
                self.body_unread |= value.contains(['@', '$', shell::UNKNOWN]);
                self.body.push(value);
                // Encoded, the text it names reads no differently here.
                self.append(value, false);
            }
            "upload-file" => self.upload = true,
            "connect-to" | "resolve" => self.rerouted = true,
            "header" => {
                // A Host header, or one whose name the guard can't read.
                let name = value.split_once(':').map_or(value, |(n, _)| n);
                self.rerouted |= name.trim().eq_ignore_ascii_case("host")
                    || !value.contains(':')
                    || name.contains(['$', shell::UNKNOWN])
                    || value.starts_with('@');
            }
            _ => {}
        }
    }

    /// A piece of the body, onto what's there.
    fn append(&mut self, piece: &str, json: bool) {
        if !self.data.is_empty() && !json {
            self.data.push('&');
        }
        self.data.push_str(piece);
    }

    /// Its [`Body`].
    fn body(&self) -> Body {
        let mut pieces: Vec<String> = self.body.iter().map(|b| (*b).to_owned()).collect();
        if !self.data.is_empty() {
            pieces.push(self.data.clone());
        }
        let decoded = pieces.iter().filter_map(|b| decode(b)).collect();
        Body { pieces, decoded }
    }

    /// A boolean switch, on as `--name` (or its short letter), off as
    /// `--no-name`: the last one given wins.
    fn switch(&mut self, name: &str) {
        let (name, on) = name
            .strip_prefix("no-")
            .map_or((name, true), |n| (n, false));
        match name {
            "get" => self.get = on,
            "head" => self.head = on,
            "globoff" => self.globoff = on,
            _ => {}
        }
    }

    fn method(&self) -> &str {
        self.method.unwrap_or(if self.head {
            "HEAD"
        } else if self.get {
            "GET"
        } else if self.upload {
            "PUT"
        } else if self.body.is_empty() {
            "GET"
        } else {
            "POST"
        })
    }

    /// Whether this operation's requests make a write on GitHub that
    /// `counts`.
    fn writes(&self, unread_everywhere: bool, counts: &Counts<'_>) -> bool {
        let method = self.method();
        let write = !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD");
        if !write {
            return false;
        }
        let unread_body = self.body_unread || self.upload || unread_everywhere;
        let bodies = self.body();
        let unknown = || counts(None, &bodies, unread_body);
        let mut paths: Vec<String> = Vec::new();
        for url in &self.urls {
            // Where an expansion or a glob may send it, or a file of URLs.
            if url.contains(['$', shell::UNKNOWN])
                || url.starts_with('@')
                || (!self.globoff && url.contains(['{', '[']))
            {
                return unknown();
            }
            let (host, path) = split(url);
            if host == "api.github.com"
                || path.starts_with("/api/v3/")
                || path == "/api/graphql"
                || self.rerouted
            {
                paths.push(path);
            }
        }
        if paths.is_empty() {
            return false;
        }
        if let Some(target) = self.target {
            if target.contains(['$', shell::UNKNOWN]) {
                return unknown();
            }
            paths = vec![squash(target)];
        }
        paths
            .iter()
            .any(|path| counts(Some(endpoint(path)), &bodies, unread_body))
    }
}

/// A JSON body decoded: its keys and strings, each on a line of its own.
/// None for one that isn't JSON.
pub fn decode(body: &str) -> Option<String> {
    fn strings(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
            serde_json::Value::Object(o) => {
                for (k, v) in o {
                    out.push_str(k);
                    out.push('\n');
                    strings(v, out);
                }
            }
            _ => {}
        }
    }
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let mut out = String::new();
    strings(&value, &mut out);
    Some(out)
}

/// A URL's host, lowercased, without its user or port, and its path, dot
/// segments squashed as curl does: what the request is sent to.
fn split(url: &str) -> (String, String) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (authority, path) = rest
        .find(['/', '?', '#'])
        .map_or((rest, "/"), |at| rest.split_at(at));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    (host, squash(path))
}

/// A path with its `.` and `..` segments resolved, before its query.
fn squash(path: &str) -> String {
    let (path, query) = path
        .find(['?', '#'])
        .map_or((path, ""), |at| path.split_at(at));
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    for (i, segment) in segments.into_iter().enumerate() {
        match segment {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("{}{query}", out.join("/"))
}

/// What curl with some words does, as far as its caller cares.
#[derive(Debug, PartialEq, Eq)]
pub enum Curl {
    /// No write that counts.
    Fine,
    /// A write through GitHub's API that counts, or may: a request whose
    /// destination or body the guard can't read, as the caller counts it.
    Writes,
    /// Reads options from a file (`--config`), which may be any of them.
    Config,
}

/// What curl with these `words` does: each of its requests read with its
/// own operation's options, and its writes to GitHub's API as `counts`
/// counts them.
pub fn read(words: &[&str], counts: &Counts<'_>) -> Curl {
    let mut ops = vec![Operation::default()];
    // Variables (`--variable`, `--expand-…`) build values the guard can't
    // read, in any operation.
    let mut variables = false;
    let mut words = words.iter().copied();
    let mut options_end = false;
    while let Some(w) = words.next() {
        let op = ops.last_mut().expect("there's always one");
        if options_end || !w.starts_with('-') || w == "-" {
            op.urls.push(w);
            continue;
        }
        if w == "--" {
            options_end = true;
            continue;
        }
        if let Some(long) = w.strip_prefix("--") {
            let (name, joined) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            if name == "next" {
                ops.push(Operation::default());
                continue;
            }
            variables |= name == "variable" || name.starts_with("expand-");
            let expands = name.starts_with("expand-");
            let name = name.strip_prefix("expand-").unwrap_or(name);
            if name == "config" {
                return Curl::Config;
            }
            if LONG_VALUE.contains(&name) {
                if let Some(value) = joined.or_else(|| words.next()) {
                    // What curl's variables make of it is unknown here.
                    let value = if expands && value.contains("{{") {
                        EXPANDED
                    } else {
                        value
                    };
                    op.take(name, value);
                }
            } else {
                op.switch(name);
            }
            continue;
        }
        // Short options, run together: one that takes a value takes the
        // rest of the word, or the next word.
        let short = &w[1..];
        for (i, c) in short.char_indices() {
            if c == ':' {
                ops.push(Operation::default());
                continue;
            }
            let op = ops.last_mut().expect("there's always one");
            if SHORT_VALUE.contains(c) {
                let rest = &short[i + c.len_utf8()..];
                let value = if rest.is_empty() {
                    words.next()
                } else {
                    Some(rest)
                };
                if c == 'K' {
                    return Curl::Config;
                }
                if let Some(value) = value {
                    op.take(long_name(c), value);
                }
                break;
            }
            op.switch(match c {
                'G' => "get",
                'I' => "head",
                'g' => "globoff",
                _ => "",
            });
        }
    }
    if ops.iter().any(|op| op.writes(variables, counts)) {
        Curl::Writes
    } else {
        Curl::Fine
    }
}

/// The long name of a short option that takes a value, where it matters.
fn long_name(c: char) -> &'static str {
    match c {
        'd' => "data",
        'F' => "form",
        'H' => "header",
        'X' => "request",
        'T' => "upload-file",
        _ => "",
    }
}

/// Whether curl, given `words` on the command line `cmd`, reads a default
/// config the guard can't read: one is there where curl looks
/// ([`default_config`]), or the line moves where it looks
/// ([`moves_default_config`]), and its first word doesn't keep it off
/// them ([`skips_default_config`]).
pub fn reads_default_config(words: &[&str], cmd: &str) -> bool {
    !skips_default_config(words.first().copied())
        && (moves_default_config(cmd) || default_config(|k| std::env::var_os(k)))
}

/// Whether curl's first word keeps it off its default config (`-q`).
pub fn skips_default_config(first: Option<&str>) -> bool {
    matches!(first, Some("-q" | "--disable"))
}

/// What a command line sets that moves curl's default config: `CURL_HOME`,
/// `XDG_CONFIG_HOME`, or `HOME` named as itself (an assignment, `export`,
/// `read`), not expanded.
static MOVES_CONFIG: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"CURL_HOME|XDG_CONFIG_HOME|(^|[^A-Za-z0-9_${])HOME([^A-Za-z0-9_]|$)")
});

pub fn moves_default_config(cmd: &str) -> bool {
    MOVES_CONFIG.is_match(cmd)
}

/// Whether a default curl config is there to read, in `env` (curl's
/// `.curlrc` or `curlrc` under `CURL_HOME`, `XDG_CONFIG_HOME` or `HOME`):
/// what it holds may add requests or options to any curl.
pub fn default_config(env: impl Fn(&str) -> Option<OsString>) -> bool {
    let mut places: Vec<PathBuf> = Vec::new();
    for var in ["CURL_HOME", "XDG_CONFIG_HOME", "HOME"] {
        if let Some(dir) = env(var).filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            places.push(dir.join(".config"));
            places.push(dir);
        }
    }
    places
        .iter()
        .any(|dir| ["curlrc", ".curlrc"].iter().any(|f| dir.join(f).exists()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_config_anywhere_curl_looks_counts() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().to_owned();
        let env = |var: &'static str| {
            let at = at.clone();
            move |k: &str| (k == var).then(|| at.clone().into_os_string())
        };
        for var in ["CURL_HOME", "XDG_CONFIG_HOME", "HOME"] {
            assert!(!default_config(env(var)), "{var}");
        }
        for file in [".curlrc", "curlrc", ".config/curlrc"] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "request = PATCH\n").unwrap();
            for var in ["CURL_HOME", "XDG_CONFIG_HOME", "HOME"] {
                assert!(default_config(env(var)), "{var}/{file}");
            }
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn paths_are_squashed_as_curl_sends_them() {
        assert_eq!(
            split("https://u:p@API.github.com:443/repos/o/r/pulls/5/x/../merge?a=1"),
            (
                "api.github.com".to_owned(),
                "/repos/o/r/pulls/5/merge?a=1".to_owned()
            )
        );
        assert_eq!(split("api.github.com").1, "/");
        assert_eq!(squash("/a/./b/../c/."), "/a/c/");
    }
}
