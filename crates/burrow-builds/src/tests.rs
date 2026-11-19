use std::io::Cursor;
use std::time::{Duration, SystemTime};

use super::*;

const A: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a";
const B: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b";
const C: &str = "/nix/store/cccccccccccccccccccccccccccccccc-c";

pub(crate) fn entry(path: &str, nar: &[u8]) -> Entry {
    Entry {
        path: path.into(),
        nar_hash: format!("sha256:{}", nix32(&Sha256::digest(nar))),
        nar_size: nar.len() as u64,
        references: vec![],
        deriver: None,
    }
}

/// The tests' key, as Nix writes a secret one.
pub(crate) fn secret() -> String {
    let seed = [7; 32];
    let public = SigningKey::from_bytes(&seed).verifying_key();
    format!(
        "test-1:{}",
        STANDARD.encode([&seed[..], public.as_bytes()].concat())
    )
}

pub(crate) fn key() -> Key {
    Key::parse(&secret()).unwrap()
}

fn store(max_size: u64) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store {
        dir: dir.path().into(),
        max_size,
    };
    store.init(&key()).unwrap();
    (dir, store)
}

fn up(store: &Store, nars: &[(&str, &[u8])]) -> Result<Uploaded, Stopped> {
    let manifest = Manifest {
        paths: nars.iter().map(|(p, n)| entry(p, n)).collect(),
    };
    let body: Vec<u8> = nars.iter().flat_map(|(_, n)| n.to_vec()).collect();
    store.upload(&manifest, &mut Cursor::new(body), &key())
}

#[test]
fn nix32_is_nixs_base32() {
    // `nix hash convert --to nix32` of the empty string's SHA-256.
    assert_eq!(
        nix32(&Sha256::digest(b"")),
        "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
    );
}

