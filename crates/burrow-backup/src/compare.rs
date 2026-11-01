//! (In the snapshot's namespace.) Each PATH restored under BASE/tree must be
//! exactly what the backup holds there (BASE/expected, its listing: names,
//! each ending in a NUL), and what the snapshot had: the same entries, of the
//! same kind, bytes, mode, owner, size and mtime. Names only, never
//! contents, go to the log.

use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

/// How many lines of a difference the log gets.
const SHOWN: usize = 20;

/// What a compare found: whether every path is what the snapshot had, and
/// what it says of each.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub ok: bool,
    pub said: Vec<String>,
}

/// Compares each of `paths` restored under `base` with the snapshot, whose
/// files are under `snapshot` (`""` in the snapshot's namespace, where they
/// are where they were).
pub fn compare(base: &Path, snapshot: &Path, paths: &[OsString]) -> Result<Verdict, String> {
    let listed = base.join("expected");
    let expected = fs::read(&listed).map_err(|e| format!("{}: {e}", listed.display()))?;
    let expected: Vec<&[u8]> = expected
        .split(|&b| b == 0)
        .filter(|e| !e.is_empty())
        .collect();
    let tree = base.join("tree");
    let mut v = Verdict {
        ok: true,
        said: Vec::new(),
    };
    for p in paths {
        let said = one(&tree, snapshot, &expected, p.as_bytes());
        v.ok &= said.is_ok();
        v.said.extend(said.unwrap_or_else(|e| e));
    }
    Ok(v)
}

/// One path's verdict: what to say, as the path passing or failing.
fn one(
    tree: &Path,
    snapshot: &Path,
    expected: &[&[u8]],
    p: &[u8],
) -> Result<Vec<String>, Vec<String>> {
    let p = match p.strip_suffix(b"/") {
        Some(q) if !q.is_empty() => q,
        _ => p,
    };
    let shown = String::from_utf8_lossy(p).into_owned();
    let mut want: Vec<Vec<u8>> = expected
        .iter()
        .filter(|e| at_or_under(e, p))
        .map(|e| e.to_vec())
        .collect();
    want.sort();
    if want.is_empty() {
        return Err(vec![format!("drill: {shown} isn't in the backup")]);
    }
    let restored = within(tree, p);
    let mut have = Vec::new();
    walk(&restored, &mut have);
    let t = tree.as_os_str().len();
    let mut have: Vec<Vec<u8>> = have
        .into_iter()
        .map(|h| h.as_os_str().as_bytes()[t..].to_vec())
        // `/` itself, when that's what's drilled: no backup lists it.
        .filter(|h| h != b"/")
        .collect();
    have.sort();
    if want != have {
        let mut out = vec![format!(
            "drill: restoring {shown} didn't bring back what the backup holds (< only in the backup, > only restored):"
        )];
        let lines = differ(&want, &have);
        out.extend(lines.into_iter().take(SHOWN));
        return Err(out);
    }
    let changed: Vec<String> = have
        .iter()
        .filter_map(|e| content(tree, snapshot, e))
        .collect();
    if !changed.is_empty() {
        let mut out = vec![format!(
            "drill: {shown} as restored isn't what the snapshot had:"
        )];
        out.extend(changed.into_iter().take(SHOWN));
        return Err(out);
    }
    let mut meta = Vec::new();
    for e in &want {
        let (s, r) = (
            describe(e, &within(snapshot, e)),
            describe(e, &within(tree, e)),
        );
        if s != r {
            meta.push(format!("< {s}"));
            meta.push(format!("> {r}"));
        }
    }
    if !meta.is_empty() {
        let mut out = vec![format!(
            "drill: the metadata of {shown} as restored isn't what the snapshot had (< snapshot, > restored):"
        )];
        out.extend(meta.into_iter().take(SHOWN));
        return Err(out);
    }
    Ok(vec![format!(
        "drill: {shown} is what the snapshot had ({} entries)",
        want.len()
    )])
}

fn at_or_under(e: &[u8], p: &[u8]) -> bool {
    if p == b"/" {
        return e.starts_with(b"/");
    }
    e == p || e.strip_prefix(p).is_some_and(|rest| rest.starts_with(b"/"))
}

fn path(e: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(e.to_vec()))
}

/// Where `e` was restored to, under `tree`.
fn within(tree: &Path, e: &[u8]) -> PathBuf {
    let mut t = tree.as_os_str().as_bytes().to_vec();
    t.extend_from_slice(e);
    path(&t)
}

