//! Freeze the checkout exactly as git-backed flakes see it, including dirty
//! tracked files, config with includes resolved, index and checked submodules.
use super::*;
fn git(top: &Path, args: &[&str]) -> Command {
    let mut c = cmd("git", &["-C", s(top)]);
    c.args(args);
    c
}
/// The branch HEAD names, if it names one.
fn head_reference(top: &Path) -> Result<String> {
    let reference = output(git(top, &["symbolic-ref", "-q", "HEAD"]))?;
    Ok(if reference.status.success() {
        text_output(reference.stdout)?
    } else {
        String::new()
    })
}
/// What a checkout's relative submodule URLs resolve against, as git
/// resolves them: its branch's remote's URL, or origin's, or with neither
/// the checkout itself; and whether its config says so.
pub fn submodule_base(top: &Path) -> Result<(String, bool)> {
    let reference = head_reference(top)?;
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
    let remote = text_output(remote.stdout)?;
    let remote = if remote.is_empty() { "origin" } else { &remote };
    for key in [
        format!("remote.{remote}.url"),
        "remote.origin.url".to_owned(),
    ] {
        let url = output(git(top, &["config", &key]))?;
        if url.status.success() {
            return Ok((text_output(url.stdout)?, true));
        }
    }
    Ok((s(top).to_owned(), false))
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
    let reference = head_reference(top)?;
    if !submodule_base(top)?.1 {
        checked(cmd(
            "git",
            &["config", "-f", s(&config), "remote.origin.url", s(top)],
        ))?;
    }
    // Alternate files are line-oriented, and libgit2 does not decode Git's
    // quoted paths. Keep caller path bytes in a filesystem link instead of
    // putting them into that format; new objects still stay in this snapshot.
    std::os::unix::fs::symlink(
        Path::new(&common).join("objects"),
        to.join(".git/borrowed-objects"),
    )?;
    fs::write(
        to.join(".git/objects/info/alternates"),
        "../borrowed-objects\n",
    )?;
    if Path::new(&common).join("shallow").is_file() {
        fs::copy(Path::new(&common).join("shallow"), to.join(".git/shallow"))?;
    }
    let rev = output(git(top, &["rev-parse", "-q", "--verify", "HEAD^{commit}"]))?;
    if rev.status.success() {
        let rev = text_output(rev.stdout)?;
        if !reference.is_empty() {
            checked(git(to, &["symbolic-ref", "HEAD", &reference]))?;
            checked(git(to, &["update-ref", &reference, &rev]))?;
        } else {
            checked(git(to, &["update-ref", "--no-deref", "HEAD", &rev]))?;
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
