//! package-lock.json: made again from nothing under the cutoff, as npm's
//! `--before` holds it, and every new release checked after.

use std::collections::HashSet;

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;

use crate::{Lock, Log, NPM, World, when};

/// (name, version, resolved) of an installed package.
type Entry = (String, String, Option<String>);

/// Every installed package in a lock file with a packages map (npm 7 on).
fn entries(lock: &Value) -> Option<Vec<Entry>> {
    let mut got = vec![];
    for (key, p) in lock.get("packages")?.as_object()? {
        // The root, workspace members and links are the repo's own, and a
        // bundled one comes inside its parent's tarball.
        let truthy = |k: &str| {
            p.get(k)
                .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
        };
        if !key.contains("node_modules/") || truthy("link") || truthy("inBundle") {
            continue;
        }
        let Some(version) = p.get("version") else {
            continue;
        };
        let name = match p.get("name").and_then(Value::as_str) {
            Some(n) if !n.is_empty() => n.to_owned(),
            _ => key
                .rsplit_once("node_modules/")
                .map_or("", |(_, n)| n)
                .to_owned(),
        };
        let version = version.as_str()?.to_owned();
        let resolved = p.get("resolved").and_then(Value::as_str).map(String::from);
        got.push((name, version, resolved));
    }
    Some(got)
}

fn entries_of(text: Option<&str>) -> Option<Vec<Entry>> {
    entries(&serde_json::from_str(text?).ok()?)
}

/// A pinned git commit, which is taken as it is, as Cargo's are.
fn git_commit(resolved: &str) -> bool {
    let Some(rest) = resolved.strip_prefix("git+") else {
        return false;
    };
    match rest.rsplit_once('#') {
        Some((url, hash)) => {
            !url.is_empty()
                && !resolved.contains(char::is_whitespace)
                && hash.len() == 40
                && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        }
        None => false,
    }
}

fn published(w: &mut dyn World, name: &str, version: &str) -> Result<Timestamp, String> {
    let times = w.npm_times(name)?;
    match times.get(version) {
        Some(t) => when(t),
        None => Err(format!(
            "the registry has no publish time for {name}@{version}"
        )),
    }
}

/// The age for the directory, the longer of the given one and npm's own
/// min-release-age, and npm's own fixed before for it, or None: --before
/// would override both, so the cutoff is the earlier of the two.
fn cutoff(
    w: &mut dyn World,
    lock: &Lock,
    age: SignedDuration,
) -> Result<(SignedDuration, Option<Timestamp>), String> {
    let out = w.run(
        &["npm", "config", "get", "before", "min-release-age"],
        &lock.home(),
    );
    if out.code != 0 {
        return Err(format!("npm config get: {}", out.stderr.trim()));
    }
    // Unset is null; an npm without the option (npm 10's min-release-age)
    // says undefined.
    let got = |k: &str| {
        out.stdout
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(_, v)| !matches!(*v, "null" | "undefined" | ""))
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.to_owned())
    };
    let mut age = age;
    if let Some(days) = got("min-release-age") {
        let days: f64 = days
            .parse()
            .map_err(|_| format!("can't read npm's min-release-age {days:?}"))?;
        let theirs = SignedDuration::try_from_secs_f64(days * 86400.0)
            .map_err(|_| format!("can't read npm's min-release-age {days:?}"))?;
        age = age.max(theirs);
    }
    let before = match got("before") {
        Some(b) => Some(js_date(&b).ok_or_else(|| format!("can't read npm's before {b:?}"))?),
        None => None,
    };
    Ok((age, before))
}

/// A JavaScript date, in the local time zone, as npm prints its before:
/// `Fri Mar 01 2024 12:30:00 GMT-0800 (Pacific Standard Time)`.
fn js_date(s: &str) -> Option<Timestamp> {
    let mut words = s.split(' ');
    let weekday = words.next()?;
    let (month, day, year, time, zone) = (
        words.next()?,
        words.next()?,
        words.next()?,
        words.next()?,
        words.next()?,
    );
    let offset = zone.strip_prefix("GMT")?;
    let ok = weekday.len() == 3
        && month.len() == 3
        && day.len() == 2
        && year.len() == 4
        && time.len() == 8
        && offset.len() == 5
        && offset.starts_with(['+', '-'])
        && offset[1..].bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return None;
    }
    jiff::fmt::strtime::parse(
        "%b %d %Y %H:%M:%S %z",
        format!("{month} {day} {year} {time} {offset}"),
    )
    .ok()?
    .to_timestamp()
    .ok()
}

