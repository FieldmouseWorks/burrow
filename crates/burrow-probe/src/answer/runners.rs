//! `runners [RUNNER]`: each of the machine's Actions runners, named ones
//! (burrow.dispatch.runners) and the template of the ones started fresh for
//! one job (burrow.dispatch.fresh), told three ways and never mixed up:
//!
//! - configured: what the deployed configuration says of its unit
//!   (modules/probe.nix writes it): user, HOME, PATH, the packages its
//!   `path` adds, the rustup settings in its environment;
//! - loaded: what systemd has loaded now (`systemctl show`): its state and
//!   the same environment keys, which a runner a switch never restarts can
//!   have older than the configuration;
//! - found: what's on its PATH now: each directory's commands and the
//!   store packages they come from, which toolchain commands are there, and
//!   the rustup default toolchain its HOME holds.
//!
//! Nothing a runner has is run: a command is found by its name in a PATH
//! directory, its package by where its link points, and the default
//! toolchain by reading rustup's settings.toml, each through shown.rs, so no
//! path the reviewers never read is listed, resolved or read. A fact the
//! probe can't read is said to be unknown, and why.

use std::collections::{BTreeMap, BTreeSet};

use super::Machine;
use super::shown::Unread;
use crate::{Runner, RunnerKind};

/// The toolchain commands every answer looks for.
const RUST: [&str; 3] = ["rustc", "cargo", "rustup"];
/// The rest a runner's answer in full looks for.
const OTHERS: [&str; 11] = [
    "git", "nix", "gh", "jq", "curl", "node", "python3", "java", "go", "cc", "heavy",
];
/// The environment keys told, configured and loaded.
const KEYS: [&str; 5] = [
    "PATH",
    "HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_HOME",
];
/// What systemd runs a unit with when it sets no PATH.
const SYSTEMD_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin";
/// The most fresh instances whose loaded units are read.
const MAX_INSTANCES: usize = 3;
/// The most packages named for one PATH directory, in full.
const MAX_PACKAGES: usize = 40;
/// The largest rustup settings.toml read.
const MAX_SETTINGS: u64 = 64 * 1024;

/// What `systemctl show` says of one loaded unit.
#[derive(Debug, Default, Clone)]
struct Loaded {
    id: String,
    state: String,
    since: String,
    user: String,
    dynamic_user: bool,
    stale: bool,
    environment: BTreeMap<String, String>,
}

