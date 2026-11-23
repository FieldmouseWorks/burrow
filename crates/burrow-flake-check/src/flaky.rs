//! The gate's side of docs/flaky-tests.md ("What a gate does"): with
//! `FLAKY_EXPIRED` and `FLAKY_REPORT` both set and not empty, a check whose
//! own build failed is built once more on its own. A pass is reported and
//! counts as one; a second failure, a check whose quarantine has run out, or
//! one whose name can't name a test, fails as it always did. So does a check
//! a derivation it needs failed for: that's a build that broke, not a test.
use super::*;

/// What the flaky tool gave the gate: the checks that get no second try,
/// and where to report one that passed on it.
pub struct Flaky {
    expired: Vec<String>,
    report: PathBuf,
}

impl Flaky {
    /// From the two variables' values: none unless both are set and
    /// neither is empty. A list of expired tests it can't read gives no
    /// test a second try, as the flaky tool does with a quarantine it
    /// can't read.
    pub fn new(expired: &str, report: &str) -> Option<Self> {
        if expired.is_empty() || report.is_empty() {
            return None;
        }
        match fs::read_to_string(expired) {
            Ok(x) => Some(Self {
                expired: x.lines().map(str::to_owned).collect(),
                report: report.into(),
            }),
            Err(e) => {
                println!(
                    "couldn't read FLAKY_EXPIRED, {expired} ({e}), so no check gets a second try"
                );
                None
            }
        }
    }
    pub fn from_env() -> Option<Self> {
        Self::new(&var("FLAKY_EXPIRED"), &var("FLAKY_REPORT"))
    }
}

/// Whether a check's name can name a test (docs/flaky-tests.md): ASCII
/// letters, digits and `_ . : / @ + = - [ ]`, starting with a letter, a
/// digit or `_`, at most 200 bytes. The flaky tool refuses a report with any
/// other, which would fail a run that passed.
pub fn valid(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 200
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:/@+=-[]".contains(c))
}

/// A check the gate builds: its attribute name under `checks.SYSTEM`, when
/// the gate knows it, and its target (`DRV^*`).
pub struct Check {
    pub name: Option<String>,
    pub target: String,
}

/// What building takes of nix.
pub trait Nix {
    /// Build these, all of them together the first time, one at a time
    /// after: whether every one built.
    fn build(&mut self, targets: &[String]) -> Result<bool>;
    /// The derivations still to build for this target: none once it's
    /// built, its own alone when that's the one that failed.
    fn pending(&mut self, target: &str) -> Result<Vec<String>>;
    /// The end of what its own build last printed, if nix kept it.
    fn log(&mut self, target: &str) -> Option<String>;
}

/// How many of a failed try's last lines a report carries: what the
/// issue shows of it.
const DETAIL_LINES: usize = 60;

/// Build the checks; with `flaky`, each whose own build failed once more,
/// on its own.
pub fn build(flaky: Option<&Flaky>, checks: &[Check], nix: &mut dyn Nix) -> Result<bool> {
    let all: Vec<String> = checks.iter().map(|c| c.target.clone()).collect();
    if nix.build(&all)? {
        return Ok(true);
    }
    let Some(flaky) = flaky else {
        return Ok(false);
    };
    let mut ok = true;
    let mut again = Vec::new();
    for c in checks {
        let drv = c.target.trim_end_matches("^*");
        let what = c.name.as_deref().unwrap_or(drv);
        let pending = match nix.pending(&c.target) {
            Ok(p) if p.is_empty() => continue,
            Ok(p) => p,
            Err(e) => {
                println!("no second try for {what}: can't tell what of it failed ({e})");
                ok = false;
                continue;
            }
        };
        let why_not = match c.name.as_deref() {
            _ if pending != [drv] => Some("a derivation it needs failed to build"),
            None => Some("the gate doesn't know its name"),
            Some(n) if !valid(n) => Some("its name can't name a test (docs/flaky-tests.md)"),
            Some(n) if flaky.expired.iter().any(|x| x == n) => {
                Some("its quarantine has run out (docs/flaky-tests.md)")
            }
            Some(n) => {
                again.push((n, &c.target));
                None
            }
        };
        if let Some(why) = why_not {
            println!("no second try for {what}: {why}");
            ok = false;
        }
    }
    if ok && again.is_empty() {
        // The build failed, but no check it built did.
        return Ok(false);
    }
    for (name, target) in again {
        // Before it's built again, which keeps the new log in its place.
        let detail = nix.log(target);
        println!("{name} failed: building it once more, on its own");
        if nix.build(std::slice::from_ref(target))? {
            println!("{name} passed on its second try: reported as flaky");
            report(&flaky.report, name, detail.as_deref())?;
        } else {
            println!("{name} failed again");
            ok = false;
        }
    }
    Ok(ok)
}

