//! What a backup holds, from `restic ls --json`: the entries at or under
//! some paths, by name, or with everything restic keeps of each.

use std::io::{BufRead, BufReader};
use std::process::Stdio;

use serde_json::Value;

use crate::Run;

/// Whether `path` is at or under any of `paths`.
pub fn under(path: &str, paths: &[String]) -> bool {
    paths.iter().any(|a| {
        let a = if a == "/" {
            ""
        } else {
            a.strip_suffix('/').unwrap_or(a)
        };
        path == a
            || path
                .strip_prefix(a)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The names of backup `id`'s entries at or under `paths`.
pub fn names(run: &Run, id: &str, paths: &[String]) -> Result<Vec<String>, String> {
    nodes(run, id, paths, |_, path| path.to_owned())
}

/// Backup `id`'s entries at or under `paths`, each as a JSON array of its
/// path, kind, mode, owner, size, times and inode, in byte order: two
/// backups of the same files list the same.
pub fn fields(run: &Run, id: &str, paths: &[String]) -> Result<Vec<String>, String> {
    let mut all = nodes(run, id, paths, |node, _| {
        let row: Vec<Value> = [
            "path", "type", "mode", "uid", "gid", "size", "mtime", "ctime", "inode",
        ]
        .iter()
        .map(|k| node.get(*k).cloned().unwrap_or(Value::Null))
        .collect();
        Value::Array(row).to_string()
    })?;
    all.sort();
    Ok(all)
}

/// The path in one of `fields`' rows.
pub fn path_of(row: &str) -> String {
    serde_json::from_str::<Value>(row)
        .ok()
        .and_then(|v| v.get(0).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| row.to_owned())
}

fn nodes(
    run: &Run,
    id: &str,
    paths: &[String],
    each: impl Fn(&Value, &str) -> String,
) -> Result<Vec<String>, String> {
    let mut child = run
        .restic()
        .args(["ls", "--json", id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("restic: {e}"))?;
    let out = child.stdout.take().ok_or("restic ls: no stdout")?;
    let mut found = Vec::new();
    let mut bad = None;
    for line in BufReader::new(out).lines() {
        let line = line.map_err(|e| format!("restic ls: {e}"))?;
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => {
                if v.get("message_type").and_then(Value::as_str) == Some("node")
                    && let Some(p) = v.get("path").and_then(Value::as_str)
                    && under(p, paths)
                {
                    found.push(each(&v, p));
                }
            }
            Err(e) => bad = bad.or(Some(format!("restic ls gave a line that isn't JSON ({e})"))),
        }
    }
    let status = child.wait().map_err(|e| format!("restic: {e}"))?;
    if !status.success() {
        return Err(format!("restic ls {id} failed ({status})"));
    }
    match bad {
        Some(e) => Err(e),
        None => Ok(found),
    }
}

/// Where two sorted lists differ, as `comm` says it: what's only in `a`,
/// then what's only in `b`.
pub fn differ<'a>(a: &'a [String], b: &'a [String]) -> (Vec<&'a String>, Vec<&'a String>) {
    let (mut i, mut j) = (0, 0);
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                only_a.push(x);
                i += 1;
            }
            (Some(_), Some(y)) | (None, Some(y)) => {
                only_b.push(y);
                j += 1;
            }
            (Some(x), None) => {
                only_a.push(x);
                i += 1;
            }
            (None, None) => break,
        }
    }
    (only_a, only_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_is_at_or_inside() {
        let paths = ["/data/dev".to_owned(), "/srv/keep it/".to_owned()];
        for yes in ["/data/dev", "/data/dev/x", "/srv/keep it", "/srv/keep it/k"] {
            assert!(under(yes, &paths), "{yes}");
        }
        for no in ["/data", "/data/devx", "/srv/keep", "/srv/keep itx", "/"] {
            assert!(!under(no, &paths), "{no}");
        }
        let root = ["/".to_owned()];
        assert!(under("/anything", &root));
        assert!(!under("", &[]));
    }

    #[test]
    fn differ_is_comm() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = s(&["a", "b", "b", "d"]);
        let b = s(&["b", "c", "d", "e"]);
        let (x, y) = differ(&a, &b);
        assert_eq!(x, [&"a".to_owned(), &"b".to_owned()]);
        assert_eq!(y, [&"c".to_owned(), &"e".to_owned()]);
        assert_eq!(differ(&a, &a), (vec![], vec![]));
    }

    #[test]
    fn a_row_says_its_path() {
        assert_eq!(path_of(r#"["/a b","file",420,0,0,3,"t","t",9]"#), "/a b");
    }
}
