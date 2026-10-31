//! keep against a fake nix that writes a binary cache as nix does: a
//! narinfo per path, named for its hash, and its NAR under `nar/`, named for
//! its contents, for the whole closure of what it's given but what the cache
//! holds already, and only once it can read there each path it refers to.
//! tests/nix.rs runs it against real nix.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::{Info, Kept, Nix, Store, keep};

/// A path in the stand-in's store: signed or not, what it refers to, and
/// its contents, which name its NAR.
struct P {
    signed: bool,
    refs: &'static [&'static str],
    contents: &'static str,
}

#[derive(Default)]
struct Fake {
    paths: BTreeMap<String, P>,
    outputs: BTreeMap<String, Vec<String>>,
    fail: bool,
    /// Each path copy wrote, in order.
    wrote: RefCell<Vec<String>>,
}

fn sp(name: &str) -> String {
    format!("/nix/store/{name}")
}

fn hash(path: &str) -> &str {
    path.trim_start_matches("/nix/store/")
        .split_once('-')
        .unwrap()
        .0
}

impl Fake {
    fn path(
        &mut self,
        name: &str,
        signed: bool,
        refs: &'static [&'static str],
        contents: &'static str,
    ) {
        let p = P {
            signed,
            refs,
            contents,
        };
        self.paths.insert(sp(name), p);
    }

    /// A derivation here, unsigned, and what it makes.
    fn drv(&mut self, name: &str, makes: &[&str]) {
        self.path(name, false, &[], "drv");
        self.outputs
            .insert(sp(name), makes.iter().map(|m| sp(m)).collect());
    }
}

/// A narinfo as nix writes one, with the fields keep reads.
fn narinfo(path: &str, contents: &str) -> String {
    format!(
        "StorePath: {path}\nURL: nar/{contents}.nar\nCompression: none\nNarHash: sha256-{contents}\nNarSize: {}\nReferences: \n",
        contents.len()
    )
}

impl Nix for Fake {
    fn store(&self) -> Result<Store, String> {
        let info = |p: &P| Info {
            signed: p.signed,
            references: p.refs.iter().map(|r| sp(r)).collect(),
            nar_hash: format!("sha256-{}", p.contents),
            nar_size: p.contents.len() as u64,
        };
        Ok(Store {
            dir: "/nix/store".into(),
            paths: self
                .paths
                .iter()
                .map(|(n, p)| (n.clone(), info(p)))
                .collect(),
        })
    }

    fn outputs(&self, drvs: &[String]) -> Result<BTreeSet<String>, String> {
        let mut made = BTreeSet::new();
        for d in drvs {
            made.extend(
                self.outputs
                    .get(d)
                    .ok_or(format!("{d}: not a derivation"))?
                    .clone(),
            );
        }
        Ok(made)
    }

    fn copy(&self, paths: &[String], to: &Path) -> Result<(), String> {
        if self.fail {
            return Err("nix copy: failed".into());
        }
        let file = |p: &str| to.join(format!("{}.narinfo", hash(p)));
        let mut closure = BTreeSet::new();
        let mut todo: Vec<String> = paths.to_vec();
        while let Some(p) = todo.pop() {
            let info = self.paths.get(&p).ok_or(format!("{p}: not in the store"))?;
            todo.extend(info.refs.iter().map(|r| sp(r)));
            closure.insert(p);
        }
        let missing: Vec<&String> = closure.iter().filter(|p| !file(p).exists()).collect();
        for p in &missing {
            for r in self.paths[*p].refs.iter().map(|r| sp(r)) {
                if !missing.contains(&&r) && !readable(&file(&r), &r) {
                    return Err(format!(
                        "cannot add '{p}': the reference '{r}' is not valid"
                    ));
                }
            }
        }
        for p in missing {
            let info = &self.paths[p];
            fs::write(file(p), narinfo(p, info.contents)).unwrap();
            fs::write(to.join(format!("nar/{}.nar", info.contents)), info.contents).unwrap();
            self.wrote.borrow_mut().push(p.clone());
        }
        Ok(())
    }
}

