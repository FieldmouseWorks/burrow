//! Freeze the checkout exactly as git-backed flakes see it, including dirty
//! tracked files, config with includes resolved, index and checked submodules.
use super::*;
fn git(top: &Path, args: &[&str]) -> Command {
    let mut c = cmd("git", &["-C", s(top)]);
    c.args(args);
    c
}
pub fn snapshot(top: &Path, to: &Path) -> Result<()> {
    let common = string(git(
        top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))?;
    let gitdir = string(git(
        top,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ))?;
    let format = string(git(top, &["rev-parse", "--show-object-format"]))?;
    checked(cmd(
        "git",
        &["init", "-q", &format!("--object-format={format}"), s(to)],
    ))?;
    let config = to.join(".git/config");
    fs::write(&config, [])?;
    for scope in ["--global", "--local", "--worktree"] {
        let o = output(git(top, &["config", scope, "--includes", "--list", "-z"]))?;
        // An absent global/worktree config is normal, as with git's own lookup.
        for kv in nul(&o.stdout)? {
            let (k, v) = kv.split_once('\n').unwrap_or((&kv, "true"));
            let lower = k.to_ascii_lowercase();
            if lower == "include.path"
                || (lower.starts_with("includeif.") && lower.ends_with(".path"))
                || lower == "core.worktree"
                || lower == "core.bare"
            {
                continue;
            }
            checked(cmd("git", &["config", "-f", s(&config), "--add", k, v]))?;
        }
    }
    checked(cmd(
        "git",
        &["config", "-f", s(&config), "core.bare", "false"],
    ))?;
    let reference = output(git(top, &["symbolic-ref", "-q", "HEAD"]))?;
    let reference = if reference.status.success() {
        String::from_utf8(reference.stdout)?.trim_end().to_owned()
    } else {
        String::new()
    };
    let remote = output(git(
        top,
        &[
            "config",
            &format!(
                "branch.{}.remote",
                reference.trim_start_matches("refs/heads/")
            ),
        ],
    ))?;
    let remote = String::from_utf8(remote.stdout)?;
    let remote = if remote.trim().is_empty() {
        "origin"
    } else {
        remote.trim()
    };
    if !output(git(top, &["config", &format!("remote.{remote}.url")]))?
        .status
        .success()
        && !output(git(top, &["config", "remote.origin.url"]))?
            .status
            .success()
    {
        checked(cmd(
            "git",
            &["config", "-f", s(&config), "remote.origin.url", s(top)],
        ))?;
    }
    fs::write(
        to.join(".git/objects/info/alternates"),
        format!("{common}/objects\n"),
    )?;
    if Path::new(&common).join("shallow").is_file() {
        fs::copy(Path::new(&common).join("shallow"), to.join(".git/shallow"))?;
    }
    let rev = output(git(top, &["rev-parse", "-q", "--verify", "HEAD^{commit}"]))?;
    if rev.status.success() {
        let rev = String::from_utf8(rev.stdout)?;
        if !reference.is_empty() {
            checked(git(to, &["symbolic-ref", "HEAD", &reference]))?;
            checked(git(to, &["update-ref", &reference, rev.trim()]))?;
        } else {
            checked(git(to, &["update-ref", "--no-deref", "HEAD", rev.trim()]))?;
        }
    }
    if Path::new(&gitdir).join("index").is_file() {
        fs::copy(Path::new(&gitdir).join("index"), to.join(".git/index"))?;
    }
    let files = checked(git(top, &["ls-files", "-z"]))?;
    let mut selected = Vec::new();
    use std::os::unix::ffi::OsStrExt;
    for bytes in files.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let f = std::ffi::OsStr::from_bytes(bytes);
        if fs::symlink_metadata(top.join(f))
            .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
        {
            selected.extend_from_slice(bytes);
            selected.push(0);
        }
    }
    // tar preserves symlinks and executable bits, and never follows directories.
    let mut pack = cmd("tar", &["--null", "--no-recursion", "-T", "-", "-cf", "-"])
        .current_dir(top)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut unpack = cmd("tar", &["-xpf", "-", "-C", s(to)])
        .stdin(pack.stdout.take().unwrap())
        .spawn()?;
    pack.stdin.take().unwrap().write_all(&selected)?;
    if !pack.wait()?.success() || !unpack.wait()?.success() {
        return Err(err("snapshot archive failed"));
    }
    let index = checked(git(top, &["ls-files", "-z", "-s"]))?;
    for entry in index
        .split(|b| *b == 0)
        .filter(|e| e.starts_with(b"160000 "))
    {
        let entry = std::str::from_utf8(entry)?;
        {
            let (_, f) = entry
                .split_once('\t')
                .ok_or_else(|| err("bad git index entry"))?;
            if top.join(f).join(".git").exists() {
                snapshot(&top.join(f), &to.join(f))?;
            } else {
                fs::create_dir_all(to.join(f))?;
            }
        }
    }
    Ok(())
}
