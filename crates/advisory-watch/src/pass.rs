//! One pass: each source run, what's new kept for a job (state.rs), and
//! everything waiting handed to its project's job.
//!
//! A source that can't be run or read is passed over, saying why on
//! stderr, and tried again at the next pass: the others still run. Only
//! the machine's own list of packages and the state it keeps can fail a
//! pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::sources::{self, Kev, Osv, Vulnix};
use crate::state::State;
use crate::{Config, Finding, stamp, task, today};

/// What the running system is.
const SYSTEM: &str = "/run/current-system";

/// How long a try at KEV's list may take.
const KEV_FOR: Duration = Duration::from_secs(60);

/// The biggest KEV list read (it's about 1.5 MB).
const KEV_LIMIT: u64 = 64 << 20;

/// osv-scanner's exit when it found nothing to scan.
const NOTHING_TO_SCAN: i32 = 128;

pub fn run(cfg: &Config) -> Result<(), String> {
    let mut st = State::open(&cfg.state)?;
    let names = installed(cfg)?;
    let mut found = Vec::new();

    // 1. Known exploited, matched by product name against what's installed.
    // Only a match is recorded as seen: a product installed later still gets
    // its known-exploited CVEs.
    if let Some(kev) = kev(&cfg.kev) {
        found.extend(st.offer("kev", sources::kev_findings(&kev, &names, &cfg.project))?);
        st.baselined("kev")?;
    }

    // 2. vulnix, once a day.
    let day = today();
    if st.vulnix_due(&day)
        && let Some(packages) = vulnix(cfg)
    {
        st.vulnix_ran(&day)?;
        found.extend(st.offer("vulnix", sources::vulnix_findings(&packages, &cfg.project))?);
        st.baselined("vulnix")?;
    }

    // 3. Every project's lockfiles. A Cargo.lock's crate counts only when
    // cargo builds it; one it doesn't isn't recorded as seen, so a feature
    // that turns it on later still gets it a job.
    for (p, dir) in projects(&cfg.src) {
        if let Some(osv) = osv(cfg, &dir) {
            let source = format!("osv-{p}");
            let built = built(cfg, &osv, &p, &st);
            found.extend(st.offer(&source, built)?);
            st.baselined(&source)?;
        }
    }

    if !found.is_empty() {
        let at = stamp();
        let lines: Vec<String> = found
            .iter()
            .map(|f| format!("{at} {}\t{}", f.project, f.text))
            .collect();
        st.log(&lines)?;
    }
    hand_over(cfg, &mut st)
}

/// One job for each project, with everything that waits for it. One that
/// can't be started leaves its findings waiting for the next pass; a
/// project whose checkout has gone takes its own with it.
fn hand_over(cfg: &Config, st: &mut State) -> Result<(), String> {
    let mut by_project: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    for f in st.pending() {
        by_project
            .entry(f.project.clone())
            .or_default()
            .push(f.clone());
    }
    for (p, found) in by_project {
        if p != cfg.project && !checkout(&cfg.src.join(&p)) {
            st.dropped(&p)?;
            st.log(&[format!(
                "{} {p}: its checkout is gone, so what it had waiting is dropped",
                stamp()
            )])?;
            continue;
        }
        let found: Vec<&Finding> = found.iter().collect();
        match job(cfg, &p, &task::text(cfg, &p, &found)) {
            Ok(id) => {
                st.handed(&p)?;
                st.log(&[format!("{} {p}: handed to Claude, proj log {id}", stamp())])?;
            }
            Err(why) => {
                eprintln!("advisory-watch: {p}: {why}");
                st.log(&[format!(
                    "{} {p}: not handed to Claude ({why}); the next pass tries again",
                    stamp()
                )])?;
            }
        }
    }
    Ok(())
}

