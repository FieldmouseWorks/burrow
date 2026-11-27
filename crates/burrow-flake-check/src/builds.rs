//! The build store's shadow (docs/gate-design.md, stage 1: "anything a run
//! trusts runs in shadow first"). Before a leg builds, it asks the machine's
//! build store (BUILDS, a substituter's URL, and BUILDS_KEY, its public key,
//! both from the caller's Actions variables, never an input) which of the
//! derivations it's about to build cold it has, and says so beside them:
//! how many, which, their bytes, how long fetching them would take, and how
//! long the kept builds' restore took. Nothing substitutes from it: curl
//! reads the outputs' narinfos concurrently by hash, nix checks those
//! fetched narinfos' signatures offline (`nix store verify --no-contents`),
//! and one NAR's first bytes are timed for the fetch. Informational, as the
//! cold build's own line is: it never fails a leg, and every question has a
//! deadline.
//!
//! A run that may write the store also gets [`NIX_CONFIG`]: what a flake's
//! own `nixConfig` could otherwise change in a run whose builds the store
//! takes, it can't.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

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
/// How many narinfo requests curl keeps in flight at once.
const PARALLEL: &str = "32";
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
        deadline: DEADLINE,
    };
    let mut lines = Vec::new();
    if at.key.is_empty() {
        lines.push(
            "build store: no public key is named for it (BURROW_BUILDS_PUBLIC_KEY), so only what needs no signature counts".to_owned(),
        );
    }
    lines.extend(match at.ask(cold) {
        Ok(r) => r.lines(),
        Err(e) => vec![format!(
            "build store (shadow, nothing substituted): couldn't tell what it has of {} cold derivations: {e}",
            cold.len()
        )],
    });
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
    /// How long one question to it may take, in seconds: [`DEADLINE`].
    pub deadline: &'static str,
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
        let mut c = self.nix("timeout", &[self.deadline, "nix"]);
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

    /// The store's narinfos for `paths`, by their hashes: each path it
    /// has, with its download's bytes and its NAR's URL, staged in `cache`
    /// as a local binary cache so signature verification never fetches them
    /// again. Read concurrently with curl (bounded by [`PARALLEL`]), never
    /// nix: given a path its store hasn't, nix tries to build it, and an
    /// HTTP store (nix 2.35) then fails the whole question. A 404 is a path
    /// it hasn't; any other answer is an error.
    fn narinfos(
        &self,
        url: &str,
        paths: &[&str],
        work: &Path,
        cache: &Path,
    ) -> Result<BTreeMap<String, (u64, String)>> {
        let mut has = BTreeMap::new();
        if paths.is_empty() {
            return Ok(has);
        }
        let dir = work.join("narinfo");
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(cache)?;
        let config = work.join("narinfo.curl");
        let mut cfg = String::new();
        for (i, p) in paths.iter().enumerate() {
            let hash = hash_part(p);
            cfg.push_str("url = ");
            cfg.push_str(&curl_quote(&format!("{url}/{hash}.narinfo"))?);
            cfg.push_str("\noutput = ");
            cfg.push_str(&curl_quote(s(&dir.join(i.to_string())))?);
            cfg.push('\n');
        }
        fs::write(&config, cfg)?;
        // One deadline for the whole batch: curl's own `--max-time` is each
        // transfer's, so a stalled store would hold a leg once per output.
        let c = self.nix(
            "timeout",
            &[
                self.deadline,
                "curl",
                "-sS",
                "--parallel",
                "--parallel-max",
                PARALLEL,
                "--proto",
                "=https,http",
                "--connect-timeout",
                "10",
                "-w",
                "%{urlnum} %{response_code}\\n",
                "-K",
                s(&config),
            ],
        );
        let o = output(c)?;
        if o.status.code() == Some(124) {
            return Err(err(format!(
                "its narinfos for {} paths took past the {}s deadline",
                paths.len(),
                self.deadline
            )));
        }
        let mut answered = vec![None; paths.len()];
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some((Ok(i), Ok(code))) = l
                .split_once(' ')
                .map(|(i, c)| (i.parse::<usize>(), c.parse::<u16>()))
                && let Some(a) = answered.get_mut(i)
            {
                *a = Some(code);
            }
        }
        for (i, (p, code)) in paths.iter().zip(answered).enumerate() {
            match code {
                Some(404) => {}
                Some(200) => {
                    let src = dir.join(i.to_string());
                    let text = fs::read_to_string(&src)?;
                    // The runner is compiled with rustc alone: no crate's reader.
                    let field = |k: &str| {
                        text.lines()
                            .find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix(": ")))
                    };
                    let size = field("FileSize")
                        .or_else(|| field("NarSize"))
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    match (field("StorePath"), field("URL")) {
                        (Some(sp), Some(nar)) if sp == *p => {
                            let hash = hash_part(p);
                            fs::rename(&src, cache.join(format!("{hash}.narinfo")))?;
                            has.insert((*p).to_owned(), (size, nar.to_owned()));
                        }
                        _ => return Err(err(format!("its narinfo for {p} doesn't name it"))),
                    }
                }
                code => {
                    return Err(err(format!(
                        "the store answered {} for {p}'s narinfo: {}",
                        code.map_or("nothing".into(), |c| format!("HTTP {c}")),
                        String::from_utf8_lossy(&o.stderr).trim()
                    )));
                }
            }
        }
        if !o.status.success() {
            return Err(err(format!(
                "curl failed reading narinfos ({}): {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )));
        }
        if !has.is_empty() {
            let store_dir = has
                .keys()
                .next()
                .and_then(|p| p.rsplit_once('/').map(|(d, _)| d))
                .filter(|d| !d.is_empty())
                .unwrap_or("/nix/store");
            fs::write(
                cache.join("nix-cache-info"),
                format!("StoreDir: {store_dir}\n"),
            )?;
        }
        Ok(has)
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
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let cache = work.join("cache");
        let mut has = self.narinfos(url, &paths, work, &cache)?;
        // What its key doesn't sign, a store that trusts only that key wouldn't
        // take: it doesn't count. With no key named, a store trusts none, so
        // only what needs no signature (a content-addressed path) counts.
        // Verify offline against the narinfos already fetched above.
        let mut untrusted = 0;
        if !has.is_empty() {
            let local = format!("file://{}", cache.display());
            let mut args = vec![
                "store",
                "verify",
                "--store",
                &local,
                "--no-contents",
                "--sigs-needed",
                "1",
                "--option",
                "trusted-public-keys",
                key,
            ];
            args.extend(has.keys().map(String::as_str));
            let o = output(self.asking(&work.join("xdg"), &args))?;
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
                    "=https,http",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    self.deadline,
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

/// A store path's hash part: the prefix before `-` of its basename.
fn hash_part(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.split_once('-').map_or(base, |(h, _)| h)
}

/// A value quoted for a curl `-K` config file, refusing control characters
/// that could split a line or inject another directive.
fn curl_quote(value: &str) -> Result<String> {
    if value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(err("control character in curl config value"));
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    Ok(out)
}

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
