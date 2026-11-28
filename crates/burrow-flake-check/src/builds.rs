//! The build store's runner-side rules (docs/gate-design.md, stage 1):
//! a run that may write the store (BUILDS, from the caller's Actions
//! variable, never an input) gets [`NIX_CONFIG`], so a flake's own
//! `nixConfig` can't add a substituter, trust a key, or drop the sandbox.
//!
//! Mid-run substituter failures are handled per substituter here instead of
//! enabling Nix's global `fallback = true` (which turns a transient
//! `cache.nixos.org` timeout into a nixpkgs bootstrap from `stage0-posix`):
//! - if the configured build store (`BUILDS_SUBSTITUTER`) fails during
//!   substitution, drop only it from `substituters` and retry building cold;
//! - if an upstream substituter (such as `https://cache.nixos.org`) fails
//!   transiently, wait briefly and retry in a fresh Nix process without
//!   enabling `fallback = true`; if it still fails after retries, report an
//!   infrastructure error rather than blaming PR code.
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

/// What a run that writes the store adds to every nix it runs, last, so it
/// wins: a flake's `nixConfig` is never taken (so it can't add a
/// substituter, or a key to trust, or turn the sandbox off), whatever the
/// runner's own config says, and builds are sandboxed. What such a run
/// built goes to the store under the machine's key.
pub const NIX_CONFIG: &str = "accept-flake-config = false\nsandbox = true";

/// Backoff delays (seconds) between retries when an upstream substituter
/// (like `https://cache.nixos.org`) hits a transient failure.
const UPSTREAM_BACKOFF_SECS: &[u64] = &[3, 10, 25];

/// Override lines appended to `NIX_CONFIG` once `BUILDS_SUBSTITUTER` has been
/// dropped mid-run, so every subsequent nix process omits it.
static DROPPED: Mutex<Option<String>> = Mutex::new(None);

/// Whether this run may write the store: its caller named one.
pub fn writes() -> bool {
    !var("BUILDS").is_empty()
}

/// The configured build-store substituter URL (empty if not configured or if
/// its reachability probe failed before Nix started).
pub fn substituter() -> String {
    var("BUILDS_SUBSTITUTER").trim().to_owned()
}

/// Normalize a substituter URL for boundary-safe matching: strip query
/// parameters (`?priority=40`, `?trusted=true`) and trailing slashes.
pub fn normalize_url(url: &str) -> &str {
    let without_query = url.split('?').next().unwrap_or(url);
    without_query.trim_end_matches('/')
}

/// Whether `BUILDS_SUBSTITUTER` was already dropped in this process.
pub fn nix_config() -> String {
    let base = var("NIX_CONFIG");
    let extra = DROPPED.lock().ok().and_then(|d| d.clone());
    match extra {
        Some(ov) if !base.is_empty() => format!("{base}\n{ov}"),
        Some(ov) => ov,
        None => base,
    }
}

/// Record a drop that already happened during bootstrap (`run.sh`) into the
/// worker's attested log so `soft/builds-fallback` observes it too.
pub fn announce_bootstrap_drop() {
    if var("BUILDS_DROPPED") == "1" {
        let sub = normalize_url(&substituter()).to_owned();
        if !sub.is_empty() {
            println!(
                "substituter '{sub}' failed during substitution; dropping it and retrying without it"
            );
        }
    }
}

/// A derivation's name: its path's, past the hash and before `.drv`.
pub fn name(drv: &str) -> String {
    let base = drv.rsplit('/').next().unwrap_or(drv);
    let n = base.split_once('-').map_or(base, |(_, n)| n);
    n.trim_end_matches(".drv").to_owned()
}

/// Currently configured substituters from Nix, falling back to
/// `BUILDS_SUBSTITUTER` + `https://cache.nixos.org` if `nix config show` is
/// unavailable (e.g. under a minimal stub).
fn active_substituters(config: &str) -> Vec<String> {
    let mut c = cmd("nix", &["config", "show", "substituters"]);
    if !config.is_empty() {
        c.env("NIX_CONFIG", config);
    }
    if let Ok(o) = output(c)
        && o.status.success()
    {
        return String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
    }
    let mut fallback = vec!["https://cache.nixos.org".to_owned()];
    let sub = substituter();
    if !sub.is_empty() {
        fallback.push(sub);
    }
    fallback
}