/// `root` and everything under it, symlinks not followed.
fn walk(root: &Path, into: &mut Vec<PathBuf>) {
    let mut todo = vec![root.to_owned()];
    while let Some(p) = todo.pop() {
        let Ok(m) = fs::symlink_metadata(&p) else {
            continue;
        };
        if m.is_dir()
            && let Ok(entries) = fs::read_dir(&p)
        {
            todo.extend(entries.flatten().map(|e| e.path()));
        }
        into.push(p);
    }
}

/// What's only in `want` (`< `), then what's only in `have` (`> `): both
/// sorted.
fn differ(want: &[Vec<u8>], have: &[Vec<u8>]) -> Vec<String> {
    let show = |e: &[u8]| String::from_utf8_lossy(e).into_owned();
    let only_want = want.iter().filter(|w| have.binary_search(w).is_err());
    let only_have = have.iter().filter(|h| want.binary_search(h).is_err());
    only_want
        .map(|w| format!("< {}", show(w)))
        .chain(only_have.map(|h| format!("> {}", show(h))))
        .collect()
}

/// How `e` restored differs from the snapshot's, beyond its metadata: kind,
/// bytes, a symlink's target or a device's number. None when it doesn't.
fn content(tree: &Path, snapshot: &Path, e: &[u8]) -> Option<String> {
    let shown = String::from_utf8_lossy(e);
    let (sp, rp) = (within(snapshot, e), within(tree, e));
    let s = match fs::symlink_metadata(&sp) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Some(format!("{shown}: restored, but not in the snapshot"));
        }
        Err(err) => return Some(format!("{shown}: {err}")),
    };
    let r = match fs::symlink_metadata(&rp) {
        Ok(m) => m,
        Err(err) => return Some(format!("{shown}: {err}, restored")),
    };
    let (ks, kr) = (kind(&s), kind(&r));
    if ks != kr {
        return Some(format!(
            "{shown}: a {} in the snapshot, a {} restored",
            kind_name(ks),
            kind_name(kr)
        ));
    }
    match ks {
        'f' => match same_bytes(&sp, &rp) {
            Ok(true) => None,
            Ok(false) => Some(format!("{shown}: its bytes differ")),
            Err(err) => Some(format!("{shown}: {err}")),
        },
        'l' => match (fs::read_link(&sp), fs::read_link(&rp)) {
            (Ok(a), Ok(b)) if a == b => None,
            (Ok(_), Ok(_)) => Some(format!("{shown}: the symlink's target differs")),
            (Err(err), _) | (_, Err(err)) => Some(format!("{shown}: {err}")),
        },
        'c' | 'b' if s.rdev() != r.rdev() => Some(format!("{shown}: another device")),
        _ => None,
    }
}

/// An entry's kind, as find's `%y` has it.
fn kind(m: &Metadata) -> char {
    let t = m.file_type();
    if t.is_symlink() {
        'l'
    } else if t.is_dir() {
        'd'
    } else if t.is_file() {
        'f'
    } else if t.is_fifo() {
        'p'
    } else if t.is_socket() {
        's'
    } else if t.is_char_device() {
        'c'
    } else if t.is_block_device() {
        'b'
    } else {
        '?'
    }
}

fn kind_name(k: char) -> &'static str {
    match k {
        'f' => "file",
        'd' => "directory",
        'l' => "symlink",
        'p' => "fifo",
        's' => "socket",
        'c' => "character device",
        'b' => "block device",
        _ => "thing of no known kind",
    }
}

/// `e`'s kind, mode and owner, and a regular file's size and mtime: one
/// line, the same for a restore that's right.
fn describe(e: &[u8], at: &Path) -> String {
    let shown = String::from_utf8_lossy(e);
    match fs::symlink_metadata(at) {
        Ok(m) => {
            let k = kind(&m);
            let mut line = format!(
                "{shown} {k} {:o} {} {}",
                m.mode() & 0o7777,
                m.uid(),
                m.gid()
            );
            if k == 'f' {
                line.push_str(&format!(
                    " {} {}.{:09}",
                    m.size(),
                    m.mtime(),
                    m.mtime_nsec()
                ));
            }
            line
        }
        Err(err) => format!("{shown} ({err})"),
    }
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0; 1 << 16], vec![0; 1 << 16]);
    loop {
        let na = fill(&mut fa, &mut ba)?;
        let nb = fill(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends: how much it read.
fn fill(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
