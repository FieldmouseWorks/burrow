//! With real Nix: what the store writes is a binary cache a store that
//! trusts only the machine's key substitutes from. Needs `nix` on PATH, as
//! the flake's check has it.

use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use burrow_builds::{Entry, Key, Manifest, Store, nix32};
use sha2::{Digest, Sha256};

/// `nix` on the chroot store under `root`, with no settings but the test's.
fn nix(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("nix")
        .args(args)
        .env("HOME", dir)
        .env("NIX_CONFIG", "experimental-features = nix-command\nsubstituters =\nsandbox = false\nrequire-sigs = true")
        .env("NIX_REMOTE", "")
        .env_remove("NIX_USER_CONF_FILES")
        .output()
        .expect("nix runs")
}

fn store_of(dir: &Path, name: &str) -> String {
    format!("local?root={}", dir.join(name).display())
}

/// A path added to the store `a`, and its NAR.
fn added(dir: &Path, name: &str, text: &str) -> (String, Vec<u8>) {
    let file = dir.join(name);
    std::fs::write(&file, text).unwrap();
    let a = store_of(dir, "a");
    let out = nix(
        dir,
        &["store", "add-file", "--store", &a, &file.to_string_lossy()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    let nar = nix(dir, &["store", "dump-path", "--store", &a, &path]);
    assert!(
        nar.status.success(),
        "{}",
        String::from_utf8_lossy(&nar.stderr)
    );
    (path, nar.stdout)
}

fn entry(path: &str, nar: &[u8]) -> Entry {
    Entry {
        path: path.into(),
        nar_hash: format!("sha256:{}", nix32(&Sha256::digest(nar))),
        nar_size: nar.len() as u64,
        references: vec![],
        deriver: None,
    }
}

/// Copies `path` from the cache into the store `to`, trusting `key`.
fn substitute(dir: &Path, cache: &Path, to: &str, path: &str, key: &str) -> std::process::Output {
    nix(
        dir,
        &[
            "copy",
            "--from",
            &format!("file://{}", cache.display()),
            "--to",
            &store_of(dir, to),
            "--option",
            "trusted-public-keys",
            key,
            path,
        ],
    )
}

#[test]
fn what_the_door_writes_substitutes_under_the_machines_key_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let key = Key::open_or_make(&dir.join("secret-key"), "machine-1").unwrap();
    let store = Store {
        dir: cache.clone(),
        max_size: u64::MAX,
    };
    store.init(&key).unwrap();
    let (one, one_nar) = added(dir, "one", "built once\n");
    let (two, two_nar) = added(dir, "two", "built twice\n");
    let manifest = Manifest {
        paths: vec![entry(&one, &one_nar), entry(&two, &two_nar)],
    };
    let body = [one_nar.clone(), two_nar.clone()].concat();
    let got = store
        .upload(&manifest, &mut Cursor::new(body.clone()), &key)
        .unwrap();
    assert_eq!(got.written, [one.clone(), two.clone()]);

    // Uploaded twice, it's stored once.
    let again = store
        .upload(&manifest, &mut Cursor::new(body), &key)
        .unwrap();
    assert_eq!(again.skipped, [one.clone(), two.clone()]);
    let nars = std::fs::read_dir(cache.join("nar")).unwrap().count();
    assert_eq!(nars, 2);

    // A store that trusts another key refuses it.
    let other = Key::open_or_make(&dir.join("other-key"), "other-1").unwrap();
    let refused = substitute(dir, &cache, "b", &one, &other.public());
    assert!(
        !refused.status.success(),
        "a store trusting another key took it"
    );

    // One that trusts the machine's alone takes both, whole.
    let public = std::fs::read_to_string(cache.join("public-key")).unwrap();
    assert_eq!(public.trim(), key.public());
    for path in [&one, &two] {
        let out = substitute(dir, &cache, "b", path, public.trim());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let b = store_of(dir, "b");
    let verified = nix(
        dir,
        &[
            "store",
            "verify",
            "--store",
            &b,
            "--sigs-needed",
            "1",
            "--option",
            "trusted-public-keys",
            public.trim(),
            &one,
            &two,
        ],
    );
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}