/// Whether nix would read this narinfo as the path's: one naming it, with
/// a NAR, its hash and a size.
fn readable(file: &Path, path: &str) -> bool {
    let Ok(text) = fs::read_to_string(file) else {
        return false;
    };
    let has = |key: &str| {
        text.lines()
            .any(|l| l.strip_prefix(key).is_some_and(|v| !v.is_empty()))
    };
    text.lines().any(|l| l == format!("StorePath: {path}"))
        && has("URL: ")
        && has("NarHash: sha256")
        && text.lines().any(|l| {
            l.strip_prefix("NarSize: ")
                .is_some_and(|n| n.parse::<u64>().is_ok_and(|n| n > 0))
        })
}

/// The paths a cache holds, and its NARs.
fn cache(dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut paths: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|f| {
            let f = f.unwrap().path();
            let text = fs::read_to_string(&f).ok()?;
            f.extension().filter(|e| *e == "narinfo")?;
            Some(
                text.lines()
                    .next()?
                    .trim_start_matches("StorePath: ")
                    .to_owned(),
            )
        })
        .collect();
    let mut nars: Vec<String> = fs::read_dir(dir.join("nar"))
        .unwrap()
        .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    paths.sort();
    nars.sort();
    (paths, nars)
}

/// A cache of these paths, as an earlier push kept it.
fn kept(dir: &Path, paths: &[(&str, &str)]) {
    let kept = dir.join("kept");
    fs::create_dir_all(kept.join("nar")).unwrap();
    fs::write(kept.join("nix-cache-info"), "StoreDir: /nix/store\n").unwrap();
    for (name, contents) in paths {
        let p = sp(name);
        fs::write(
            kept.join(format!("{}.narinfo", hash(&p))),
            narinfo(&p, contents),
        )
        .unwrap();
        fs::write(kept.join(format!("nar/{contents}.nar")), contents).unwrap();
    }
}

fn names(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| sp(p)).collect()
}

fn nars(contents: &[&str]) -> Vec<String> {
    contents.iter().map(|c| format!("{c}.nar")).collect()
}

/// A first push, with nothing kept: what this gate built, but not its
/// derivations, nor what a public cache signed though the built paths
/// refer to it, near or far, which nix never writes.
#[test]
fn a_push_keeps_what_no_one_signed_but_derivations() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::create_dir(d.join("kept")).unwrap();
    let mut nix = Fake::default();
    nix.path("aaa-git-2.54", false, &["hhh-hello"], "git");
    nix.path("bbb-vm-test-run-proj", false, &[], "vm");
    nix.path("ggg-glibc", true, &[], "glibc");
    nix.path("hhh-hello", true, &["ggg-glibc"], "hello");
    nix.drv("fff-git.drv", &["aaa-git-2.54"]);
    nix.drv("ddd-gate.drv", &["bbb-vm-test-run-proj"]);
    let k = keep(&nix, d).unwrap();
    assert_eq!((k.gate, k.carried), (2, 0), "{k:?}");
    assert_eq!(
        cache(&d.join("kept")),
        (
            names(&["aaa-git-2.54", "bbb-vm-test-run-proj"]),
            nars(&["git", "vm"])
        )
    );
    assert_eq!(
        fs::read_to_string(d.join("kept/nix-cache-info")).unwrap(),
        "StoreDir: /nix/store\n"
    );
    assert_eq!(
        *nix.wrote.borrow(),
        names(&["aaa-git-2.54", "bbb-vm-test-run-proj"])
    );
    // What it restored is moved aside, and nothing's left being kept.
    assert!(d.join("restored").is_dir() && !d.join("keep").exists());
    assert_eq!(k.bytes, crate::size(&d.join("kept")).unwrap());
}

