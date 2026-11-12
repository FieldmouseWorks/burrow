//! Renders the live view of a snapshot file to HTML, for the Playwright
//! check (checks/dashboard-live.nix): `render-live SNAPSHOT SHELL OUT`.
//! The shell's stylesheet is linked beside OUT.

use std::{env, fs, path::Path};

use burrow_dashboard::assets::Assets;
use burrow_dashboard::pages::{Frame, live};

fn main() {
    let a: Vec<String> = env::args().collect();
    let [_, snap, shell, out] = &a[..] else {
        eprintln!("usage: render-live SNAPSHOT SHELL OUT");
        std::process::exit(2);
    };
    let snap = serde_json::from_slice(&fs::read(snap).expect("snapshot")).expect("snapshot json");
    let fonts = Path::new(shell).join("fonts");
    let assets = Assets::load(Path::new(shell), &fonts).expect("assets");
    let f = Frame {
        title: "remi",
        host: "remi.ts.net",
        assets: &assets,
    };
    let html = live::live(&f, &snap, snap_at(&snap) + 60).into_string();
    fs::write(out, html).expect("write");
}

fn snap_at(s: &burrow_proj::snapshot::Snapshot) -> u64 {
    s.at as u64
}
