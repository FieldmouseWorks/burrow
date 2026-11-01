//! Names and paths `agent new` is given: a project, the paths `--with`
//! shares, and the id a sandbox gets from its project's name.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Whether `p` names a folder directly in the projects folder and nothing
/// else: the name goes into the share paths.
pub fn project_name(p: &str) -> bool {
    !(p.is_empty() || p == "." || p == ".." || p.contains('/'))
}

/// How many links `resolve` follows before it looks for a loop, as
/// coreutils' `realpath -m` does.
const LINKS_BEFORE_LOOPS: u32 = 20;
/// How many links `resolve` follows at most: the kernel's own limit for one
/// path. `realpath -m` has none, so it never ends on a link that makes the
/// path longer each time it's followed (`a` to `a/a`).
const MAX_LINKS: u32 = 40;

/// `p` as `realpath -m` makes it: absolute (from `cwd`), every symlink in
/// the part that's there followed, `.` and `..` gone; the part that isn't
/// there is taken as it's written. Past the first 20 links, a link met
/// again from the same directory with the same rest of the path to go is a
/// loop, and is taken as it is. More than 40 links is an error.
pub fn resolve(p: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let mut name = p.as_os_str().as_bytes().to_vec();
    let mut out = if p.is_absolute() {
        PathBuf::from("/")
    } else {
        cwd.to_owned()
    };
    let mut at = 0;
    let mut links = 0;
    let mut seen = HashSet::new();
    while at < name.len() {
        if name[at] == b'/' {
            at += 1;
            continue;
        }
        let end = name[at..]
            .iter()
            .position(|&b| b == b'/')
            .map_or(name.len(), |i| at + i);
        match &name[at..end] {
            b"." => {}
            b".." => {
                out.pop();
            }
            part => {
                let next = out.join(OsStr::from_bytes(part));
                if let Ok(target) = fs::read_link(&next) {
                    let looped = links >= LINKS_BEFORE_LOOPS && {
                        let dir =
                            fs::metadata(&out).map_err(|e| format!("{}: {e}", out.display()))?;
                        !seen.insert((dir.dev(), dir.ino(), name[at..].to_vec()))
                    };
                    if !looped {
                        links += 1;
                        if links > MAX_LINKS {
                            return Err("too many levels of symbolic links".to_owned());
                        }
                        let mut more = target.into_os_string().into_vec();
                        more.extend_from_slice(&name[end..]);
                        if more.first() == Some(&b'/') {
                            out = PathBuf::from("/");
                        }
                        name = more;
                        at = 0;
                        continue;
                    }
                }
                out = next;
            }
        }
        at = end;
    }
    Ok(out)
}