pub fn answer(m: &Machine, name: Option<&str>) -> Result<String, String> {
    let all = &m.cfg.runners;
    if all.is_empty() {
        return Ok(
            "No runners are configured here (burrow.dispatch.runners, burrow.dispatch.fresh).\n"
                .into(),
        );
    }
    let chosen: Vec<&Runner> = match name {
        None => all.iter().collect(),
        Some(n) => match all.iter().find(|r| r.name == n) {
            Some(r) => vec![r],
            None => {
                return Err(format!(
                    "{n:?} isn't one of this machine's runners: {}",
                    all.iter()
                        .map(|r| r.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        },
    };
    let full = name.is_some();
    let mut out = String::new();
    for r in chosen {
        out += &runner(m, r, full);
        out.push('\n');
    }
    out += "Configured: the deployed configuration's unit. Loaded: the unit systemd has now; a running runner keeps the environment it started with. Found: PATH's directories as they are now, read without running anything. A checkout's rust-toolchain.toml or a job's own setup can still choose another toolchain.\n";
    if !full {
        out += "Ask `runners RUNNER` for one runner's packages and commands in full.\n";
    }
    Ok(out)
}

fn runner(m: &Machine, r: &Runner, full: bool) -> String {
    let mut out = match r.kind {
        RunnerKind::Named => format!(
            "{}: a named runner for {} (labels: {})\n",
            r.name,
            r.repos.join(", "),
            r.labels.join(", ")
        ),
        RunnerKind::Fresh => format!(
            "{}INSTANCE: runners started fresh for one job each, for {} (label: {})\n",
            r.name,
            if r.repos.is_empty() {
                "no repos".to_string()
            } else {
                r.repos.join(", ")
            },
            r.labels.join(", ")
        ),
    };
    if !r.defined {
        out += "  configured: this system's configuration defines no such unit\n";
    }
    let configured_path = r.environment.get("PATH").cloned();
    out += &format!(
        "  configured: user {}, HOME {}, PATH {}\n",
        match (&r.user, r.dynamic_user) {
            (_, true) => "a dynamic user of its own".to_string(),
            (Some(u), false) => u.clone(),
            (None, false) => "root (none named)".to_string(),
        },
        r.environment
            .get("HOME")
            .or(r.home.as_ref())
            .map_or("unknown".to_string(), String::clone),
        configured_path
            .as_deref()
            .map_or(format!("none set (systemd's {SYSTEMD_PATH})"), |p| short(
                p, full
            )),
    );
    if !r.packages.is_empty() {
        out += &format!("  configured packages: {}\n", r.packages.join(", "));
    }
    for key in ["RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME"] {
        if let Some(v) = r.environment.get(key) {
            out += &format!("  configured {key}={v}\n");
        }
    }

    // What systemd has loaded: the unit, or each fresh instance there is.
    let loaded = loaded(m, r);
    let first = match &loaded {
        Ok((units, _)) if units.is_empty() => {
            out += &match r.kind {
                RunnerKind::Fresh => {
                    "  loaded: no instance is loaded now, so only its configuration says\n"
                        .to_string()
                }
                RunnerKind::Named => {
                    format!("  loaded: systemd says nothing of {}.service\n", r.name)
                }
            };
            None
        }
        Ok((units, omitted)) => {
            if *omitted > 0 {
                out += &format!(
                    "  loaded: {omitted} more instance{} not inspected (at most {MAX_INSTANCES} are read); the found facts below describe {} only\n",
                    if *omitted == 1 { "" } else { "s" },
                    units.first().map_or("none", |u| u.id.as_str())
                );
            }
            for u in units {
                out += &format!(
                    "  loaded: {} {}{}{}{}\n",
                    u.id,
                    u.state,
                    if u.since.is_empty() {
                        String::new()
                    } else {
                        format!(" since {}", u.since)
                    },
                    if u.dynamic_user {
                        ", a dynamic user".to_string()
                    } else if u.user.is_empty() {
                        String::new()
                    } else {
                        format!(", user {}", u.user)
                    },
                    if u.stale {
                        "; its unit file changed since systemd loaded it"
                    } else {
                        ""
                    }
                );
                for key in KEYS {
                    let now = u.environment.get(key);
                    let then = r.environment.get(key);
                    match (now, then) {
                        (Some(a), Some(b)) if a == b => {}
                        (Some(a), _) => out += &format!("    loaded {key}={}\n", short(a, full)),
                        (None, Some(_)) => {
                            out += &format!("    loaded: no {key}, though one is configured\n")
                        }
                        (None, None) => {}
                    }
                }
            }
            units.first().cloned()
        }
        Err(e) => {
            out += &format!("  loaded: unknown: {e}\n");
            None
        }
    };

    // The environment found facts are read in: the loaded unit's when
    // there is one, as it is (a key it lacks is absent, whatever the newer
    // configuration says), else the configuration's. The implicit HOME
    // (the user's own) is only the configured user's.
    let (env, home) = match &first {
        Some(u) => {
            out += &format!("  found: read in the environment loaded for {}\n", u.id);
            let same_user =
                !u.dynamic_user && !r.dynamic_user && u.user == r.user.clone().unwrap_or_default();
            (
                u.environment.clone(),
                same_user.then(|| r.home.clone()).flatten(),
            )
        }
        None => (r.environment.clone(), r.home.clone()),
    };
    let path = env
        .get("PATH")
        .cloned()
        .unwrap_or_else(|| SYSTEMD_PATH.to_string());
    let dirs: Vec<&str> = path.split(':').collect();

    // Each PATH directory as it is.
    let mut there = 0;
    let mut commands = 0;
    let mut missing = Vec::new();
    let mut detail = String::new();
    for dir in &dirs {
        if !dir.starts_with('/') {
            missing.push(format!(
                "{} (not absolute: depends on a working directory, never searched by these lines)",
                if dir.is_empty() {
                    "an empty entry"
                } else {
                    dir
                }
            ));
            continue;
        }
        match m.list(dir) {
            Ok(listing) => {
                there += 1;
                commands += listing.entries.len();
                if full {
                    let packages = packages(dir, &listing);
                    let named: Vec<&str> = packages
                        .iter()
                        .take(MAX_PACKAGES)
                        .map(String::as_str)
                        .collect();
                    detail += &format!(
                        "    {dir}: {} commands from {}{}{}\n",
                        listing.entries.len(),
                        if named.is_empty() {
                            "no store package".to_string()
                        } else {
                            named.join(", ")
                        },
                        if packages.len() > MAX_PACKAGES {
                            format!(" and {} more", packages.len() - MAX_PACKAGES)
                        } else {
                            String::new()
                        },
                        left_out(&listing)
                    );
                }
            }
            Err(why) => missing.push(format!("{} ({})", short(dir, full), why.say())),
        }
    }
    out += &format!(
        "  found: {there} of {} PATH directories, {commands} commands{}\n",
        dirs.len(),
        if missing.is_empty() {
            String::new()
        } else {
            format!("; not read: {}", missing.join(", "))
        }
    );
    out += &detail;

    // The toolchain, by name on PATH.
    let found: BTreeMap<&str, Option<Result<Found, String>>> = RUST
        .iter()
        .chain(if full { OTHERS.as_slice() } else { &[] })
        .map(|t| (*t, find(m, &dirs, t)))
        .collect();
    let say = |t: &str| match &found[t] {
        Some(Ok(f)) => format!("{t} {}", f.say()),
        Some(Err(why)) => format!("{t} {why}"),
        None => format!("{t} not on PATH"),
    };
    out += &format!(
        "  found rust: {}; default toolchain: {}\n",
        RUST.iter().map(|t| say(t)).collect::<Vec<_>>().join(", "),
        default_toolchain(m, home.as_ref(), &env, &found)
    );
    if full {
        out += &format!(
            "  found tools: {}\n",
            OTHERS.iter().map(|t| say(t)).collect::<Vec<_>>().join(", ")
        );
    }
    out
}

/// What a listing left out, said.
fn left_out(l: &super::shown::Listing) -> String {
    let mut out = String::new();
    if l.hidden > 0 {
        out += &format!("; {} left out as hidden", l.hidden);
    }
    if l.unread > 0 {
        out += &format!(
            "; {} left out as unknown: the probe could not tell",
            l.unread
        );
    }
    out
}

/// The units systemd has loaded for `r`: the named one, or each fresh
/// instance there is (at most MAX_INSTANCES).
fn loaded(m: &Machine, r: &Runner) -> Result<(Vec<Loaded>, usize), String> {
    let ctl = &m.cfg.tools.systemctl;
    let mut omitted = 0;
    let units: Vec<String> = match r.kind {
        RunnerKind::Named => vec![format!("{}.service", r.name)],
        RunnerKind::Fresh => {
            let pattern = format!("{}*.service", r.name);
            let listed = m.output(
                ctl,
                &[
                    "list-units",
                    "--all",
                    "--plain",
                    "--no-legend",
                    "--no-pager",
                    "--full",
                    "--",
                    &pattern,
                ],
                false,
                &[],
            )?;
            let all: Vec<&str> = listed
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .filter(|u| u.starts_with(&r.name) && u.ends_with(".service"))
                .collect();
            omitted = all.len().saturating_sub(MAX_INSTANCES);
            all.into_iter()
                .take(MAX_INSTANCES)
                .map(str::to_owned)
                .collect()
        }
    };
    if units.is_empty() {
        return Ok((vec![], 0));
    }
    let mut args = vec![
        "show",
        "--no-pager",
        "--property=Id,LoadState,ActiveState,SubState,ActiveEnterTimestamp,User,DynamicUser,NeedDaemonReload,Environment",
        "--",
    ];
    args.extend(units.iter().map(String::as_str));
    let text = m.output(ctl, &args, false, &[])?;
    let shown = text
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(show)
        .filter(|u| !u.id.is_empty())
        .collect();
    Ok((shown, omitted))
}

/// One unit's block of `systemctl show`.
fn show(block: &str) -> Loaded {
    let mut u = Loaded::default();
    let mut load = String::new();
    let mut active = String::new();
    let mut sub = String::new();
    for line in block.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "Id" => u.id = v.to_owned(),
            "LoadState" => load = v.to_owned(),
            "ActiveState" => active = v.to_owned(),
            "SubState" => sub = v.to_owned(),
            "ActiveEnterTimestamp" => u.since = v.to_owned(),
            "User" => u.user = v.to_owned(),
            "DynamicUser" => u.dynamic_user = v == "yes",
            "NeedDaemonReload" => u.stale = v == "yes",
            "Environment" => {
                u.environment = words(v)
                    .into_iter()
                    .filter_map(|w| {
                        let (k, v) = w.split_once('=')?;
                        KEYS.contains(&k).then(|| (k.to_owned(), v.to_owned()))
                    })
                    .collect()
            }
            _ => {}
        }
    }
    u.state = if load == "loaded" || load.is_empty() {
        format!("{active}/{sub}")
    } else {
        format!("{load}, {active}/{sub}")
    };
    u
}

/// `systemctl show`'s Environment= value split into its assignments: by
/// spaces outside double quotes, with backslash escapes.
fn words(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    word.push(n);
                    any = true;
                }
            }
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

/// A command found on PATH.
#[derive(Debug, Clone)]
struct Found {
    /// The PATH directory it was first found in.
    dir: String,
    /// The store package it comes from, if it does.
    package: Option<String>,
}

impl Found {
    fn say(&self) -> String {
        format!(
            "in {} ({})",
            self.dir,
            self.package.as_deref().unwrap_or("not from the store")
        )
    }

    fn rustup(&self) -> bool {
        self.package
            .as_deref()
            .is_some_and(|p| p.starts_with("rustup"))
    }
}

/// The first PATH directory with a command named `name`, as PATH finds it:
/// None for none. A directory that isn't there is passed; one the probe
/// couldn't search (hidden, unreadable, a relative or empty entry) leaves
/// the answer unknown, whether or not a later directory has the command,
/// since an earlier one it couldn't search may. Err says so, or where the
/// first command there is is, or leads to, a path the reviewers never read.
fn find(m: &Machine, dirs: &[&str], name: &str) -> Option<Result<Found, String>> {
    let mut unsearched: Vec<String> = Vec::new();
    for dir in dirs {
        if !dir.starts_with('/') {
            unsearched.push(if dir.is_empty() {
                "an empty entry".to_owned()
            } else {
                short(dir, false)
            });
            continue;
        }
        match m.entry(dir, name) {
            Ok(None) | Err(Unread::Missing) => continue,
            Ok(Some(entry)) => {
                let package = entry
                    .link
                    .as_deref()
                    .and_then(store_name)
                    .or_else(|| m.resolve(dir).ok().as_deref().and_then(store_name));
                let found = Found {
                    dir: short(dir, false),
                    package,
                };
                return Some(if unsearched.is_empty() {
                    Ok(found)
                } else {
                    Err(format!(
                        "unknown: {}, unless an earlier PATH directory holds one, which the probe could not search: {}",
                        found.say(),
                        unsearched.join(", ")
                    ))
                });
            }
            Err(Unread::Hidden) => {
                unsearched.push(format!("{} ({})", short(dir, false), Unread::Hidden.say()));
            }
            Err(why) if matches!(why, Unread::Denied | Unread::Other(_)) => {
                unsearched.push(format!("{} ({})", short(dir, false), why.say()));
            }
            Err(_) => continue,
        }
    }
    (!unsearched.is_empty()).then(|| {
        Err(format!(
            "unknown: not found in the directories searched; the probe could not search {}",
            unsearched.join(", ")
        ))
    })
}

/// What decides the default toolchain `rustc` and `cargo` get, as far as
/// the runner's environment and HOME say.
fn default_toolchain(
    m: &Machine,
    home: Option<&String>,
    env: &BTreeMap<String, String>,
    found: &BTreeMap<&str, Option<Result<Found, String>>>,
) -> String {
    if let Some(t) = env.get("RUSTUP_TOOLCHAIN") {
        return format!("{t} (RUSTUP_TOOLCHAIN in its environment)");
    }
    let unknown = RUST.iter().any(|t| matches!(found[t], Some(Err(_))));
    let rustup = matches!(found["rustup"], Some(Ok(_)))
        || ["rustc", "cargo"]
            .iter()
            .any(|t| matches!(&found[t], Some(Ok(f)) if f.rustup()));
    if !rustup {
        return match &found["rustc"] {
            _ if unknown => "unknown: a toolchain command on PATH isn't read".into(),
            Some(Ok(f)) => format!(
                "rustc's own ({}): no rustup on PATH",
                f.package.as_deref().unwrap_or("not from the store")
            ),
            _ => "none: neither rustc nor rustup is on PATH".into(),
        };
    }
    let home = env.get("HOME").or(home);
    let settings = match (env.get("RUSTUP_HOME"), home) {
        (Some(h), _) => format!("{}/settings.toml", h.trim_end_matches('/')),
        (None, Some(h)) => format!("{}/.rustup/settings.toml", h.trim_end_matches('/')),
        (None, None) => return "unknown: neither RUSTUP_HOME nor HOME is known".into(),
    };
    if settings.contains('%') {
        return format!(
            "none until a job sets one: {settings} is under each instance's own state, made empty as it starts"
        );
    }
    match m.read_small(&settings, MAX_SETTINGS) {
        Ok(text) => match setting(&text, "default_toolchain") {
            Some(t) => format!("{t} ({settings})"),
            None => format!("none: {settings} names no default_toolchain"),
        },
        Err(Unread::Missing) => format!("none: {settings} isn't there"),
        Err(why) => format!("{settings}: {}", why.say()),
    }
}

/// A top-level `key = "value"` of a TOML file, as rustup writes it.
fn setting(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Tables come after the top-level keys.
            return None;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            return (!v.is_empty() && v.len() <= 200).then(|| v.to_owned());
        }
    }
    None
}