/// Build the `NIX_CONFIG` override that drops `builds_sub` (normalized) from
/// `active` while preserving every other substituter.
fn drop_override(builds_sub: &str, active: &[String]) -> String {
    let kept: Vec<&str> = active
        .iter()
        .map(String::as_str)
        .filter(|entry| normalize_url(entry) != builds_sub)
        .collect();
    if kept.is_empty() {
        "substituters =\nextra-substituters =".to_owned()
    } else {
        format!("substituters = {}\nextra-substituters =", kept.join(" "))
    }
}

/// Record a dropped build store if not already dropped; returns `true` iff
/// this call performed the drop.
fn record_drop(builds_sub: &str, active: &[String]) -> bool {
    let ov = drop_override(builds_sub, active);
    if let Ok(mut slot) = DROPPED.lock() {
        if slot.is_some() {
            return false;
        }
        *slot = Some(ov);
        return true;
    }
    false
}

/// Streaming scanner over Nix's stdout/stderr that identifies fatal
/// substitution errors and attributes them to exact configured substituters.
#[derive(Debug, Default)]
pub(crate) struct FailureScan {
    known: Vec<String>,
    copying: HashMap<String, Vec<String>>,
    pub(crate) failed: Vec<String>,
    pub(crate) fatal: bool,
    pending: Vec<u8>,
}

impl FailureScan {
    pub(crate) fn new(active: &[String]) -> Self {
        let known: Vec<String> = active
            .iter()
            .map(|u| normalize_url(u).to_owned())
            .filter(|u| !u.is_empty())
            .collect();
        Self {
            known,
            ..Self::default()
        }
    }

    fn mark_failed(&mut self, url: &str) {
        let norm = normalize_url(url);
        if self.known.iter().any(|k| k == norm) && !self.failed.iter().any(|f| f == norm) {
            self.failed.push(norm.to_owned());
        }
    }

    fn match_download_url(&mut self, download: &str) -> bool {
        let found: Option<String> = self
            .known
            .iter()
            .find(|k| {
                download == k.as_str()
                    || download
                        .strip_prefix(k.as_str())
                        .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('?'))
            })
            .cloned();
        if let Some(u) = found {
            self.mark_failed(&u);
            true
        } else {
            false
        }
    }

    pub(crate) fn feed_line(&mut self, raw: &str) {
        let clean = crate::negative::classify::plain(raw);
        let line = clean.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_start();
        // Never treat builder output excerpts (`       > ...`) as Nix diagnostics.
        if trimmed.starts_with('>') {
            return;
        }
        // Only inspect Nix's own diagnostic/copying lines, never `<drv>> ...`.
        if let Some(rest) = line.strip_prefix("copying path '") {
            if let Some((path, after)) = rest.split_once("' from '")
                && let Some((url, _)) = after.split_once('\'')
            {
                let norm = normalize_url(url).to_owned();
                let entry = self.copying.entry(path.to_owned()).or_default();
                if !entry.contains(&norm) {
                    entry.push(norm);
                }
            }
            return;
        }
        let is_nix_diag = line.starts_with("disabling binary cache '")
            || line.starts_with("error: ")
            || line.starts_with("warning: ")
            || trimmed.starts_with("… ")
            || trimmed.starts_with("error: ")
            || trimmed.starts_with("substituter '");
        if !is_nix_diag {
            return;
        }
        let is_error = line.starts_with("error: ") || trimmed.starts_with("error: ");

        if let Some(rest) = line.strip_prefix("error: path '")
            && let Some(path) =
                rest.strip_suffix("' is required, but there is no substituter that can build it")
        {
            self.fatal = true;
            if let Some(urls) = self.copying.get(path).cloned() {
                for u in urls {
                    self.mark_failed(&u);
                }
            }
            return;
        }

        if (line.starts_with("error: some substitutes for the outputs of derivation '")
            && line.contains("' failed"))
            || (line.starts_with("error: some references of path '")
                && line.ends_with("' could not be realised"))
        {
            self.fatal = true;
            return;
        }

        if let Some(u) = quoted_after(line, "disabling binary cache '") {
            self.mark_failed(u);
        }
        if let Some(u) = quoted_after(line, "substituter '") {
            let after =
                &line[line.find("substituter '").unwrap() + "substituter '".len() + u.len()..];
            if after.starts_with("' is disabled") || after.starts_with("' is temporarily disabled")
            {
                self.fatal = true;
                self.mark_failed(u);
            }
        }
        if let Some(i) = line.find("' does not appear to be a binary cache")
            && let Some(start) = line[..i].rfind('\'')
        {
            self.mark_failed(&line[start + 1..i]);
        }
        if let Some(d) = quoted_after(line, "unable to download '")
            && self.match_download_url(d)
            && is_error
        {
            self.fatal = true;
        }
        if line.contains("NAR for '")
            && let Some(u) = quoted_after(line, "fetched from '")
        {
            let after =
                &line[line.find("fetched from '").unwrap() + "fetched from '".len() + u.len()..];
            if after.starts_with("' is incomplete") {
                self.fatal = true;
                self.mark_failed(u);
            }
        }
        if line.contains("ignoring substitute for '")
            && let Some(u) = quoted_after(line, "from '")
        {
            self.mark_failed(u);
        }
        if let Some(u) = quoted_after(line, "ignoring untrusted substituter '") {
            self.mark_failed(u);
        }
    }

    pub(crate) fn finish(&mut self) {
        if !self.pending.is_empty() {
            let tail = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            self.feed_line(&tail);
        }
    }
}

