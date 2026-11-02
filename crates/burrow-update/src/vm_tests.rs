//! The VM tests, under KVM, on exactly this tree.
//!
//! A test that fails is run once more on the same tree. If it passes then,
//! it's flaky: the update goes ahead, and a Claude job is told to make the
//! test reliable (the owner hears about it until it has passed first time
//! three runs in a row). If it fails again, it's run on main as it was
//! before the updates. Passing there means the updates broke it, and they
//! wait for a fix. Failing there too means main's test is broken, which
//! isn't the updates' fault: they still wait, unless they take away a
//! serious known vulnerability from what runs now (CVSS 7+, per vulnix).
//! Those go live without the test, under the same dead man's switch and
//! health check as everything else.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::check::run_check;
use crate::dispatch::{age, open, settled};
use crate::memo::{Verdict, clean};
use crate::run::{Run, Step};
use crate::status::Outcome;
use crate::{Broke, Config};

/// How serious a known vulnerability is before taking it away lets an
/// update go live without a VM test main fails too: CVSS v3 7.0 and up.
const SERIOUS: f64 = 7.0;

/// How long a VM test fails before the owner hears that updates are stuck.
const STUCK: i64 = 86_400;

/// How many runs in a row a flaky test passes first time before it's
/// counted as fixed.
const STEADY: u32 = 3;

impl Run<'_> {
    pub(crate) fn vm_tests(&mut self) -> Step {
        let cfg = self.cfg;
        let logs = cfg.state.join("logs");
        let head = self.git(&["rev-parse", "HEAD"])?;
        for c in self.checks.clone() {
            let log = logs.join(format!("check-{c}.log"));
            let r = match self.memo.result_of(&c, &head) {
                Some(r) => r,
                None => {
                    let r = self.twice(&c, &head, &log)?;
                    self.memo.record(&c, &head, r)?;
                    r
                }
            };
            let key = format!("check-{c}");
            if r == Verdict::Pass {
                settled(cfg, &key, &format!("The VM test {c} passes again."));
                continue;
            }

            // It fails every time. The updates' fault, or main's?
            let base = if head == self.main {
                Verdict::Fail
            } else if let Some(base) = self.memo.result_of(&c, &self.main) {
                base
            } else {
                let on_main = logs.join(format!("check-{c}-main.log"));
                let base = verdict(run_check(cfg, &c, &self.main, &on_main));
                self.memo.record(&c, &self.main, base)?;
                base
            };
            let mut ended = None;
            if base == Verdict::Fail {
                self.untested.push(c.clone());
                self.dispatch(
                    &key,
                    &format!(
                        "{}: the VM test {c} fails on {} main ({}) itself, not only with the updates. The log is {}; the tree is {} (don't edit there, it's reset every run). Fix main. {}",
                        cfg.update,
                        cfg.origin,
                        self.main,
                        log.display(),
                        cfg.work.display(),
                        self.rules
                    ),
                );
            } else {
                let what = self.what()?;
                ended = Some(self.end(
                    Outcome::Failed,
                    &format!("VM test {c} fails with {what} and passes on main"),
                ));
                self.dispatch(
                    &key,
                    &format!(
                        "{}: the VM test {c} fails with these updates on top of main: {what}. It passes on main, so the updates break it. The log is {}; the tree is {} at {head} (don't edit there, it's reset every run). Make the updates pass it. {}",
                        cfg.update,
                        log.display(),
                        cfg.work.display(),
                        self.rules
                    ),
                );
            }
            if age(cfg, &key) > STUCK {
                let what = self.what()?;
                open(
                    cfg,
                    &key,
                    &format!("Updates stuck: VM test {c} fails"),
                    &format!(
                        "For a day now {} can't get the VM test {c} to pass ({what}), so updates that don't fix a serious vulnerability aren't deployed. Claude jobs have tried; see `proj status` on {}.",
                        cfg.update, cfg.host
                    ),
                );
            }
            if let Some(ended) = ended {
                return Err(ended);
            }
        }
        if self.untested.is_empty() {
            return Ok(());
        }
        let scanned = vulns(cfg, &self.running.current)
            .and_then(|before| Some((before, vulns(cfg, &self.new)?)));
        self.fixes = match scanned {
            Some((before, after)) => before.difference(&after).cloned().collect(),
            None => {
                println!("vulnix couldn't scan both systems; treating it as fixing nothing");
                Vec::new()
            }
        };
        let untested = self.untested.join(" ");
        if self.fixes.is_empty() {
            let what = self.what()?;
            return Err(self.end(
                Outcome::Failed,
                &format!(
                    "VM test {untested} fails on main as well; holding {what} until main passes"
                ),
            ));
        }
        println!(
            "main fails {untested}, but this fixes: {}",
            self.fixes.join(", ")
        );
        open(
            cfg,
            "untested",
            "Security updates went live without a passing VM test",
            &format!(
                "The VM test {untested} fails on main itself, and these updates fix serious vulnerabilities in what {} runs:\n{}\nSo they went live without it, under the dead man's switch and the health check. A Claude job is fixing main's test; this closes when an update passes every test again.",
                cfg.host,
                self.fixes.join("\n")
            ),
        );
        Ok(())
    }

    /// The VM test `c` on `head`'s tree, and once more when it fails: a
    /// pass the second time is a flaky test, and a job's to make reliable.
    /// `log` is what the run that counts said.
    fn twice(&self, c: &str, head: &str, log: &Path) -> Result<Verdict, Broke> {
        let cfg = self.cfg;
        let logs = cfg.state.join("logs");
        let flaky = format!("flaky-{c}");
        if run_check(cfg, c, head, log) {
            if clean(cfg, c, true)? >= STEADY {
                settled(
                    cfg,
                    &flaky,
                    &format!("The VM test {c} has passed first time three runs in a row."),
                );
            }
            return Ok(Verdict::Pass);
        }
        let again = logs.join(format!("check-{c}.log.again"));
        if !run_check(cfg, c, head, &again) {
            moved(&again, log)?;
            return Ok(Verdict::Fail);
        }
        let first = logs.join(format!("{flaky}.log"));
        moved(log, &first)?;
        moved(&again, log)?;
        clean(cfg, c, false)?;
        self.dispatch(
            &flaky,
            &format!(
                "{}: the VM test {c} is flaky: on {head} it failed once (log: {}) and passed when run again ({}). A flaky test holds back security updates, so find the cause and fix it, in the test or in what it tests; a retry or a longer timeout isn't a fix unless the cause really is time. {}",
                cfg.update,
                first.display(),
                log.display(),
                self.rules
            ),
        );
        open(
            cfg,
            &flaky,
            &format!("VM test {c} is flaky"),
            &format!(
                "The VM test {c} failed and then passed on the same tree, so updates went ahead. A Claude job is finding the cause; this closes once the test passes first time three runs in a row."
            ),
        );
        Ok(Verdict::Pass)
    }
}

