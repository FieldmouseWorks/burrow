//! The build store's shadow (docs/gate-design.md, stage 1: "anything a run
//! trusts runs in shadow first"). Before a leg builds, it asks the machine's
//! build store (BUILDS, a substituter's URL, and BUILDS_KEY, its public key,
//! both from the caller's Actions variables, never an input) which of the
//! derivations it's about to build cold it has, and says so beside them:
//! how many, which, their bytes, how long fetching them would take, and how
//! long the kept builds' restore took. Nothing substitutes from it: nix
//! reads only narinfos (`nix path-info`, `nix store verify --no-contents`),
//! with a cache of its own, and one NAR's first bytes are timed for the
//! fetch. Informational, as the cold build's own line is: it never fails a
//! leg, and every question has a deadline.
//!
//! A run that may write the store also gets [`NIX_CONFIG`]: what a flake's
//! own `nixConfig` could otherwise change in a run whose builds the store
//! takes, it can't.
use super::*;
use std::collections::BTreeMap;

/// What a run that writes the store adds to every nix it runs, last, so it
/// wins: a flake's `nixConfig` is never taken (so it can't add a
/// substituter, or a key to trust, or turn the sandbox off), whatever the
/// runner's own config says, and builds are sandboxed. What such a run
/// built goes to the store under the machine's key.
pub const NIX_CONFIG: &str = "accept-flake-config = false\nsandbox = true";

/// Whether this run may write the store: its caller named one.
pub fn writes() -> bool {
    !var("BUILDS").is_empty()
}

/// How long one question to the store may take.
const DEADLINE: &str = "60";
/// The most of one NAR fetched to time the store.
const SAMPLE: u64 = 64 << 20;

/// Says what the store would have served of `cold`, the derivations this
/// leg will build: when the run names a store, and never failing.
pub fn shadow(cold: &[String]) {
    if !writes() || cold.is_empty() {
        return;
    }
    let at = Store {
        url: var("BUILDS"),
        key: var("BUILDS_KEY"),
        temp: temp(),
        nix_config: None,
    };
    let lines = match at.ask(cold) {
        Ok(r) => r.lines(),
        Err(e) => vec![format!(
            "build store (shadow, nothing substituted): couldn't tell what it has of {} cold derivations: {e}",
            cold.len()
        )],
    };
    for l in &lines {
        println!("{l}");
    }
    if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = fs::OpenOptions::new().append(true).create(true).open(path)
    {
        for l in &lines {
            let _ = writeln!(f, "{l}");
        }
    }
}

/// What the store has of a leg's cold derivations.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Cold derivations' names, the store's and the rest.
    pub served: Vec<String>,
    pub unserved: Vec<String>,
    /// What fetching the served ones would download.
    pub bytes: u64,
    /// Bytes fetched to time the store, and the seconds it took.
    pub sample: Option<(u64, f64)>,
    /// The kept builds' restore: seconds and bytes, when there was one.
    pub restore: Option<(f64, u64)>,
    /// Paths the store has whose signature BUILDS_KEY doesn't take.
    pub untrusted: usize,
}

impl Report {
    /// How long fetching what the store has would take, at the sample's
    /// rate.
    pub fn fetch_seconds(&self) -> Option<f64> {
        let (b, s) = self.sample?;
        (b > 0).then(|| self.bytes as f64 * s / b as f64)
    }