#[test]
fn a_manifest_names_store_paths_once_with_nixs_hashes() {
    let ok = |m: Value| Manifest::parse(m.to_string().as_bytes());
    use serde_json::{Value, json};
    let e = json!({"path": A, "narHash": entry(A, b"x").nar_hash, "narSize": 1});
    assert!(ok(json!({"paths": [e]})).is_ok());
    for bad in [
        json!({"paths": [e, e]}),
        json!({"paths": [{"path": "/etc/passwd", "narHash": e["narHash"], "narSize": 1}]}),
        json!({"paths": [{"path": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-../x", "narHash": e["narHash"], "narSize": 1}]}),
        json!({"paths": [{"path": "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-a", "narHash": e["narHash"], "narSize": 1}]}),
        json!({"paths": [{"path": A, "narHash": "sha256-AAAA", "narSize": 1}]}),
        json!({"paths": [{"path": A, "narHash": e["narHash"], "narSize": 1, "references": ["/tmp/x"]}]}),
        json!({"paths": [{"path": A, "narHash": e["narHash"], "narSize": 1, "url": "x"}]}),
    ] {
        assert!(ok(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn a_key_reads_as_nix_writes_it_and_is_made_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key");
    let made = Key::open_or_make(&path, "remi-1").unwrap();
    let again = Key::open_or_make(&path, "remi-1").unwrap();
    assert_eq!(made.public(), again.public());
    assert!(made.public().starts_with("remi-1:"));
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(Key::parse("remi-1:AAAA").is_err());
}

#[test]
fn an_upload_writes_each_nar_and_its_signed_narinfo_once() {
    let (_d, store) = store(u64::MAX);
    let got = up(&store, &[(A, b"aaa"), (B, b"bbbb")]).unwrap();
    assert_eq!(got.written, [A, B]);
    let info =
        fs::read_to_string(store.dir.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo")).unwrap();
    assert!(
        info.contains(&format!(
            "StorePath: {A}\nURL: nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar\n"
        )),
        "{info}"
    );
    assert!(info.contains("\nSig: test-1:"), "{info}");
    assert_eq!(
        fs::read(store.dir.join("nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar")).unwrap(),
        b"aaa"
    );
    // Everything it serves, nginx can read.
    use std::os::unix::fs::PermissionsExt as _;
    for f in [
        "nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo",
    ] {
        let mode = fs::metadata(store.dir.join(f))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644, "{f}");
    }
    // Again: read and let go, nothing written twice.
    let again = up(&store, &[(A, b"aaa"), (C, b"c")]).unwrap();
    assert_eq!(
        (again.written, again.skipped),
        (vec![C.to_owned()], vec![A.to_owned()])
    );
}

#[test]
fn a_nar_that_isnt_the_manifests_is_never_written() {
    let (_d, store) = store(u64::MAX);
    let manifest = Manifest {
        paths: vec![entry(A, b"aaa"), entry(B, b"bbb")],
    };
    let err = store
        .upload(&manifest, &mut Cursor::new(b"aaaBBB".to_vec()), &key())
        .unwrap_err();
    let Stopped::Mismatch { path, done } = err else {
        panic!("{err}")
    };
    assert_eq!((path.as_str(), done.written), (B, vec![A.to_owned()]));
    assert!(!store.has(B));
    assert!(fs::read_dir(store.dir.join("nar")).unwrap().count() == 1);
    // Short of a NAR, or past the last one, the body isn't the manifest's.
    let (_d, store) = store_with(u64::MAX);
    for body in [&b"aaabb"[..], &b"aaabbbX"[..]] {
        let err = store
            .upload(&manifest, &mut Cursor::new(body.to_vec()), &key())
            .unwrap_err();
        assert!(matches!(err, Stopped::Short { .. }), "{err}");
    }
}

fn store_with(max: u64) -> (tempfile::TempDir, Store) {
    store(max)
}

/// Says nginx served `path`'s NAR `ago` seconds ago.
fn fetched(store: &Store, path: &str, ago: u64) {
    let f = store
        .dir
        .join("fetched")
        .join(format!("{}.nar", hash_part(path).unwrap()));
    fs::write(&f, "\n").unwrap();
    File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(ago))
        .unwrap();
}

#[test]
fn past_its_cap_the_store_drops_what_was_fetched_longest_ago() {
    let (_d, store) = store(u64::MAX);
    up(&store, &[(A, &[b'a'; 1000]), (B, &[b'b'; 1000])]).unwrap();
    let one = fs::metadata(store.dir.join("nar/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.nar"))
        .unwrap()
        .len()
        + fs::metadata(store.dir.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.narinfo"))
            .unwrap()
            .len();
    // A was written first but fetched since; B was fetched longer ago.
    fetched(&store, A, 10);
    fetched(&store, B, 100);
    let store = Store {
        max_size: 2 * one + 10,
        ..store
    };
    let got = up(&store, &[(C, &[b'c'; 1000])]).unwrap();
    assert_eq!(got.dropped, [B]);
    assert!(store.has(A) && store.has(C) && !store.has(B));
    assert!(
        !store
            .dir
            .join("nar/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.nar")
            .exists()
    );
    assert!(
        !store
            .dir
            .join("fetched/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.nar")
            .exists()
    );
    // Dropped, it's written again by the next run that has it, and what's
    // gone longest unfetched goes instead.
    fetched(&store, C, 5);
    let again = up(&store, &[(B, &[b'b'; 1000])]).unwrap();
    assert_eq!(
        (again.written, again.dropped),
        (vec![B.to_owned()], vec![A.to_owned()])
    );
}

#[test]
fn a_paths_signature_is_of_its_references_as_nix_prints_them() {
    // Sorted, each once, whatever order the manifest sent them in: what
    // Nix's fingerprint holds, or the signature never verifies.
    let mut sorted = entry(C, b"c");
    sorted.references = vec![A.into(), B.into()];
    let mut sent = sorted.clone();
    sent.references = vec![B.into(), A.into(), B.into()];
    assert_eq!(key().sign(&sent), key().sign(&sorted));
}