/// When the first registry release the refresh brought in that's too young
/// will be old enough, or None. One published after npm's fixed before never
/// will be: a refresh with only those is a failure.
#[allow(clippy::too_many_arguments)]
fn ripe(
    w: &mut dyn World,
    lock: &Lock,
    refresh: &str,
    base: Option<&str>,
    cutoff: Timestamp,
    age: SignedDuration,
    before: Option<Timestamp>,
    bad: &mut Vec<String>,
) -> Result<Option<Timestamp>, String> {
    let had: HashSet<Entry> = match base {
        Some(b) => entries_of(Some(b))
            .unwrap_or_default()
            .into_iter()
            .collect(),
        None => HashSet::new(),
    };
    let new: Vec<Entry> = entries_of(Some(refresh))
        .unwrap_or_default()
        .into_iter()
        .filter(|x| !had.contains(x) && x.2.as_deref().is_some_and(|r| r.starts_with(NPM)))
        .collect();
    let mut young = vec![];
    for (name, version, _) in &new {
        let t = published(w, name, version)?;
        if t > cutoff {
            young.push(t);
        }
    }
    let path = lock.path;
    if young.is_empty() {
        bad.push(format!(
            "{path}: the refresh came out as the base's without anything too young in it"
        ));
        return Ok(None);
    }
    let ripe = young
        .iter()
        .filter(|t| before.is_none_or(|b| **t <= b))
        .map(|t| *t + age)
        .min();
    if ripe.is_none() {
        let before = before.map_or(String::new(), |b| b.strftime("%Y-%m-%d %H:%M").to_string());
        bad.push(format!(
            "{path}: everything too young in the refresh is past npm's own before ({before}Z), \
             and no wait ends that"
        ));
    }
    Ok(ripe)
}

/// Makes the lock file again under the cutoff. When it comes out as the
/// base's, when the first of the refresh's too young releases is old enough.
pub fn refresh(
    lock: &Lock,
    age: SignedDuration,
    w: &mut dyn World,
    bad: &mut Vec<String>,
    log: Log,
) -> Result<Option<Timestamp>, String> {
    let path = lock.path;
    let base = lock.base();
    let base = base.as_deref();
    let (age, before) = cutoff(w, lock, age)?;
    let mut cutoff = w.now() - age;
    if let Some(b) = before {
        cutoff = cutoff.min(b);
    }
    let refresh = lock.read()?;
    std::fs::remove_file(lock.file()).map_err(|e| format!("can't remove it: {e}"))?;
    let flag = format!("--before={}", cutoff.strftime("%Y-%m-%dT%H:%M:%SZ"));
    // Twice: a lock file made from nothing can come out inconsistent with
    // package.json until npm runs again over it (Renovate's npmInstallTwice).
    for _ in 0..2 {
        let argv = [
            "npm",
            "install",
            "--package-lock-only",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            &flag,
        ];
        let out = w.run(&argv, &lock.home());
        if out.code != 0 {
            let last = out.stderr.trim().lines().next_back().unwrap_or("no output");
            bad.push(format!(
                "{path}: npm couldn't resolve with only releases from before {}Z: {last}",
                cutoff.strftime("%Y-%m-%d %H:%M")
            ));
            return Ok(None);
        }
    }
    let made: Value =
        serde_json::from_str(&lock.read()?).map_err(|e| format!("can't read it: {e}"))?;
    let version = made
        .get("lockfileVersion")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if version < 2.0 || !made.get("packages").is_some_and(Value::is_object) {
        let v = made
            .get("lockfileVersion")
            .map_or("None".to_owned(), Value::to_string);
        bad.push(format!(
            "{path}: lock file version {v} has no packages map to check"
        ));
        return Ok(None);
    }
    if lock.same_as_base(base) {
        log(format!(
            "lock-age: {path}: nothing in the refresh was old enough, so it came out as the base's, \
             and Renovate would commit its own instead"
        ));
        return ripe(w, lock, &refresh, base, cutoff, age, before, bad);
    }
    // Nothing the base had counts as checked when it can't be read: all of
    // it is checked.
    let had: HashSet<Entry> = entries_of(base).unwrap_or_default().into_iter().collect();
    let made = entries(&made).ok_or("can't read its packages")?;
    for (name, version, resolved) in made {
        let from = resolved.as_deref().unwrap_or_default();
        if had.contains(&(name.clone(), version.clone(), resolved.clone())) || git_commit(from) {
            continue; // one the base had, or a pinned git commit, as Cargo's are
        }
        if !from.starts_with(NPM) {
            let from = resolved.as_deref().unwrap_or("None");
            bad.push(format!(
                "{path}: {name} {version} comes from {from}, whose publish time isn't known"
            ));
        } else if published(w, &name, &version)? > cutoff {
            bad.push(format!(
                "{path}: {name} {version} was published within the age"
            ));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{git_commit, js_date};

    #[test]
    fn reads_npms_before_in_any_zone() {
        let at = |s| js_date(s).map(|t| t.to_string());
        let utc = Some("2024-03-01T20:30:00Z".to_owned());
        assert_eq!(
            at("Fri Mar 01 2024 12:30:00 GMT-0800 (Pacific Standard Time)"),
            utc
        );
        assert_eq!(at("Sat Mar 02 2024 02:00:00 GMT+0530 (India)"), utc);
        assert_eq!(at("next tuesday"), None);
        assert_eq!(at("Fri Mar 01 2024 12:30:00 UTC"), None);
    }

    #[test]
    fn knows_a_pinned_commit() {
        let hash = "a".repeat(40);
        assert!(git_commit(&format!(
            "git+ssh://git@github.com/o/g.git#{hash}"
        )));
        assert!(!git_commit(&format!(
            "git+ssh://git@github.com/o/g.git#{}",
            &hash[1..]
        )));
        assert!(!git_commit(&format!("git+ssh://x y#{hash}")));
        assert!(!git_commit(&format!("https://x#{hash}")));
        assert!(!git_commit(&format!("git+#{hash}")));
    }
}
