//! The upload against the door itself: crates/burrow-builds' `door::serve`
//! on a local socket, with a `cn` stand-in that admits the token `public`
//! (as a public listed repo's run), refuses `refused` as Conary's rule does
//! (exit 65) and fails on anything else, and Actions' token service as a
//! local stand-in that hands out the token a case names and keeps each
//! audience it was asked for.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use burrow_builds::door::{self, Config};
use burrow_builds::{Key, Store as Cache, audience};
use sha2::{Digest, Sha256};

use super::{BATCH, Dump, Http, Nars, Sent, batches, nix_hash, ordered, upload_in};
use crate::{Info, Store, picked};

fn sp(name: &str) -> String {
    format!("/nix/store/{name}")
}

/// A store path's name for `n`: 32 of Nix's base-32 characters, then `x`.
fn name(n: char, rest: &str) -> String {
    format!("{}-{rest}", n.to_string().repeat(32))
}

/// A store as a run's own: what it built (unsigned), what a public cache
/// gave it (signed), a derivation, and two downloads, one that something
/// built refers to and one nothing does.
struct Run {
    store: Store,
    nars: BTreeMap<String, Vec<u8>>,
}

impl Run {
    fn new() -> Run {
        let mut run = Run {
            store: Store {
                dir: "/nix/store".into(),
                paths: BTreeMap::new(),
            },
            nars: BTreeMap::new(),
        };
        let (lib, app, public, drv, src, tarball) = (
            name('a', "lib"),
            name('b', "app"),
            name('c', "glibc"),
            name('d', "app.drv"),
            name('f', "src.tar.gz"),
            name('g', "unused.tar.gz"),
        );
        run.path(&public, true, false, &[], "public");
        run.path(&src, false, true, &[], "a download");
        run.path(&tarball, false, true, &[], "a download nothing needs");
        run.path(&lib, false, false, &[&public, &src], "the lib");
        // The app refers to itself, and to the lib.
        run.path(&app, false, false, &[&app, &lib], "the app");
        run.path(&drv, false, false, &[], "Derive(...)");
        run
    }

    fn path(&mut self, name: &str, signed: bool, download: bool, refs: &[&str], nar: &str) {
        let digest = Sha256::digest(nar.as_bytes());
        self.store.paths.insert(
            sp(name),
            Info {
                signed,
                download,
                references: refs.iter().map(|r| sp(r)).collect(),
                nar_hash: format!("sha256-{}", STANDARD.encode(digest)),
                nar_size: nar.len() as u64,
            },
        );
        self.nars.insert(sp(name), nar.as_bytes().to_vec());
    }
}

impl Nars for Run {
    fn nar(&self, path: &str) -> Result<Box<dyn Read>, String> {
        Ok(Box::new(Cursor::new(self.nars[path].clone())))
    }
}

/// What an upload sends: what a run built, and the download it refers to,
/// each after what it refers to; never what a public cache signed, a
/// derivation, nor a download nothing built needs.
#[test]
fn a_run_sends_only_the_unsigned_paths_keep_picks_each_after_its_references() {
    let run = Run::new();
    let picked = picked(&run.store);
    assert_eq!(
        picked,
        [
            sp(&name('a', "lib")),
            sp(&name('b', "app")),
            sp(&name('f', "src.tar.gz"))
        ]
    );
    let order = ordered(&run.store, &picked);
    assert_eq!(
        order,
        [
            sp(&name('f', "src.tar.gz")),
            sp(&name('a', "lib")),
            sp(&name('b', "app"))
        ]
    );
}

/// No upload carries more than a batch's bytes of NARs but a path bigger
/// than that alone, and every path goes in its order.
#[test]
fn batches_stay_under_their_bytes_in_order() {
    let entries = |sizes: &[u64]| {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| burrow_builds::Entry {
                path: sp(&name(char::from(b'a' + i as u8), "x")),
                nar_hash: String::new(),
                nar_size: n,
                references: vec![],
                deriver: None,
            })
            .collect::<Vec<_>>()
    };
    let sizes = |b: Vec<Vec<burrow_builds::Entry>>| -> Vec<Vec<u64>> {
        b.iter()
            .map(|b| b.iter().map(|e| e.nar_size).collect())
            .collect()
    };
    assert_eq!(
        sizes(batches(entries(&[3, 4, 2, 9, 1, 1]), 8)),
        [vec![3, 4], vec![2], vec![9], vec![1, 1]]
    );
    assert!(batches(vec![], 8).is_empty());
    const { assert!(BATCH < 1 << 30) };
}