    pub fn lines(&self) -> Vec<String> {
        let n = self.served.len() + self.unserved.len();
        let mib = |b: u64| b.div_ceil(1 << 20);
        let names = |v: &[String]| {
            let mut s = v.iter().take(40).cloned().collect::<Vec<_>>().join(", ");
            if v.len() > 40 {
                s.push_str(&format!(", and {} more", v.len() - 40));
            }
            s
        };
        let mut has = format!(
            "build store (shadow, nothing substituted): it has {} of the {n} derivations this leg builds cold, {} MiB",
            self.served.len(),
            mib(self.bytes)
        );
        if !self.served.is_empty() {
            has.push_str(&format!(": {}", names(&self.served)));
        }
        if self.untrusted > 0 {
            has.push_str(&format!(
                "; {} more paths it has aren't signed by its key, and don't count",
                self.untrusted
            ));
        }
        let fetch = match (self.fetch_seconds(), self.sample) {
            _ if self.served.is_empty() => "nothing to fetch".to_owned(),
            (Some(t), Some((b, s))) => format!(
                "fetching them would take about {t:.0}s (estimated from {} MiB fetched in {s:.1}s)",
                mib(b)
            ),
            _ => "how long fetching them would take is unknown: no NAR could be timed".to_owned(),
        };
        let restore = match self.restore {
            Some((s, b)) => format!("the kept builds' restore took {s:.0}s for {} MiB", mib(b)),
            None => "no kept builds were restored".to_owned(),
        };
        let json = format!(
            "builds-shadow {{\"cold\":{n},\"served\":{},\"bytes\":{},\"fetch_seconds\":{},\"sample_bytes\":{},\"sample_seconds\":{},\"restore_seconds\":{},\"restore_bytes\":{},\"untrusted\":{},\"served_names\":{},\"unserved_names\":{}}}",
            self.served.len(),
            self.bytes,
            opt(self.fetch_seconds()),
            self.sample.map_or("null".into(), |(b, _)| b.to_string()),
            opt(self.sample.map(|(_, s)| s)),
            opt(self.restore.map(|(s, _)| s)),
            self.restore.map_or("null".into(), |(_, b)| b.to_string()),
            self.untrusted,
            list(&self.served),
            list(&self.unserved),
        );
        vec![has, format!("build store: {fetch}; {restore}"), json]
    }
}

fn opt(v: Option<f64>) -> String {
    v.map_or("null".into(), |v| format!("{v:.3}"))
}

fn list(v: &[String]) -> String {
    format!(
        "[{}]",
        v.iter().map(|s| quote(s)).collect::<Vec<_>>().join(",")
    )
}

/// A derivation's name: its path's, past the hash and before `.drv`.
pub fn name(drv: &str) -> String {
    let base = drv.rsplit('/').next().unwrap_or(drv);
    let n = base.split_once('-').map_or(base, |(_, n)| n);
    n.trim_end_matches(".drv").to_owned()
}

/// The store a run asks, and where it asks from.
pub struct Store {
    /// Its URL and public key (BUILDS and BUILDS_KEY).
    pub url: String,
    pub key: String,
    /// The runner's temp directory, where the restore's clock is.
    pub temp: PathBuf,
    /// The nix config to ask with, when not the process's.
    pub nix_config: Option<String>,
}

impl Store {
    /// nix, with this config.
    fn nix(&self, program: &str, args: &[&str]) -> Command {
        let mut c = cmd(program, args);
        if let Some(cfg) = &self.nix_config {
            c.env("NIX_CONFIG", cfg);
        }
        c.stdin(Stdio::null());
        c
    }

    /// A nix command that asks the store only, under a deadline, with a
    /// narinfo cache of its own (`work`) so what it learns stays here.
    fn asking(&self, work: &Path, args: &[&str]) -> Command {
        let mut c = self.nix("timeout", &[DEADLINE, "nix"]);
        c.args(["--extra-experimental-features", "nix-command"])
            .args(args)
            .args([
                "--option",
                "connect-timeout",
                "10",
                "--option",
                "download-attempts",
                "2",
            ])
            .env("XDG_CACHE_HOME", work);
        c
    }

    /// What the store has of `cold`.
    pub fn ask(&self, cold: &[String]) -> Result<Report> {
        let work = self.temp.join("builds-shadow");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        let r = self.asked(cold, &work);
        let _ = fs::remove_dir_all(&work);
        r
    }