/// A command run as the user, with what runuser leaves of root's
/// environment. That leaves root's runtime dir, when it has one, which
/// would send a job to root's own manager: proj finds the user's itself.
fn as_user(cfg: &Config, program: &Path) -> Command {
    let mut c = Command::new(&cfg.tools.runuser);
    c.args(["-u", &cfg.user, "--"])
        .arg(program)
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null());
    c
}

/// The job's id, from proj.
fn job(cfg: &Config, p: &str, task: &str) -> Result<String, String> {
    let mut c = as_user(cfg, &cfg.tools.proj);
    c.args(["run", "--first", "--role", "advisory"]);
    if let Some(app) = &cfg.app {
        c.args(["--app", app]);
    }
    let out = c
        .args([p, task])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", cfg.tools.proj.display()))?;
    if !out.status.success() {
        return Err(format!("proj run: {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

/// The package names in the running system.
fn installed(cfg: &Config) -> Result<BTreeSet<String>, String> {
    let nix_store = &cfg.tools.nix_store;
    let out = Command::new(nix_store)
        .args(["-qR", SYSTEM])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", nix_store.display()))?;
    if !out.status.success() {
        return Err(format!("nix-store -qR {SYSTEM}: {}", out.status));
    }
    Ok(sources::package_names(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// KEV's list, from the first place that has it. cisa.gov refuses some
/// hosting providers' addresses (403); CISA's own GitHub copy is the same
/// list. What the places said is said only when none had it.
fn kev(urls: &[String]) -> Option<Kev> {
    use ureq::tls::{RootCerts, TlsConfig};
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(KEV_FOR))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let mut failed = Vec::new();
    for url in urls {
        let got = agent
            .get(url)
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut r| {
                if !r.status().is_success() {
                    return Err(format!("HTTP {}", r.status()));
                }
                r.body_mut()
                    .with_config()
                    .limit(KEV_LIMIT)
                    .read_json::<Kev>()
                    .map_err(|e| e.to_string())
            });
        match got {
            Ok(kev) => return Some(kev),
            Err(e) => failed.push(format!("{url}: {e}")),
        }
    }
    for f in failed {
        eprintln!("advisory-watch: KEV: {f}");
    }
    None
}

/// vulnix's packages, when it ran. It fails when it finds something, so
/// what it printed counts whatever its exit.
fn vulnix(cfg: &Config) -> Option<Vec<Vulnix>> {
    let out = Command::new(&cfg.tools.vulnix)
        .args(["--system", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    read("vulnix", out, |text| serde_json::from_slice(text))
}

/// osv-scanner's findings in a checkout, scanned as the user. It fails
/// when it finds something too. A checkout with nothing it can read (no
/// lockfiles) is no news: it says so with 128.
fn osv(cfg: &Config, dir: &Path) -> Option<Osv> {
    let out = as_user(cfg, &cfg.tools.osv_scanner)
        .args(["scan", "source", "--recursive", "--format", "json"])
        .arg(dir)
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = &out
        && o.status.code() == Some(NOTHING_TO_SCAN)
    {
        return None;
    }
    read(&format!("osv-scanner {}", dir.display()), out, |text| {
        serde_json::from_slice(text)
    })
}

/// osv-scanner's findings in project `p` that cargo builds: one from a
/// Cargo.lock is left out when `cargo tree` ran there and didn't list its
/// crate. cargo runs only for findings not already known, once a lockfile.
fn built(cfg: &Config, osv: &Osv, p: &str, st: &State) -> Vec<Finding> {
    let found = sources::osv_findings(osv, p);
    let locks: BTreeSet<String> = found
        .iter()
        .filter(|f| !st.known(&f.finding.key))
        .filter_map(|f| f.cargo.as_ref().map(|c| c.lock.clone()))
        .collect();
    let trees: BTreeMap<String, BTreeSet<(String, String)>> = locks
        .into_iter()
        .filter_map(|lock| {
            let tree = cargo_tree(cfg, Path::new(&lock))?;
            Some((lock, tree))
        })
        .collect();
    found
        .into_iter()
        .filter(|f| {
            let Some(c) = &f.cargo else { return true };
            trees
                .get(c.lock.as_str())
                .is_none_or(|t| t.contains(&(c.name.clone(), c.version.clone())))
        })
        .map(|f| f.finding)
        .collect()
}

/// Every crate the workspace a Cargo.lock belongs to builds, on any
/// target, with every feature, as the user. cargo never rewrites the
/// lockfile (`--locked`) or fetches (`--offline`); none when it can't say,
/// and then every finding in it counts.
fn cargo_tree(cfg: &Config, lock: &Path) -> Option<BTreeSet<(String, String)>> {
    let manifest = lock.with_file_name("Cargo.toml");
    // From /, never the checkout: cargo reads the .cargo/config.toml of
    // where it runs, and runs the rustc that names (`build.rustc`) even for
    // `tree`. The scan reaches every directory in a checkout, so that's
    // code no one looked at. A project's own sources (vendored, say) are
    // then unknown, and cargo failing leaves every finding counting.
    let out = as_user(cfg, &cfg.tools.cargo)
        .current_dir("/")
        .args(["tree", "--workspace", "--edges", "normal,build,dev"])
        .args(["--target", "all", "--all-features"])
        .args(["--prefix", "none", "--format", "{p}"])
        .args(["--locked", "--offline", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            Some(sources::crates_built(&String::from_utf8_lossy(&o.stdout)))
        }
        Ok(o) => {
            eprintln!(
                "advisory-watch: cargo tree {}: {}; its findings all count",
                manifest.display(),
                o.status
            );
            None
        }
        Err(e) => {
            eprintln!("advisory-watch: {}: {e}", cfg.tools.cargo.display());
            None
        }
    }
}

/// What a source printed, read: none when it printed nothing and failed,
/// or printed what doesn't read.
fn read<T>(
    what: &str,
    out: std::io::Result<std::process::Output>,
    parse: impl Fn(&[u8]) -> serde_json::Result<T>,
) -> Option<T> {
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            eprintln!("advisory-watch: {what}: {e}");
            return None;
        }
    };
    if !out.status.success() && out.stdout.is_empty() {
        eprintln!("advisory-watch: {what}: {}", out.status);
        return None;
    }
    parse(&out.stdout)
        .map_err(|e| eprintln!("advisory-watch: {what}: its output doesn't read: {e}"))
        .ok()
}

/// The projects in `src`: each directory, dotted ones aside, that's a git
/// checkout of its own (a worktree's .git is a file), by name.
fn projects(src: &Path) -> Vec<(String, PathBuf)> {
    let entries = match fs::read_dir(src) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("advisory-watch: {}: {e}", src.display());
            return Vec::new();
        }
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (!name.starts_with('.') && checkout(&e.path())).then(|| (name, e.path()))
        })
        .collect();
    found.sort();
    found
}

fn checkout(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_are_checkouts_of_their_own_by_name() {
        let src = tempfile::tempdir().unwrap();
        let names = [
            "m", "c", "x", "a", "q", "e", "z", "b", "k", "t", "g", "o", "d", "w", "i", "s",
        ];
        for n in names {
            fs::create_dir_all(src.path().join(n).join(".git")).unwrap();
        }
        fs::create_dir_all(src.path().join(".dotted/.git")).unwrap();
        fs::create_dir_all(src.path().join("plain")).unwrap();
        fs::create_dir_all(src.path().join("wt")).unwrap();
        fs::write(
            src.path().join("wt/.git"),
            "gitdir: ../a/.git/worktrees/wt\n",
        )
        .unwrap();
        let mut sorted = names.to_vec();
        sorted.sort();
        let found: Vec<(String, PathBuf)> = sorted
            .iter()
            .map(|n| (n.to_string(), src.path().join(n)))
            .collect();
        assert_eq!(projects(src.path()), found);
        assert!(projects(&src.path().join("none")).is_empty());
    }
}