/// A later push: what the kept builds hold and this gate has again is
/// linked across, not written again; what they hold that a derivation here
/// makes, though this gate didn't need it, is carried; and what no
/// derivation here makes is dropped.
#[test]
fn what_a_derivation_here_still_makes_is_carried_and_the_rest_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    kept(
        d,
        &[
            ("ccc-check", "check"),
            ("aaa-git-2.54", "git"),
            ("zzz-old-check", "old"),
        ],
    );
    let mut nix = Fake::default();
    // The check came whole from the kept builds, and its derivation still
    // makes git, which it was built from.
    nix.path("ccc-check", false, &["ggg-glibc"], "check");
    nix.path("ggg-glibc", true, &[], "glibc");
    nix.drv("kkk-check.drv", &["ccc-check"]);
    nix.drv("fff-git.drv", &["aaa-git-2.54"]);
    let inode = |p: &Path| fs::metadata(p).unwrap().ino();
    let before = inode(&d.join("kept/nar/git.nar"));
    let k = keep(&nix, d).unwrap();
    assert_eq!((k.gate, k.carried), (1, 1), "{k:?}");
    assert_eq!(
        cache(&d.join("kept")),
        (
            names(&["aaa-git-2.54", "ccc-check"]),
            nars(&["check", "git"])
        )
    );
    assert_eq!(inode(&d.join("kept/nar/git.nar")), before);
    // The check was kept before, and glibc, which it refers to, stood in:
    // nix wrote nothing.
    assert!(nix.wrote.borrow().is_empty(), "{:?}", nix.wrote.borrow());
    assert_eq!(
        cache(&d.join("restored")).0,
        names(&["aaa-git-2.54", "ccc-check", "zzz-old-check"])
    );
}

/// A narinfo keep can't read, or whose NAR isn't a file of its own under
/// nar/, isn't a path of the kept builds, and nothing else there is.
#[test]
fn what_the_kept_builds_hold_is_only_what_they_name_as_nix_does() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    kept(d, &[("aaa-good", "good")]);
    let k = d.join("kept");
    for (hash, text) in [
        (
            "bbb",
            "StorePath: /nix/store/bbb-up\nURL: nar/../../etc.nar\n",
        ),
        ("ccc", "StorePath: /nix/store/ccc-deep\nURL: nar/x/y.nar\n"),
        ("ddd", "StorePath: /nix/store/ddd-nourl\n"),
        ("eee", "URL: nar/e.nar\n"),
        (
            "fff",
            "StorePath: /nix/store/fff-elsewhere\nURL: elsewhere.nar\n",
        ),
    ] {
        fs::write(k.join(format!("{hash}.narinfo")), text).unwrap();
    }
    fs::write(
        k.join("ggg.txt"),
        "StorePath: /nix/store/ggg-text\nURL: nar/good.nar\n",
    )
    .unwrap();
    let mut nix = Fake::default();
    let all = [
        "aaa-good",
        "bbb-up",
        "ccc-deep",
        "ddd-nourl",
        "fff-elsewhere",
        "ggg-text",
    ];
    nix.drv("kkk-all.drv", &all);
    let kept = keep(&nix, d).unwrap();
    assert_eq!(kept.carried, 1);
    assert_eq!(
        cache(&d.join("kept")),
        (names(&["aaa-good"]), nars(&["good"]))
    );
}

/// A failure leaves the kept builds as they were, for nothing to be saved.
#[test]
fn a_failure_leaves_the_kept_builds_as_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    kept(d, &[("aaa-git-2.54", "git")]);
    let mut nix = Fake {
        fail: true,
        ..Fake::default()
    };
    nix.path("bbb-vm", false, &[], "vm");
    nix.drv("fff-git.drv", &["aaa-git-2.54"]);
    let err = keep(&nix, d).unwrap_err();
    assert!(err.contains("nix copy"), "{err}");
    assert_eq!(
        cache(&d.join("kept")),
        (names(&["aaa-git-2.54"]), nars(&["git"]))
    );
    assert!(!d.join("restored").exists());
}

/// What a push kept, as the step says it.
#[test]
fn nothing_to_keep_is_an_empty_cache() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::create_dir(d.join("kept")).unwrap();
    let mut nix = Fake::default();
    nix.path("ggg-glibc", true, &[], "glibc");
    let k = keep(&nix, d).unwrap();
    assert_eq!(
        k,
        Kept {
            gate: 0,
            carried: 0,
            bytes: "StoreDir: /nix/store\n".len() as u64
        }
    );
    assert!(nix.wrote.borrow().is_empty());
}