fn quoted_after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let start = line.find(prefix)? + prefix.len();
    let rest = &line[start..];
    let end = rest.find('\'')?;
    Some(&rest[..end])
}

impl Write for FailureScan {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&self.pending[..=pos]).into_owned();
            self.pending.drain(..=pos);
            self.feed_line(&line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Run a Nix build/check command with per-substituter failure handling:
/// drop `BUILDS_SUBSTITUTER` on mid-run failure and retry cold, or retry
/// briefly on transient upstream substituter failures without global fallback.
pub fn run_nix(with_heavy: bool, args: &[String]) -> Result<bool> {
    let prog = if with_heavy { "heavy" } else { "nix" };
    run_nix_with(
        prog,
        &substituter(),
        UPSTREAM_BACKOFF_SECS,
        |secs| std::thread::sleep(std::time::Duration::from_secs(secs)),
        || {
            let cfg = nix_config();
            let active = active_substituters(&cfg);
            let mut c = if with_heavy {
                let mut full = Vec::with_capacity(args.len() + 2);
                full.push("--".to_owned());
                full.push("nix".to_owned());
                full.extend_from_slice(args);
                let refs: Vec<&str> = full.iter().map(String::as_str).collect();
                merged("heavy", &refs)
            } else {
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                merged("nix", &refs)
            };
            if !cfg.is_empty() {
                c.env("NIX_CONFIG", &cfg);
            }
            let mut scan = FailureScan::new(&active);
            let ended = tee(c, &mut scan, &mut io::stdout())?;
            scan.finish();
            Ok((ended.code, active, scan))
        },
        record_drop,
    )
}

fn run_nix_with<F, S, D>(
    prog: &str,
    builds_substituter: &str,
    backoff: &[u64],
    mut sleep: S,
    mut exec: F,
    mut try_drop: D,
) -> Result<bool>
where
    F: FnMut() -> Result<(i32, Vec<String>, FailureScan)>,
    S: FnMut(u64),
    D: FnMut(&str, &[String]) -> bool,
{
    let builds_sub = normalize_url(builds_substituter).to_owned();
    let mut upstream_tries = 0usize;
    loop {
        let (code, active, scan) = exec()?;
        if code == 0 {
            return Ok(true);
        }
        if code >= verdict::STOPPED {
            verdict::broke(&*err(format!(
                "{prog} was killed or couldn't run (exit {code})"
            )));
            return Ok(false);
        }
        if !scan.fatal {
            return Ok(false);
        }
        let builds_failed = !builds_sub.is_empty()
            && scan.failed.iter().any(|u| u == &builds_sub)
            && try_drop(&builds_sub, &active);
        let upstream_failed: Vec<String> = scan
            .failed
            .iter()
            .filter(|u| builds_sub.is_empty() || u.as_str() != builds_sub)
            .cloned()
            .collect();

        if builds_failed {
            println!(
                "substituter '{builds_sub}' failed during substitution; dropping it and retrying without it"
            );
            if upstream_failed.is_empty() {
                continue;
            }
        }
        if !upstream_failed.is_empty() {
            if let Some(&delay) = backoff.get(upstream_tries) {
                upstream_tries += 1;
                println!(
                    "substituter {} failed transiently during substitution; retrying in {delay}s ({upstream_tries}/{})",
                    upstream_failed.join(", "),
                    backoff.len()
                );
                sleep(delay);
                continue;
            }
            return Err(err(format!(
                "substituter {} failed after {} retries",
                upstream_failed.join(", "),
                backoff.len()
            )));
        }
        return Ok(false);
    }
}

#[cfg(test)]
mod tests;
