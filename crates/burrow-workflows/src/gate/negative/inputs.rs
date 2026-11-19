//! The flake's inputs, when the negative tree's evaluation fails and tries
//! again in a store apart. That store gets every input that can be fetched,
//! each on its own: a host out of reach decides nothing, and a second try
//! that reaches for an input it wasn't given can't tell.

use std::fs;
use std::path::PathBuf;

use super::Repo;
use crate::harness::{read, write};

/// A tarball of one directory holding `x.txt`, its name, so no two are one
/// store path, named `name.tar.gz` in `www`.
fn tarball(r: &Repo, name: &str) -> PathBuf {
    let www = r.g.s.join("www");
    write(&www.join(name).join("x.txt"), &format!("{name}\n"));
    let mut tar = r.g.s.command("tar");
    tar.args(["czf", &format!("{name}.tar.gz"), name])
        .current_dir(&www);
    assert!(crate::harness::output(tar).status.success());
    www.join(format!("{name}.tar.gz"))
}

/// A file served over HTTP as often as it's asked for, at the URL returned.
fn serve(file: PathBuf) -> String {
    let body = fs::read(&file).unwrap();
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/{}",
        server.local_addr().unwrap(),
        file.file_name().unwrap().to_str().unwrap()
    );
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for mut conn in server.incoming().flatten() {
            let mut request = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            while request.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(head.as_bytes());
            let _ = conn.write_all(&body);
        }
    });
    url
}

/// nix in the repo's main, with a fetchers' cache of its own, so the job's
/// is none the wiser.
fn nix(r: &Repo, args: &[&str]) -> String {
    let s = &r.g.s;
    let mut cmd = s.command(args[0]);
    cmd.args(&args[1..])
        .current_dir(&r.origin)
        .env("NIX_CONFIG", &r.g.nix_config)
        .env("XDG_CACHE_HOME", s.join("lock-cache"));
    let out = crate::harness::output(cmd);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Main's flake with these inputs (`name = url`, each `flake = false`),
/// each one passed to the test file's own check too, locked and committed.
fn with_inputs(r: &Repo, inputs: &[(&str, &str)]) {
    let names: Vec<_> = inputs.iter().map(|(n, _)| *n).collect();
    let declared: String = inputs
        .iter()
        .map(|(n, url)| format!("  inputs.{n} = {{ url = \"{url}\"; flake = false; }};\n"))
        .collect();
    let flake = read(&r.origin.join("flake.nix"))
        .replace(
            "  outputs = { self }:",
            &format!("{declared}  outputs = {{ self, {} }}:", names.join(", ")),
        )
        .replace(
            "{ inherit drv; }",
            &format!("{{ inherit drv {}; }}", names.join(" ")),
        );
    write(&r.origin.join("flake.nix"), &flake);
    write(
        &r.origin.join("tests/check.nix"),
        "{ drv, ... }: drv \"fromtests\" [ ]\n",
    );
    r.g.s.git(&r.origin, &["add", "-A"]);
    nix(r, &["nix", "flake", "lock"]);
}

/// A flake input the store hasn't got yet is fetched by whichever
/// evaluation needs it first. When that's the negative tree's failing one,
/// its failure is still the test's own: the second try, which fetches
/// nothing, is the one held to it. And when it came from a substituter,
/// as nixpkgs does on a fresh runner, which leaves no copy the fetchers
/// keep, the second try still fetches nothing: the inputs are fetched
/// into its store first.
#[test]
fn an_input_the_failing_evaluation_fetched_first_still_counts() {
    let r = Repo::new();
    let s = &r.g.s;
    // The input: a tarball, served over HTTP as often as it's asked for.
    let url = serve(tarball(&r, "dep"));
    // Locked with a cache of its own, then collected, so neither the job's
    // store nor its fetcher cache has the input.
    with_inputs(&r, &[("dep", &url)]);
    // A binary cache holding the input, for the second run.
    let archived: serde_json::Value =
        serde_json::from_str(&nix(&r, &["nix", "flake", "archive", "--json"])).unwrap();
    let dep_path = archived["inputs"]["dep"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let bincache = format!("file://{}", s.join("bincache").display());
    nix(&r, &["nix", "copy", "--to", &bincache, &dep_path]);
    nix(&r, &["nix-store", "--gc"]);
    s.git(&r.origin, &["add", "-A"]);
    s.git(&r.origin, &["commit", "-qm", "an input"]);
    let head = r.pr(
        "needsdep",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv, dep }: builtins.seq (builtins.readFile \"${dep}/x.txt\") (throw \"needs what the change adds\")\n",
                ),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert!(ran.list("checks").contains(&"fromtests"), "{}", ran.out);
    // Again with neither the store nor the fetchers' cache holding it,
    // and the binary cache to substitute it from.
    nix(&r, &["nix-store", "--gc"]);
    fs::remove_dir_all(s.join("cache")).unwrap();
    let config = r.g.nix_config.replace(
        "substituters =",
        &format!("require-sigs = false\nsubstituters = {bincache}"),
    );
    assert_ne!(config, r.g.nix_config);
    let ran = r.run_with(&head, "", &[("NIX_CONFIG", &config)]);
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
}

/// An input nothing evaluates, whose host is out of reach, as
/// spectrum-os.org was for microvm's `spectrum` (#1212): the failing
/// evaluation never fetched it, so its second try is held to it without it,
/// and the host being down decides nothing, nor keeps out the input it
/// does read.
#[test]
fn an_input_nothing_reads_isnt_fetched_for_the_second_try() {
    let r = Repo::new();
    let s = &r.g.s;
    let dep = serve(tarball(&r, "dep"));
    let gone = tarball(&r, "gone");
    with_inputs(
        &r,
        &[
            ("dep", &dep),
            ("gone", &format!("file://{}", gone.display())),
        ],
    );
    // Its host gone, and neither the job's store nor its fetchers' cache
    // holding it.
    fs::remove_file(&gone).unwrap();
    nix(&r, &["nix-store", "--gc"]);
    s.git(&r.origin, &["add", "-A"]);
    s.git(
        &r.origin,
        &["commit", "-qm", "an input from a host that's gone"],
    );
    let head = r.pr(
        "gone",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv, dep, ... }: builtins.seq (builtins.readFile \"${dep}/x.txt\") (throw \"needs what the change adds\")\n",
                ),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert!(ran.list("checks").contains(&"fromtests"), "{}", ran.out);
    assert!(
        ran.out
            .contains("isn't in this store, so the second try isn't given it"),
        "{}",
        ran.out
    );
}

/// An input the failing evaluation does read, whose host is out of reach:
/// its failure is the fetch's, not the test's, on either try, so it can't
/// tell.
#[test]
fn an_input_the_new_test_reads_from_a_host_thats_gone_cant_tell() {
    let r = Repo::new();
    let s = &r.g.s;
    let gone = tarball(&r, "gone");
    with_inputs(&r, &[("gone", &format!("file://{}", gone.display()))]);
    fs::remove_file(&gone).unwrap();
    nix(&r, &["nix-store", "--gc"]);
    s.git(&r.origin, &["add", "-A"]);
    s.git(
        &r.origin,
        &["commit", "-qm", "an input from a host that's gone"],
    );
    let head = r.pr(
        "readsgone",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv, gone }: builtins.seq (builtins.readFile \"${gone}/x.txt\") (throw \"needs what the change adds\")\n",
                ),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("fetched on the way"), "{}", ran.out);
}
