//! gate-reuse: before a push to the default branch builds anything, ask the
//! caller's lookup whether CI already attested a pass of this same gate on
//! this exact tree (the crate's README, "Reusing an attested pass", is the
//! contract). Only an answer that is a hit, for this very request, naming
//! one of this repository's runs, is a hit; anything else, an error
//! included, leaves the whole gate to run.
use super::*;

/// The contract both sides name, in the request and in the answer.
pub const CONTRACT: &str = "burrow-gate-reuse/v1";
/// What a token the lookup can check this job by is for: the request's
/// digest follows, so the token vouches for this request alone.
const AUDIENCE: &str = "burrow-gate-reuse:v1:sha256:";

/// What the lookup's answer means for this run.
#[derive(Debug, PartialEq)]
pub enum Answer {
    /// The gate passed on this tree in that run: build nothing.
    Hit(String),
    /// No such pass on record: build.
    Miss,
    /// It couldn't say, or its answer can't be read: build, and say so.
    Unknown(String),
    /// Not a push to the default branch, by its exact name: build, as
    /// without gate-reuse.
    NotAsked(String),
}

/// The step: ask, and say what the answer was. A miss is a success, as the
/// gate then runs as it always has; an answer it can't read or trust is a
/// soft failure (degraded/gate-reuse), and the gate runs too.
pub fn reuse() -> Result<bool> {
    let work = Work::new("reuse")?;
    let answer = ask(&work.0).unwrap_or_else(|e| Answer::Unknown(e.to_string()));
    match answer {
        Answer::Hit(run) => {
            println!("gate-reuse: this gate already passed on this tree, attested, in {run}");
            append_output("hit", "true")?;
            append_output("run", &run)?;
            Ok(true)
        }
        Answer::Miss => {
            println!("gate-reuse: no attested pass of this gate on this tree, so it runs");
            append_output("hit", "false")?;
            Ok(true)
        }
        Answer::NotAsked(why) => {
            println!("gate-reuse: nothing to ask ({why}), so the gate runs");
            append_output("hit", "false")?;
            Ok(true)
        }
        Answer::Unknown(why) => {
            println!("gate-reuse: can't tell ({why}), so the gate runs");
            Ok(false)
        }
    }
}

fn append_output(k: &str, v: &str) -> Result<()> {
    use std::fs::OpenOptions;
    writeln!(
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(var("GITHUB_OUTPUT"))?,
        "{k}={v}"
    )?;
    Ok(())
}