/// The door's form of a NAR's hash, as `nix-store -q --hash` says it, from
/// path-info's SRI.
#[test]
fn a_nar_hash_is_the_doors_form() {
    // `nix hash convert --to nix32` of the same digest.
    assert_eq!(
        nix_hash("sha256-EFUdrtf6Rn0LWIJufrmg8q99aT3jGfLvd1//zaJEufY=").unwrap(),
        "sha256:1xmr8jicvzszfzpz46g37mlpvbzjl2wpwvl2b05psipssyp1sm8h"
    );
    let nix32 = "sha256:1xmr8jicvzszfzpz46g37mlpvbzjl2wpwvl2b05psipssyp1sm8h";
    assert_eq!(nix_hash(nix32).unwrap(), nix32);
    for bad in ["sha256-AAAA", "sha512-EFUd", "md5:abc", ""] {
        assert!(nix_hash(bad).is_err(), "{bad}");
    }
}

/// A door on a local socket, as nginx hands it each upload, over a cache
/// in a directory of its own, behind a `cn` stand-in.
struct Machine {
    dir: tempfile::TempDir,
    url: String,
    tokens: String,
    /// The audiences Actions was asked for, and cn was asked about.
    asked: Arc<Mutex<Vec<String>>>,
}

impl Machine {
    /// With Actions handing out `token`.
    fn new(token: &'static str) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let key = dir.path().join("secret-key");
        let k = Key::open_or_make(&key, "machine-1").unwrap();
        Cache {
            dir: cache.clone(),
            max_size: u64::MAX,
        }
        .init(&k)
        .unwrap();
        let cn = dir.path().join("cn");
        burrow_test::executable(
            &cn,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$4\" >> {checked}\n\
                 case \"$(cat)\" in\n\
                 public) echo '{{\"repository\":\"sam/homelab\",\"visibility\":\"public\"}}' ;;\n\
                 refused) echo 'cn: the job ran its own copy of the workflow, not the signer' >&2; exit 65 ;;\n\
                 *) echo 'cn: broken' >&2; exit 1 ;;\n\
                 esac\n",
                checked = dir.path().join("checked").display()
            ),
        );
        let cfg = Config {
            dir: cache,
            key,
            key_name: "machine-1".into(),
            max_size: u64::MAX,
            repos: vec!["sam/homelab".into()],
            cn: vec![cn.display().to_string()],
        };
        // nginx: the cache's files as they are, and an upload to the door.
        let door = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/builds", door.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in door.incoming() {
                let s = s.unwrap();
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    let mut first = [0; 4];
                    s.peek(&mut first).unwrap();
                    let mut input = BufReader::new(s.try_clone().unwrap());
                    let mut output = s;
                    if &first == b"POST" {
                        door::serve(&cfg, &mut input, &mut output).unwrap();
                    } else {
                        serve_file(&cfg.dir, &mut input, &mut output);
                    }
                });
            }
        });
        let asked = Arc::new(Mutex::new(Vec::new()));
        let tokens = TcpListener::bind("127.0.0.1:0").unwrap();
        let token_url = format!(
            "http://{}/token?api-version=2.0",
            tokens.local_addr().unwrap()
        );
        let seen = asked.clone();
        std::thread::spawn(move || {
            for s in tokens.incoming() {
                answer_token(s.unwrap(), token, &seen);
            }
        });
        Machine {
            dir,
            url,
            tokens: token_url,
            asked,
        }
    }

    fn http(&self) -> Http {
        Http::new(&self.url, &self.tokens, "request-token").unwrap()
    }

    fn cache(&self) -> PathBuf {
        self.dir.path().join("cache")
    }

    fn holds(&self, path: &str) -> bool {
        Cache {
            dir: self.cache(),
            max_size: u64::MAX,
        }
        .has(path)
    }

    fn checked(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("checked"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A GET or HEAD of a file under /builds/, from the cache, as nginx serves
/// it; one connection a request.
fn serve_file(dir: &Path, input: &mut dyn BufRead, output: &mut TcpStream) {
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let mut h = String::new();
    while input.read_line(&mut h).unwrap() > 0 && h != "\r\n" {
        h.clear();
    }
    let mut words = line.split(' ');
    let (method, path) = (words.next().unwrap(), words.next().unwrap());
    let file = path.strip_prefix("/builds/").map(|f| dir.join(f));
    match file.and_then(|f| fs::read(f).ok()) {
        Some(body) => {
            write!(
                output,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            if method == "GET" {
                output.write_all(&body).unwrap();
            }
        }
        None => write!(
            output,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap(),
    }
}

/// Actions' token service: the token, for the audience asked, which it
/// keeps, once the request has its bearer.
fn answer_token(s: TcpStream, token: &str, asked: &Mutex<Vec<String>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let mut bearer = false;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        bearer |= h
            .trim()
            .eq_ignore_ascii_case("authorization: bearer request-token");
    }
    let path = line.split(' ').nth(1).unwrap_or_default();
    let aud = path.split_once("&audience=").map(|(_, a)| a.to_owned());
    let (status, body) = match aud {
        Some(a) if bearer => {
            asked.lock().unwrap().push(a);
            ("200 OK", format!("{{\"count\":1,\"value\":\"{token}\"}}"))
        }
        _ => ("401 Unauthorized", "{}".to_owned()),
    };
    let mut s = s;
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

/// An admitted run's upload, as the client sends it over HTTP: the door
/// takes each path it picked, whole, its NAR's hash checked, and nothing
/// else; each upload has a token of its own whose audience is that
/// upload's manifest, the one the door checked; and what the store holds
/// already is skipped.
#[test]
fn an_admitted_run_writes_what_it_built_and_nothing_else() {
    let run = Run::new();
    let m = Machine::new("public");
    // Two uploads: the download, then the lib and the app.
    let sent = upload_in(&run.store, &run, &m.http(), 16).unwrap();
    assert_eq!(
        sent,
        Sent {
            written: 3,
            skipped: 0,
            bytes: (10 + 7 + 7) as u64,
            batches: 2,
            refused: None,
        }
    );
    for p in picked(&run.store) {
        assert!(m.holds(&p), "{p} wasn't written");
    }
    for p in [
        name('c', "glibc"),
        name('d', "app.drv"),
        name('g', "unused.tar.gz"),
    ] {
        assert!(!m.holds(&sp(&p)), "{p} was written");
    }
    let asked = m.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_ne!(asked[0], asked[1]);
    assert!(asked.iter().all(|a| a.starts_with(burrow_builds::AUDIENCE)));
    assert_eq!(m.checked(), asked, "the door checked another audience");
    // The narinfo the store wrote is signed, and its NAR is the path's.
    let app = sp(&name('b', "app"));
    let info = fs::read_to_string(m.cache().join(format!("{}.narinfo", "b".repeat(32)))).unwrap();
    assert!(info.contains(&format!("StorePath: {app}\n")), "{info}");
    assert!(info.contains("Sig: machine-1:"), "{info}");
    let nar = fs::read(m.cache().join(format!("nar/{}.nar", "b".repeat(32)))).unwrap();
    assert_eq!(nar, run.nars[&app]);

    // One manifest is one audience, the bytes sent: the second's is the
    // lib's and the app's.
    let manifest = |paths: &[String]| {
        let entries = paths
            .iter()
            .map(|p| super::entry(&run.store, p).unwrap())
            .collect();
        serde_json::to_vec(&burrow_builds::Manifest { paths: entries }).unwrap()
    };
    let all = ordered(&run.store, &picked(&run.store));
    assert_eq!(asked[1], audience(&manifest(&all[1..])));

    // What the store holds already isn't sent again: run again, nothing is.
    let again = upload_in(&run.store, &run, &m.http(), BATCH).unwrap();
    assert_eq!((again.written, again.skipped, again.batches), (0, 3, 0));
    assert_eq!(
        m.asked.lock().unwrap().len(),
        2,
        "a token for nothing to send"
    );
    // One that holds some of it is sent the rest.
    let other = Machine::new("public");
    let lib = sp(&name('a', "lib"));
    let part = upload_in(&run.store, &run, &other.http(), 16).unwrap();
    assert_eq!(part.written, 3);
    fs::remove_file(other.cache().join(format!("{}.narinfo", "a".repeat(32)))).unwrap();
    let rest = upload_in(&run.store, &run, &other.http(), BATCH).unwrap();
    assert_eq!((rest.written, rest.skipped, rest.batches), (1, 2, 1));
    assert!(other.holds(&lib));
}

/// A job the door refuses (the signer rule, through cn) writes nothing,
/// says why, and isn't a failure: it's the door's decision, as a PR's run
/// of its own copy of the workflow gets on every push.
#[test]
fn a_refused_run_writes_nothing_and_says_why() {
    let run = Run::new();
    let m = Machine::new("refused");
    let sent = upload_in(&run.store, &run, &m.http(), BATCH).unwrap();
    assert_eq!(sent.written + sent.skipped + sent.batches, 0);
    let why = sent.refused.as_deref().unwrap();
    assert!(why.contains("not the signer"), "{why}");
    assert!(super::summary(&sent).contains("not the signer"));
    for p in picked(&run.store) {
        assert!(!m.holds(&p));
    }
}

/// A store that fails, or that isn't there, fails the upload: that's the
/// degradation, never the gate's.
#[test]
fn a_store_that_fails_or_isnt_there_fails_the_upload() {
    let run = Run::new();
    let m = Machine::new("broken");
    let e = upload_in(&run.store, &run, &m.http(), BATCH).unwrap_err();
    assert!(e.contains("HTTP 500") && e.contains("broken"), "{e}");

    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/builds", closed.local_addr().unwrap());
    drop(closed);
    let door = Http::new(&url, &m.tokens, "request-token").unwrap();
    let e = upload_in(&run.store, &run, &door, BATCH).unwrap_err();
    assert!(e.contains("didn't answer"), "{e}");
}

/// The token goes to the store's URL, so it's https, or a test's own
/// machine.
#[test]
fn the_store_is_https() {
    for ok in ["https://example.org/builds", "http://127.0.0.1:8080/builds"] {
        assert!(Http::new(ok, "https://t/?a=1", "b").is_ok(), "{ok}");
    }
    for bad in [
        "http://example.org/builds",
        "ftp://example.org/builds",
        "example.org",
        "http://127.0.0.1.example.org/",
    ] {
        assert!(Http::new(bad, "https://t/?a=1", "b").is_err(), "{bad}");
    }
}

/// `nix-store --dump` with real nix: a NAR whose hash is the one path-info
/// gives, in the door's form. Needs nix on PATH, as the flake's check has.
#[test]
fn a_dump_is_the_nar_path_info_hashes() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("f");
    fs::write(&file, "built here\n").unwrap();
    let nix = |args: &[&str]| {
        let o = std::process::Command::new("nix")
            .args(["--extra-experimental-features", "nix-command"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let store = local_store(t.path());
    let path = nix(&[
        "store",
        "add-file",
        "--store",
        &store,
        &file.display().to_string(),
    ]);
    let path = path.trim();
    let json = nix(&[
        "path-info",
        "--store",
        &store,
        "--json",
        "--json-format",
        "2",
        path,
    ]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let base = path.rsplit('/').next().unwrap();
    let sri = v["info"][base]["narHash"].as_str().unwrap();
    // Dump reads nix's own store; this test's is a chroot one.
    let nar = std::process::Command::new("nix")
        .args(["--extra-experimental-features", "nix-command"])
        .args(["store", "dump-path", "--store", &store, path])
        .output()
        .unwrap();
    assert!(
        nar.status.success(),
        "{}",
        String::from_utf8_lossy(&nar.stderr)
    );
    let nar = nar.stdout;
    let want = nix_hash(sri).unwrap();
    let got = format!("sha256:{}", burrow_builds::nix32(&Sha256::digest(&nar)));
    assert_eq!(got, want);
    // And Dump, on a path the machine's store has, ends cleanly; on one it
    // hasn't, fails rather than end short.
    let mut out = Vec::new();
    let missing = sp(&name('z', "nowhere"));
    assert!(Dump.nar(&missing).unwrap().read_to_end(&mut out).is_err());
}

fn local_store(dir: &Path) -> String {
    format!("local?root={}", dir.join("root").display())
}