/// The store packages a PATH directory's commands come from: where each
/// link points, or the directory's own store path for its files.
fn packages(dir: &str, listing: &super::shown::Listing) -> BTreeSet<String> {
    let own = store_name(&listing.real).or_else(|| store_name(dir));
    listing
        .entries
        .iter()
        .filter_map(|e| match &e.link {
            Some(t) => store_name(t),
            None => own.clone(),
        })
        .collect()
}

/// NAME of a path under `/nix/store/HASH-NAME`.
pub(super) fn store_name(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/nix/store/")?;
    let first = rest.split('/').next()?;
    let (hash, name) = (first.get(..32)?, first.get(32..)?);
    let name = name.strip_prefix('-')?;
    (hash.bytes().all(|b| b.is_ascii_alphanumeric()) && !name.is_empty()).then(|| name.to_owned())
}

/// A PATH, or one directory of it, with each store hash cut out unless in
/// full: `/nix/store/…-jq-1.7.1-bin/bin`.
fn short(path: &str, full: bool) -> String {
    if full {
        return path.to_owned();
    }
    path.split(':')
        .map(|d| match d.strip_prefix("/nix/store/") {
            Some(rest) if rest.len() > 33 && rest.as_bytes()[32] == b'-' => {
                format!("/nix/store/…{}", &rest[32..])
            }
            _ => d.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::{setting, store_name, words};

    #[test]
    fn store_names() {
        let h = "a".repeat(32);
        assert_eq!(
            store_name(&format!("/nix/store/{h}-rustup-1.28.2/bin/rustup")).as_deref(),
            Some("rustup-1.28.2")
        );
        assert_eq!(store_name("/usr/bin/rustc"), None);
        assert_eq!(store_name("/nix/store/short-x/bin"), None);
    }

    #[test]
    fn environment_words() {
        assert_eq!(
            words(r#"PATH=/a:/b HOME=/var/lib/ci "X=has space" Y=a\"b"#),
            vec!["PATH=/a:/b", "HOME=/var/lib/ci", "X=has space", "Y=a\"b"]
        );
        assert!(words("").is_empty());
    }

    #[test]
    fn rustup_settings() {
        let text = "default_host_triple = \"x86_64-unknown-linux-gnu\"\ndefault_toolchain = \"stable-x86_64-unknown-linux-gnu\"\nprofile = \"minimal\"\nversion = \"12\"\n\n[overrides]\n";
        assert_eq!(
            setting(text, "default_toolchain").as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            setting(
                "version = \"12\"\n[overrides]\ndefault_toolchain = \"x\"\n",
                "default_toolchain"
            ),
            None
        );
    }
}
