//! The shell: the stylesheet, the script, the icons (made from the house
//! mark) and the fonts, each served under the shell's version so a browser
//! may keep it for good, and the service worker that caches them. They're
//! read at start from the directories the module gives (this crate's
//! assets/, and its fonts derivation), every one of them, or it won't start.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The shell's files in assets/, as they're served.
pub const SHELL: &[&str] = &[
    "app.css",
    "app.js",
    "mark-128.png",
    "favicon-32.png",
    "apple-touch-icon.png",
    "icon-192.png",
    "icon-512.png",
    "icon-maskable-512.png",
];

/// The house fonts, as modules/dashboard/fonts.nix names them; served
/// under fonts/.
pub const FONTS: &[&str] = &[
    "big-shoulders-display.woff2",
    "source-serif-4-regular.woff2",
    "source-serif-4-italic.woff2",
    "source-serif-4-semibold.woff2",
    "ibm-plex-mono-regular.woff2",
    "ibm-plex-mono-medium.woff2",
];

/// The service worker's source in assets/, which `service_worker` fills in.
const SERVICE_WORKER: &str = "sw.js";

pub struct Assets {
    /// A digest of everything below: a new shell is a new version.
    pub version: String,
    files: HashMap<String, Vec<u8>>,
    worker: String,
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

impl Assets {
    /// The shell from `shell` and the fonts from `fonts`.
    pub fn load(shell: &Path, fonts: &Path) -> Result<Assets, String> {
        let mut files = HashMap::new();
        for name in SHELL {
            files.insert((*name).to_owned(), read(&shell.join(name))?);
        }
        for name in FONTS {
            files.insert(format!("fonts/{name}"), read(&fonts.join(name))?);
        }
        let worker = String::from_utf8(read(&shell.join(SERVICE_WORKER))?)
            .map_err(|e| format!("{SERVICE_WORKER}: {e}"))?;
        let mut names: Vec<&String> = files.keys().collect();
        names.sort();
        let mut hash = Sha256::new();
        for name in names {
            hash.update(name.as_bytes());
            hash.update([0]);
            hash.update(&files[name]);
        }
        hash.update(worker.as_bytes());
        let version = hash
            .finalize()
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Assets {
            version,
            files,
            worker,
        })
    }

    /// The file `name` and its type.
    pub fn get(&self, name: &str) -> Option<(&[u8], &'static str)> {
        let bytes = self.files.get(name)?;
        Some((bytes, content_type(name)))
    }

    /// Where `name` is served.
    pub fn url(&self, name: &str) -> String {
        format!("/assets/{}/{name}", self.version)
    }

    /// The service worker, told its version and the shell it caches: every
    /// asset and the offline page, never a page with data on it.
    pub fn service_worker(&self) -> String {
        let mut shell: Vec<String> = self.files.keys().map(|n| self.url(n)).collect();
        shell.sort();
        shell.push("/offline".into());
        let shell = serde_json::to_string(&shell).unwrap_or_default();
        self.worker
            .replace("\"@VERSION@\"", &format!("{:?}", self.version))
            .replace("[\"@SHELL@\"]", &shell)
    }

    /// The web app manifest.
    pub fn manifest(&self, title: &str) -> String {
        serde_json::json!({
            "name": format!("{title} dashboard"),
            "short_name": title,
            "description": format!("{title}, from the owner's devices"),
            "id": "/",
            "start_url": "/",
            "scope": "/",
            "display": "standalone",
            "background_color": "#0b0b0b",
            "theme_color": "#0b0b0b",
            "icons": [
                { "src": self.url("icon-192.png"), "sizes": "192x192", "type": "image/png", "purpose": "any" },
                { "src": self.url("icon-512.png"), "sizes": "512x512", "type": "image/png", "purpose": "any" },
                { "src": self.url("icon-maskable-512.png"), "sizes": "512x512", "type": "image/png", "purpose": "maskable" },
            ],
        })
        .to_string()
    }
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// This crate's shell.
    pub fn shell() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
    }

    /// A fonts directory with every font in it (stand-ins).
    pub fn fonts() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in FONTS {
            fs::write(dir.path().join(name), name.as_bytes()).unwrap();
        }
        dir
    }

    #[test]
    fn every_file_of_the_shell_has_to_be_there() {
        let fonts = fonts();
        let dir = tempfile::tempdir().unwrap();
        for name in SHELL.iter().chain([&SERVICE_WORKER]) {
            burrow_test::copy_file(shell().join(name), dir.path().join(name));
        }
        assert!(Assets::load(dir.path(), fonts.path()).is_ok());
        fs::remove_file(dir.path().join(SERVICE_WORKER)).unwrap();
        let err = Assets::load(dir.path(), fonts.path()).err().unwrap();
        assert!(err.contains(SERVICE_WORKER), "{err}");
    }

    #[test]
    fn every_font_has_to_be_there() {
        let dir = fonts();
        assert!(Assets::load(shell(), dir.path()).is_ok());
        fs::remove_file(dir.path().join(FONTS[0])).unwrap();
        let err = Assets::load(shell(), dir.path()).err().unwrap();
        assert!(err.contains(FONTS[0]), "{err}");
    }

    #[test]
    fn the_worker_caches_the_shell_and_the_offline_page_only() {
        let dir = fonts();
        let a = Assets::load(shell(), dir.path()).unwrap();
        let sw = a.service_worker();
        assert!(sw.contains(&format!("{:?}", a.version)));
        assert!(!sw.contains("@VERSION@") && !sw.contains("@SHELL@"));
        assert!(sw.contains(&a.url("app.css")));
        assert!(sw.contains(&a.url("fonts/ibm-plex-mono-regular.woff2")));
        assert!(sw.contains("\"/offline\""));
        assert!(!sw.contains("\"/\""));
    }

    #[test]
    fn the_version_follows_the_files() {
        let dir = fonts();
        let a = Assets::load(shell(), dir.path()).unwrap();
        fs::write(dir.path().join(FONTS[1]), b"changed").unwrap();
        let b = Assets::load(shell(), dir.path()).unwrap();
        assert_ne!(a.version, b.version);
        assert_eq!(a.get("app.css").unwrap().1, "text/css; charset=utf-8");
        assert!(a.get("../etc/passwd").is_none());
    }

    #[test]
    fn the_manifest_installs() {
        let dir = fonts();
        let a = Assets::load(shell(), dir.path()).unwrap();
        let m: serde_json::Value = serde_json::from_str(&a.manifest("box")).unwrap();
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["start_url"], "/");
        assert_eq!(m["short_name"], "box");
        let icons = m["icons"].as_array().unwrap();
        assert!(icons.iter().any(|i| i["purpose"] == "maskable"));
        for i in icons {
            let src = i["src"].as_str().unwrap();
            let name = src.rsplit('/').next().unwrap();
            assert!(a.get(name).is_some(), "{src}");
        }
    }
}