/// Asks the lookup, from the step's environment: the answer, or why there
/// is none.
fn ask(work: &Path) -> Result<Answer> {
    let lookup = var("LOOKUP");
    if !lookup_url(&lookup) {
        return Ok(Answer::Unknown(format!(
            "gate-reuse-lookup is {lookup:?}, not an https URL (or http on this machine)"
        )));
    }
    // Only the default branch, by its exact name: an expression's == ignores
    // case, and git's branch names don't.
    let (git_ref, default) = (var("GITHUB_REF"), var("DEFAULT_BRANCH"));
    if default.is_empty() || git_ref != format!("refs/heads/{default}") {
        return Ok(Answer::NotAsked(format!(
            "{git_ref} isn't the default branch, {default}"
        )));
    }
    let (repository, commit, tree) = (var("GITHUB_REPOSITORY"), var("COMMIT"), var("TREE"));
    // The push's own commit and its tree, as the event names them: the tree
    // the gate would check.
    if !object_id(&commit) || !object_id(&tree) || var("HEAD_ID") != commit {
        return Ok(Answer::Unknown(format!(
            "the push names no tree for {commit:?}"
        )));
    }
    let gate =
        fs::read(temp().join("gate.json")).map_err(|e| err(format!("no gate identity ({e})")))?;
    let body = request(&repository, &commit, &tree, &gate)?;
    let query = sha256(body.as_bytes())?;
    println!("gate-reuse: asking {lookup} about tree {tree}, request sha256 {query}:\n{body}");
    let request = work.join("request.json");
    fs::write(&request, &body)?;
    let mut headers = String::from("Content-Type: application/json\n");
    headers.push_str(&format!("Authorization: Bearer {}\n", token(work, &query)?));
    let header_file = work.join("headers");
    fs::write(&header_file, headers)?;
    let reply = work.join("reply");
    let mut c = cmd(
        "curl",
        &[
            "-sS",
            "--proto",
            "=https,http",
            "--max-time",
            "60",
            "--retry",
            "2",
            "--retry-max-time",
            "120",
            "-X",
            "POST",
            "-H",
            &format!("@{}", s(&header_file)),
            "--data-binary",
            &format!("@{}", s(&request)),
            "-o",
            s(&reply),
            "-w",
            "%{http_code}",
            "--",
            &lookup,
        ],
    );
    let o = output({
        c.stdin(Stdio::null());
        c
    })?;
    if !o.status.success() {
        return Ok(Answer::Unknown(format!(
            "the lookup didn't answer: curl {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )));
    }
    let status = String::from_utf8_lossy(&o.stdout).trim().to_owned();
    if status != "200" {
        return Ok(Answer::Unknown(format!(
            "the lookup answered HTTP {status}"
        )));
    }
    let server = match var("GITHUB_SERVER_URL") {
        v if v.is_empty() => "https://github.com".to_owned(),
        v => v,
    };
    Ok(answer(&query, &server, &repository, &fs::read(&reply)?))
}

/// An https URL, or an http one on this machine itself.
/// The authority (up to the first `/`, `?` or `#`) is a bare host and maybe
/// a port: no user information, nothing curl could read as another host. An
/// http one's host is a loopback name, spelled one of three ways.
fn lookup_url(url: &str) -> bool {
    let (local, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (false, rest),
        (_, Some(rest)) => (true, rest),
        _ => return false,
    };
    if !url.bytes().all(|b| b.is_ascii_graphic()) || url.contains('\\') {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port))
            if !host.ends_with(':')
                && (host.starts_with('[') == host.ends_with(']'))
                && !port.contains(']') =>
        {
            (host, Some(port))
        }
        _ => (authority, None),
    };
    let port_ok =
        port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
    let host_ok = if local {
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
    } else {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    };
    port_ok && host_ok
}

/// A git object id, SHA-1's or SHA-256's, in lowercase hex.
fn object_id(v: &str) -> bool {
    matches!(v.len(), 40 | 64) && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The request's body: one line of JSON, its keys sorted, so its digest is
/// the request's.
fn request(repository: &str, commit: &str, tree: &str, gate: &[u8]) -> Result<String> {
    let filter = format!(
        "if type != \"object\" then error(\"the gate identity isn't an object\") else . end \
         | {{contract: {}, repository: {}, commit: {}, tree: {}, gate: .}}",
        quote(CONTRACT),
        quote(repository),
        quote(commit),
        quote(tree)
    );
    let mut child = cmd("jq", &["-cS", &filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(gate)?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        return Err(err(String::from_utf8_lossy(&o.stderr).into_owned()));
    }
    let body = text_output(o.stdout)?;
    if body.lines().count() != 1 {
        return Err(err("the gate identity isn't one JSON value"));
    }
    Ok(body)
}

fn sha256(bytes: &[u8]) -> Result<String> {
    let mut child = cmd("sha256sum", &[])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(bytes)?;
    let o = child.wait_with_output()?;
    let out = String::from_utf8(o.stdout)?;
    match out.split_whitespace().next() {
        Some(hex) if o.status.success() && hex.len() == 64 => Ok(hex.to_owned()),
        _ => Err(err("sha256sum gave no digest")),
    }
}

/// This job's OIDC token for the request whose digest is `query`: the
/// lookup can tell from it which repository and workflow ask, and that they
/// asked this. A job that can't get one asks nothing.
fn token(work: &Path, query: &str) -> Result<String> {
    let (url, bearer) = (
        var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
    );
    if url.is_empty() || bearer.is_empty() {
        return Err(err("no OIDC token in this job to sign the request with"));
    }
    let header = work.join("token-headers");
    fs::write(&header, format!("Authorization: bearer {bearer}\n"))?;
    let reply = checked(cmd(
        "curl",
        &[
            "-fsS",
            "--max-time",
            "30",
            "--retry",
            "4",
            "--retry-max-time",
            "90",
            "-H",
            &format!("@{}", s(&header)),
            "--",
            &format!("{url}&audience={AUDIENCE}{query}"),
        ],
    ))
    .map_err(|e| err(format!("no OIDC token for the request: {e}")))?;
    let token = jq(
        &reply,
        ".value | if type == \"string\" then . else error(\"no token\") end",
    )?;
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(err("the OIDC token isn't one"));
    }
    Ok(token)
}

/// What the lookup's reply to the request whose digest is `query` says,
/// for `repository` on `server`. Only a reply that is one JSON object,
/// under this contract, answering this request, whose `evidence` is `hit`
/// and whose `run` is one of the repository's runs, is a hit.
pub fn answer(query: &str, server: &str, repository: &str, reply: &[u8]) -> Answer {
    let filter = r#"if length != 1 then error("not one JSON value") else .[0] end
        | if type != "object" then error("not a JSON object") else . end
        | [(.contract, .query, .evidence, .run)
           | if type == "string" then . elif . == null then "" else error("a field that isn't a string") end]
        | @tsv"#;
    let mut child = match cmd("jq", &["-sr", filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Answer::Unknown(format!("can't read the answer: {e}")),
    };
    let fed = child.stdin.take().unwrap().write_all(reply);
    let o = match (fed, child.wait_with_output()) {
        (Ok(()), Ok(o)) => o,
        (Err(e), _) | (_, Err(e)) => return Answer::Unknown(format!("can't read the answer: {e}")),
    };
    let line = String::from_utf8_lossy(&o.stdout);
    let fields: Vec<&str> = line
        .strip_suffix('\n')
        .unwrap_or(&line)
        .split('\t')
        .collect();
    let [contract, answered, evidence, run] = fields[..] else {
        return Answer::Unknown(format!(
            "its answer isn't one: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    };
    if !o.status.success() {
        return Answer::Unknown("its answer isn't one".into());
    }
    if contract != CONTRACT {
        return Answer::Unknown(format!("it answers under {contract:?}, not {CONTRACT}"));
    }
    if answered != query {
        return Answer::Unknown(format!("it answers request {answered:?}, not {query}"));
    }
    match evidence {
        "hit" if run_of(run, server, repository) => Answer::Hit(run.to_owned()),
        "hit" => Answer::Unknown(format!(
            "it says hit, but names {run:?}, not a run of {repository}"
        )),
        "miss" => Answer::Miss,
        "unknown" => Answer::Unknown("it says it can't tell".into()),
        other => Answer::Unknown(format!("it says {other:?}")),
    }
}

/// `{server}/{repository}/actions/runs/{id}`, maybe with `/attempts/{n}` or
/// `/job/{id}` after it: a run, or a job, of the repository's.
fn run_of(url: &str, server: &str, repository: &str) -> bool {
    let prefix = format!("{server}/{repository}/actions/runs/");
    let Some(rest) = url
        .get(..prefix.len())
        .filter(|p| p.eq_ignore_ascii_case(&prefix))
        .and_then(|_| url.get(prefix.len()..))
    else {
        return false;
    };
    let number = |v: &str| !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit());
    match rest.split('/').collect::<Vec<_>>()[..] {
        [run] => number(run),
        [run, "attempts" | "job", n] => number(run) && number(n),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: &str = "0f0e";
    const SERVER: &str = "https://github.com";
    const REPO: &str = "owner/repo";

    fn says(reply: &str) -> Answer {
        answer(Q, SERVER, REPO, reply.as_bytes())
    }

    fn hit(run: &str) -> String {
        format!(
            r#"{{"contract": "{CONTRACT}", "query": "{Q}", "evidence": "hit", "run": "{run}"}}"#
        )
    }

    #[test]
    fn only_a_hit_for_this_request_naming_this_repos_run_is_one() {
        let run = "https://github.com/owner/repo/actions/runs/42";
        assert_eq!(says(&hit(run)), Answer::Hit(run.into()));
        for run in [
            "https://github.com/owner/repo/actions/runs/42/attempts/2",
            "https://github.com/owner/repo/actions/runs/42/job/7",
            "https://github.com/Owner/Repo/actions/runs/42",
        ] {
            assert_eq!(says(&hit(run)), Answer::Hit(run.into()), "{run}");
        }
        let miss = format!(r#"{{"contract": "{CONTRACT}", "query": "{Q}", "evidence": "miss"}}"#);
        assert_eq!(says(&miss), Answer::Miss);
    }

    #[test]
    fn anything_else_is_unknown() {
        let run = "https://github.com/owner/repo/actions/runs/42";
        let unknown = |reply: &str| {
            assert!(
                matches!(says(reply), Answer::Unknown(_)),
                "{reply}: {:?}",
                says(reply)
            );
        };
        for reply in [
            "",
            "not json",
            "[]",
            "null",
            "\"hit\"",
            &format!("{} {}", hit(run), hit(run)),
            &hit(run).replace(Q, "beef"),
            &hit(run).replace(CONTRACT, "burrow-gate-reuse/v2"),
            &hit(run).replace("\"hit\"", "\"HIT\""),
            &hit(run).replace("\"hit\"", "true"),
            &hit(run).replace("\"hit\"", "\"unknown\""),
            &hit(run).replace("\"hit\"", "\"pass\""),
            &format!(r#"{{"contract": "{CONTRACT}", "query": "{Q}", "evidence": "hit"}}"#),
            &format!(
                r#"{{"contract": "{CONTRACT}", "query": "{Q}", "evidence": "hit", "run": 42}}"#
            ),
            &format!(r#"{{"query": "{Q}", "evidence": "hit", "run": "{run}"}}"#),
            &format!(r#"{{"contract": "{CONTRACT}", "evidence": "hit", "run": "{run}"}}"#),
            &format!(
                r#"{{"contract": "{CONTRACT}", "query": ["{Q}"], "evidence": "hit", "run": "{run}"}}"#
            ),
            &hit("https://github.com/owner/other/actions/runs/42"),
            &hit("https://github.com/owner/repo/actions/runs/"),
            &hit("https://github.com/owner/repo/actions/runs/42/logs"),
            &hit("https://github.com/owner/repo/actions/runs/42?x=1"),
            &hit("https://github.com/owner/repo/actions/runs/4\\t2"),
            &hit("https://github.com.evil/owner/repo/actions/runs/42"),
            &hit("http://github.com/owner/repo/actions/runs/42"),
            &hit("https://github.com/owner/repo/actions/runs/42/job/"),
        ] {
            unknown(reply);
        }
    }

    #[test]
    fn a_lookup_is_https_or_this_machines() {
        for url in [
            "https://conary.example/gate",
            "http://127.0.0.1:8080/gate",
            "http://localhost/gate",
            "http://[::1]:9/gate",
            "http://[::1]",
            "https://conary.example:8443/gate?x=1",
        ] {
            assert!(lookup_url(url), "{url}");
        }
        for url in [
            "",
            "https://",
            "http://conary.example/gate",
            "http://localhost.evil/gate",
            "http://127.0.0.10/gate",
            "file:///etc/passwd",
            "ftp://x",
            "http://localhost:80@lookup.example/gate",
            "http://localhost@lookup.example/gate",
            "http://127.0.0.1:x/gate",
            "http://127.0.0.1:/gate",
            "http://127.0.0.1:123456/gate",
            "http://[::1]x/gate",
            "http://[::1/gate",
            "http://localhost\\@lookup.example/gate",
            "http://localhost /gate",
            "https://user@conary.example/gate",
            "https://user:pw@conary.example/gate",
            "https://conary.example:/gate",
            "https://:443/gate",
            "HTTP://localhost/gate",
        ] {
            assert!(!lookup_url(url), "{url}");
        }
    }

    #[test]
    fn the_request_is_one_sorted_line_with_the_gate_as_given() {
        let body = request(
            REPO,
            &"a".repeat(40),
            &"b".repeat(40),
            br#"{"z": 1, "a": [2]}"#,
        )
        .unwrap();
        assert_eq!(
            body,
            format!(
                r#"{{"commit":"{}","contract":"{CONTRACT}","gate":{{"a":[2],"z":1}},"repository":"{REPO}","tree":"{}"}}"#,
                "a".repeat(40),
                "b".repeat(40)
            )
        );
        assert!(request(REPO, "a", "b", b"[1]").is_err());
        assert!(request(REPO, "a", "b", b"{} {}").is_err());
        assert!(request(REPO, "a", "b", b"").is_err());
    }
}