/// One line of the report, appended: a report that can't be written is the
/// gate's own error, since an unrecorded flake is one nobody fixes.
fn report(path: &Path, test: &str, detail: Option<&str>) -> Result<()> {
    let mut line = format!("{{\"test\":{}", quote(test));
    if let Some(d) = detail {
        let lines: Vec<&str> = d.lines().collect();
        let tail = lines[lines.len().saturating_sub(DETAIL_LINES)..].join("\n");
        line.push_str(&format!(",\"detail\":{}", quote(&tail)));
    }
    line.push_str("}\n");
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| {
            err(format!(
                "couldn't write FLAKY_REPORT, {}: {e}",
                path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// Checks that fail their first `n` builds, or whose dependency fails.
    #[derive(Default)]
    struct Fake {
        fails: HashMap<String, usize>,
        broken_dep: HashSet<String>,
        built: HashSet<String>,
        builds: Vec<Vec<String>>,
    }
    fn target(name: &str) -> String {
        format!("/nix/store/{name}.drv^*")
    }
    impl Nix for Fake {
        fn build(&mut self, targets: &[String]) -> Result<bool> {
            self.builds.push(targets.to_vec());
            let mut ok = true;
            for t in targets {
                let left = self.fails.entry(t.clone()).or_default();
                if self.broken_dep.contains(t) || *left > 0 {
                    *left = left.saturating_sub(1);
                    ok = false;
                } else {
                    self.built.insert(t.clone());
                }
            }
            Ok(ok)
        }
        fn pending(&mut self, t: &str) -> Result<Vec<String>> {
            let drv = t.trim_end_matches("^*").to_owned();
            Ok(if self.built.contains(t) {
                vec![]
            } else if self.broken_dep.contains(t) {
                vec!["/nix/store/dep.drv".into(), drv]
            } else {
                vec![drv]
            })
        }
        fn log(&mut self, t: &str) -> Option<String> {
            Some(format!("building {t}\nboom"))
        }
    }
    fn checks(names: &[&str]) -> Vec<Check> {
        names
            .iter()
            .map(|n| Check {
                name: Some((*n).into()),
                target: target(n),
            })
            .collect()
    }
    /// The tool's two files, `expired` listing those tests.
    fn flaky(dir: &Path, expired: &[&str]) -> Flaky {
        let x = dir.join("expired");
        fs::write(&x, expired.join("\n")).unwrap();
        Flaky::new(s(&x), s(&dir.join("report.jsonl"))).unwrap()
    }
    /// A directory of the test's own, gone after it.
    struct Dir(PathBuf);
    impl Dir {
        fn new(test: &str) -> Self {
            let d = env::temp_dir().join(format!("flaky-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn reported(dir: &Path) -> String {
        fs::read_to_string(dir.join("report.jsonl")).unwrap_or_default()
    }

    #[test]
    fn a_check_that_fails_once_then_passes_is_reported_and_passes() {
        let dir = Dir::new("a_check_that_fails_once_then_passes_is_reported_and_passes");
        let f = flaky(dir.path(), &[]);
        let mut nix = Fake::default();
        nix.fails.insert(target("browser"), 1);
        assert!(build(Some(&f), &checks(&["browser", "lint"]), &mut nix).unwrap());
        assert_eq!(
            nix.builds,
            [
                vec![target("browser"), target("lint")],
                vec![target("browser")]
            ]
        );
        let line = reported(dir.path());
        assert_eq!(line.lines().count(), 1, "{line}");
        assert_eq!(
            line,
            format!(
                "{{\"test\":\"browser\",\"detail\":{}}}\n",
                quote(&format!("building {}\nboom", target("browser")))
            )
        );
    }

    #[test]
    fn an_expired_check_fails_at_once_and_the_rest_still_get_their_try() {
        let dir = Dir::new("an_expired_check_fails_at_once_and_the_rest_still_get_their_try");
        let f = flaky(dir.path(), &["browser"]);
        let mut nix = Fake::default();
        nix.fails.insert(target("browser"), 1);
        nix.fails.insert(target("stream"), 1);
        assert!(!build(Some(&f), &checks(&["browser", "stream"]), &mut nix).unwrap());
        // browser gets no second try; stream does, and is reported.
        assert_eq!(
            nix.builds,
            [
                vec![target("browser"), target("stream")],
                vec![target("stream")]
            ]
        );
        assert_eq!(
            reported(dir.path()),
            "{\"test\":\"stream\",\"detail\":\"building /nix/store/stream.drv^*\\nboom\"}\n"
        );
    }

    #[test]
    fn with_either_variable_unset_or_empty_nothing_changes() {
        let dir = Dir::new("with_either_variable_unset_or_empty_nothing_changes");
        let x = dir.path().join("expired");
        fs::write(&x, "").unwrap();
        let r = dir.path().join("report.jsonl");
        for (expired, report) in [("", ""), (s(&x), ""), ("", s(&r))] {
            let f = Flaky::new(expired, report);
            assert!(f.is_none(), "{expired:?} {report:?}");
            let mut nix = Fake::default();
            nix.fails.insert(target("browser"), 1);
            assert!(!build(f.as_ref(), &checks(&["browser"]), &mut nix).unwrap());
            assert_eq!(nix.builds, [vec![target("browser")]]);
        }
        assert!(!r.exists());
    }

    #[test]
    fn a_build_that_broke_or_a_name_that_cant_be_a_tests_gets_no_second_try() {
        let dir = Dir::new("a_build_that_broke_or_a_name_that_cant_be_a_tests_gets_no_second_try");
        let f = flaky(dir.path(), &[]);
        let mut nix = Fake::default();
        nix.broken_dep.insert(target("vm"));
        assert!(!build(Some(&f), &checks(&["vm"]), &mut nix).unwrap());
        let mut nix = Fake::default();
        nix.fails.insert(target("it's"), 1);
        assert!(!build(Some(&f), &checks(&["it's"]), &mut nix).unwrap());
        assert_eq!(nix.builds.len(), 1);
        assert_eq!(reported(dir.path()), "");
    }

    #[test]
    fn a_failed_second_try_fails() {
        let dir = Dir::new("a_failed_second_try_fails");
        let f = flaky(dir.path(), &[]);
        let mut nix = Fake::default();
        nix.fails.insert(target("browser"), 2);
        assert!(!build(Some(&f), &checks(&["browser"]), &mut nix).unwrap());
        assert_eq!(nix.builds.len(), 2);
        assert_eq!(reported(dir.path()), "");
    }

    #[test]
    fn names_follow_the_flaky_tools_rules() {
        for ok in [
            "browser",
            "crates-burrow-flake-check",
            "_a.b:c/d@e+f=g-h[0]",
        ] {
            assert!(valid(ok), "{ok}");
        }
        for bad in ["", "-a", "a b", "it's", "é", &"a".repeat(201)] {
            assert!(!valid(bad), "{bad}");
        }
        assert!(valid(&"a".repeat(200)));
    }
}
