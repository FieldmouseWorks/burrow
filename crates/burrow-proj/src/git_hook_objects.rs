//! Preserve byte paths while giving the isolated check stable alternates.
use std::{
    ffi::OsString,
    os::unix::{ffi::OsStringExt, fs::symlink},
    path::{Path, PathBuf},
};

pub fn install(repo: &Path, mut primary: Vec<u8>, counts: &[u8]) -> Result<(), String> {
    if primary.pop() != Some(b'\n') {
        return Err("missing object path terminator".into());
    }
    let mut paths = vec![primary];
    for line in counts.split(|b| *b == b'\n') {
        if let Some(path) = line.strip_prefix(b"alternate: ") {
            paths.push(unquote(path)?);
        }
    }
    let mut alternates = String::new();
    for (i, path) in paths.into_iter().enumerate() {
        let link = repo.join(format!("source-objects-{i}"));
        symlink(PathBuf::from(OsString::from_vec(path)), &link).map_err(|e| e.to_string())?;
        alternates.push_str(link.to_str().ok_or("temporary path isn't UTF-8")?);
        alternates.push('\n');
    }
    std::fs::write(repo.join("objects/info/alternates"), alternates).map_err(|e| e.to_string())
}

/// Git's quote_c_style output used by count-objects for alternate paths.
fn unquote(path: &[u8]) -> Result<Vec<u8>, String> {
    if !path.starts_with(b"\"") {
        return Ok(path.to_vec());
    }
    let path = path
        .strip_prefix(b"\"")
        .and_then(|p| p.strip_suffix(b"\""))
        .ok_or("unterminated quoted object path")?;
    let mut result = Vec::new();
    let mut bytes = path.iter().copied();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            result.push(b);
            continue;
        }
        let escaped = bytes.next().ok_or("truncated object path escape")?;
        result.push(match escaped {
            b'a' => 7,
            b'b' => 8,
            b't' => 9,
            b'n' => 10,
            b'v' => 11,
            b'f' => 12,
            b'r' => 13,
            b'\\' => b'\\',
            b'"' => b'"',
            b'0'..=b'3' => {
                let a = bytes
                    .next()
                    .filter(|b| (b'0'..=b'7').contains(b))
                    .ok_or("invalid octal object path")?;
                let b = bytes
                    .next()
                    .filter(|b| (b'0'..=b'7').contains(b))
                    .ok_or("invalid octal object path")?;
                (escaped - b'0') * 64 + (a - b'0') * 8 + (b - b'0')
            }
            _ => return Err("invalid object path escape".into()),
        });
    }
    Ok(result)
}