fn verdict(passed: bool) -> Verdict {
    if passed { Verdict::Pass } else { Verdict::Fail }
}

fn moved(from: &Path, to: &Path) -> Result<(), Broke> {
    fs::rename(from, to).map_err(|e| Broke(format!("{} -> {}: {e}", from.display(), to.display())))
}

/// Serious known vulnerabilities in `system`'s closure, as "CVE package",
/// each once: nothing when vulnix fails or says something that isn't its
/// report, since a scan that didn't happen must never read as "fixed
/// everything". (vulnix exits non-zero when it finds any: what it says is
/// what counts.)
pub(crate) fn vulns(cfg: &Config, system: &str) -> Option<BTreeSet<String>> {
    let out = Command::new(&cfg.tools.vulnix)
        .args(["--closure", "--json", system])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let Value::Array(report) = serde_json::from_slice(&out.stdout).ok()? else {
        return None;
    };
    let mut found = BTreeSet::new();
    for p in &report {
        let pname = match &p["pname"] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        for cve in p["affected_by"].as_array()? {
            let cve = cve.as_str()?;
            let score = p["cvssv3_basescore"][cve].as_f64().unwrap_or(0.0);
            if score >= SERIOUS {
                found.insert(format!("{cve} {pname}"));
            }
        }
    }
    Some(found)
}

#[cfg(test)]
#[path = "vm_tests_tests.rs"]
mod tests;