/// A sandbox id's first part, from its project's name: lowercase letters
/// and digits, everything else one `-` between them; `agent` when that
/// leaves nothing.
pub fn slug(project: &str) -> String {
    let mut s = String::new();
    for b in project.bytes() {
        let c = b.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c as char);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.strip_prefix('-').unwrap_or(&s);
    let s = s.strip_suffix('-').unwrap_or(s);
    if s.is_empty() {
        "agent".to_owned()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_project_is_a_name_in_the_folder() {
        for p in ["web", "My Site", ".hidden", "a..b"] {
            assert!(project_name(p), "{p}");
        }
        for p in ["", ".", "..", "a/b", "/abs", "web/"] {
            assert!(!project_name(p), "{p}");
        }
    }

    #[test]
    fn a_slug_is_the_name_in_lowercase_and_dashes() {
        for (p, s) in [
            ("web", "web"),
            ("My Site", "my-site"),
            ("--a__B--", "a-b"),
            ("a.b-c", "a-b-c"),
            ("Café", "caf"),
            ("ÉÉ", "agent"),
            ("...", "agent"),
            ("x9", "x9"),
        ] {
            assert_eq!(slug(p), s, "{p}");
        }
    }

    #[test]
    fn a_path_resolves_as_realpath_m_resolves_it() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("data/lab/vm")).unwrap();
        fs::create_dir_all(root.join("elsewhere")).unwrap();
        symlink(root.join("elsewhere"), root.join("data/out")).unwrap();
        symlink("lab", root.join("data/rel")).unwrap();
        let r = |p: &str| resolve(Path::new(p), &root.join("data")).unwrap();
        let at = |p: &str| root.join(p);
        assert_eq!(r("lab"), at("data/lab"));
        assert_eq!(r("./lab/./vm/"), at("data/lab/vm"));
        assert_eq!(r("lab/../lab/vm"), at("data/lab/vm"));
        assert_eq!(r("out"), at("elsewhere"));
        assert_eq!(r("out/x/../y"), at("elsewhere/y"));
        assert_eq!(r("rel/vm"), at("data/lab/vm"));
        assert_eq!(r("missing/../lab"), at("data/lab"));
        assert_eq!(r("missing/a/b"), at("data/missing/a/b"));
        assert_eq!(r(&format!("{}/data/lab", root.display())), at("data/lab"));
        assert_eq!(r("../../../../../../../.."), PathBuf::from("/"));
    }

    /// Links in a ring, in `dir`: `n0` to `n1` and so on, the last back to
    /// `n0`.
    fn ring(dir: &Path, n: usize) {
        fs::create_dir_all(dir).unwrap();
        for i in 0..n {
            symlink(format!("n{}", (i + 1) % n), dir.join(format!("n{i}"))).unwrap();
        }
    }

    #[test]
    fn a_loop_is_taken_as_it_is_once_it_comes_round_again() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let r = |p: &str| resolve(Path::new(p), &root);
        // As realpath -m has them: the first 20 links are followed, and the
        // first link after them that comes round again is where it stops.
        for (n, stops) in [(1, 0), (2, 0), (3, 2), (12, 8), (20, 0)] {
            let dir = root.join(format!("ring{n}"));
            ring(&dir, n);
            assert_eq!(
                r(&format!("ring{n}/n0/x")),
                Ok(dir.join(format!("n{stops}/x"))),
                "{n}"
            );
        }
        // Through a directory and back: the same link with the same rest
        // to go, met from the same directory.
        fs::create_dir_all(root.join("x")).unwrap();
        symlink("../x/l", root.join("x/l")).unwrap();
        assert_eq!(r("x/l/q"), Ok(root.join("x/l/q")));
        // No loop: one link used again with less of the path to go each
        // time, and links of one name with the same rest to go, each in a
        // directory of its own.
        symlink(".", root.join("s")).unwrap();
        assert_eq!(r(&format!("{}x", "s/".repeat(25))), Ok(root.join("x")));
        fs::create_dir_all(root.join("d25/l")).unwrap();
        for i in 0..25 {
            fs::create_dir_all(root.join(format!("d{i}"))).unwrap();
            symlink(format!("../d{}/l", i + 1), root.join(format!("d{i}/l"))).unwrap();
        }
        assert_eq!(r("d0/l/x"), Ok(root.join("d25/l/x")));
        // 40 links are followed; one more is too many, as it is to the
        // kernel.
        for n in [40, 41] {
            let dir = root.join(format!("chain{n}"));
            fs::create_dir_all(dir.join(format!("l{n}"))).unwrap();
            for i in 0..n {
                symlink(format!("l{}", i + 1), dir.join(format!("l{i}"))).unwrap();
            }
            let want = if n == 40 {
                Ok(dir.join("l40/z"))
            } else {
                Err("too many levels of symbolic links".to_owned())
            };
            assert_eq!(r(&format!("chain{n}/l0/z")), want, "{n}");
        }
        // A ring too long to come round within 40 links, and a link that
        // makes the path longer each time, where realpath -m never ends.
        ring(&root.join("ring21"), 21);
        symlink("g/g", root.join("g")).unwrap();
        for p in ["ring21/n0/x", "g/x"] {
            assert_eq!(
                r(p),
                Err("too many levels of symbolic links".to_owned()),
                "{p}"
            );
        }
    }
}