    fn asked(&self, cold: &[String], work: &Path) -> Result<Report> {
        let url = self.url.trim_end_matches('/');
        let key = &self.key;
        // Each cold derivation's outputs. A fixed output's path is one
        // `nix derivation show` doesn't print; this query does.
        let mut outputs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for drv in cold {
            let o = output(self.nix("nix-store", &["--query", "--outputs", drv]))?;
            let outs = if o.status.success() {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            };
            outputs.insert(drv.clone(), outs);
        }
        // A floating content-addressed output has no path yet: nothing has it.
        let paths: Vec<&str> = outputs
            .values()
            .flatten()
            .map(String::as_str)
            .filter(|p| !p.is_empty())
            .collect();
        let mut has: BTreeMap<String, (u64, String)> = BTreeMap::new();
        if !paths.is_empty() {
            let mut args = vec!["path-info", "--store", url, "--json", "--json-format", "2"];
            args.extend(&paths);
            let o = output(self.asking(work, &args))?;
            if !o.status.success() {
                return Err(err(format!(
                    "nix path-info: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )));
            }
            let found = jq(
                &o.stdout,
                r#".info | to_entries[] | select(.value != null) | [.key, (.value.downloadSize // .value.narSize | tostring), .value.url] | join(" ")"#,
            )?;
            for l in found.lines() {
                let mut w = l.splitn(3, ' ');
                let (Some(p), Some(n), Some(u)) = (w.next(), w.next(), w.next()) else {
                    continue;
                };
                has.insert(
                    format!("{STORE}/{p}"),
                    (n.parse().unwrap_or(0), u.to_owned()),
                );
            }
        }
        // What its key doesn't sign, a store that trusts only that key wouldn't
        // take: it doesn't count.
        let mut untrusted = 0;
        if !key.is_empty() && !has.is_empty() {
            let mut args = vec![
                "store",
                "verify",
                "--store",
                url,
                "--no-contents",
                "--sigs-needed",
                "1",
                "--option",
                "trusted-public-keys",
                key,
            ];
            args.extend(has.keys().map(String::as_str));
            let o = output(self.asking(work, &args))?;
            let said = String::from_utf8_lossy(&o.stderr);
            for l in said.lines() {
                if let Some(p) = l.split('\'').nth(1).filter(|_| l.contains("is untrusted")) {
                    untrusted += usize::from(has.remove(p).is_some());
                }
            }
            if !o.status.success() && untrusted == 0 {
                return Err(err(format!("nix store verify: {}", said.trim())));
            }
        }
        let mut report = Report {
            untrusted,
            ..Report::default()
        };
        let mut nars = Vec::new();
        for (drv, outs) in &outputs {
            if !outs.is_empty() && outs.iter().all(|o| has.contains_key(o)) {
                report.served.push(name(drv));
                report.bytes += outs.iter().map(|o| has[o].0).sum::<u64>();
                nars.extend(outs.iter().map(|o| &has[o]));
            } else {
                report.unserved.push(name(drv));
            }
        }
        report.served.sort();
        report.unserved.sort();
        // The biggest NAR it would serve, its first SAMPLE bytes timed.
        let biggest = nars.into_iter().max_by_key(|(n, _)| *n);
        if let Some((_, nar)) = biggest {
            let o = output(self.nix(
                "curl",
                &[
                    "-sS",
                    "--fail",
                    "--proto",
                    "=https,http,file",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    DEADLINE,
                    "-r",
                    &format!("0-{}", SAMPLE - 1),
                    "-o",
                    "/dev/null",
                    "-w",
                    "%{size_download} %{time_total}",
                    "--",
                    &format!("{url}/{nar}"),
                ],
            ))?;
            let said = String::from_utf8_lossy(&o.stdout);
            let mut w = said.split_whitespace();
            if o.status.success()
                && let (Some(Ok(b)), Some(Ok(s))) =
                    (w.next().map(str::parse), w.next().map(str::parse))
            {
                report.sample = Some((b, s));
            }
        }
        report.restore = restore(&self.temp);
        Ok(report)
    }
}

/// The store paths are under, as every runner's nix has it.
const STORE: &str = "/nix/store";

/// The kept builds' restore, as the workflow's clock steps had it: its
/// seconds, from before the lookup to after the restore, and the bytes it
/// left in RUNNER_TEMP/kept.
fn restore(temp: &Path) -> Option<(f64, u64)> {
    let at =
        |f: &str| -> Option<u128> { fs::read_to_string(temp.join(f)).ok()?.trim().parse().ok() };
    let (start, end) = (at("builds-restore-start")?, at("builds-restore-end")?);
    let seconds = end.checked_sub(start)? as f64 / 1e9;
    let du = output(cmd("du", &["-sb", s(&temp.join("kept"))])).ok()?;
    let bytes = String::from_utf8_lossy(&du.stdout)
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((seconds, bytes))
}

#[cfg(test)]
mod tests;
