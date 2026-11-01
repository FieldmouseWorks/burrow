//! What each source says, read into findings. Running the sources is
//! pass.rs's; this is only their output.
//!
//! - CISA's Known Exploited Vulnerabilities list: being exploited right now.
//!   An entry counts when its product is installed (by name, or as
//!   VENDOR-PRODUCT), and Linux's kernel always does.
//! - vulnix on the running system (NVD): CVSS 7 and up, the kernel aside
//!   (its fixes arrive with every LTS bump, and the exploited ones are on
//!   CISA's list).
//! - osv-scanner over a project's checkout (crates, npm, PyPI, Go...).
//!
//! Every field a key or a text is made of is written as jq's @tsv wrote it
//! (`tsv`), so the keys the shell version recorded as seen still match.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::Finding;

/// The package names in `nix-store -qR`'s list: each path's name, its
/// version (from the first `-` before a digit) dropped, in lower case.
pub fn package_names(paths: &str) -> BTreeSet<String> {
    paths
        .lines()
        .map(|line| {
            let name = store_name(line).unwrap_or(line);
            let unversioned = name
                .char_indices()
                .find(|&(i, c)| c == '-' && name[i + 1..].starts_with(|d: char| d.is_ascii_digit()))
                .map_or(name, |(i, _)| &name[..i]);
            unversioned.to_ascii_lowercase()
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// A store path's name: what follows `/nix/store/HASH-`.
fn store_name(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/nix/store/")?;
    let (hash, name) = (rest.get(..32)?, rest.get(32..)?);
    let name = name.strip_prefix('-')?;
    hash.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        .then_some(name)
}

/// A field as jq's @tsv writes it: a tab, a newline, a carriage return or a
/// backslash escaped, so no field spans lines or splits on a tab.
pub fn tsv(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// A vendor's or a product's name as KEV's are matched: lower case, and
/// nothing but letters, digits and dashes.
fn product(name: &str) -> String {
    name.chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct Kev {
    pub vulnerabilities: Vec<KevEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KevEntry {
    #[serde(rename = "cveID")]
    pub cve_id: String,
    pub vendor_project: String,
    pub product: String,
    #[serde(default)]
    pub vulnerability_name: Option<String>,
}

/// KEV's entries for what's installed, for the machine's project.
pub fn kev_findings(kev: &Kev, names: &BTreeSet<String>, project: &str) -> Vec<Finding> {
    kev.vulnerabilities
        .iter()
        .filter(|e| {
            let (v, p) = (product(&e.vendor_project), product(&e.product));
            names.contains(&p)
                || names.contains(&format!("{v}-{p}"))
                || (p == "kernel" && v == "linux")
        })
        .map(|e| {
            let cve = tsv(&e.cve_id);
            let title = tsv(e.vulnerability_name.as_deref().unwrap_or(""));
            Finding {
                project: project.to_owned(),
                key: format!("kev:{cve}"),
                text: format!("{cve} (KNOWN EXPLOITED): {title}"),
            }
        })
        .collect()
}

/// One of vulnix's packages.
#[derive(Debug, Deserialize)]
pub struct Vulnix {
    pub pname: String,
    pub name: String,
    #[serde(default)]
    pub affected_by: Vec<String>,
    #[serde(default)]
    pub cvssv3_basescore: Option<BTreeMap<String, Value>>,
}

/// vulnix's CVSS 7+ findings outside the kernel, for the machine's project.
/// A CVE without a score counts as 0.
pub fn vulnix_findings(packages: &[Vulnix], project: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    for p in packages {
        if p.pname == "linux" || p.pname.starts_with("linux-") {
            continue;
        }
        for cve in &p.affected_by {
            let score = p.cvssv3_basescore.as_ref().and_then(|s| s.get(cve));
            let Some(Value::Number(score)) = score else {
                continue;
            };
            if score.as_f64().is_none_or(|s| s < 7.0) {
                continue;
            }
            let (cve, name) = (tsv(cve), tsv(&p.name));
            found.push(Finding {
                project: project.to_owned(),
                key: format!("vulnix:{cve}:{name}"),
                text: format!("{cve} in {name} (CVSS {score})"),
            });
        }
    }
    found
}

#[derive(Debug, Deserialize)]
pub struct Osv {
    #[serde(default)]
    results: Option<Vec<OsvResult>>,
}

#[derive(Debug, Deserialize)]
struct OsvResult {
    #[serde(default)]
    packages: Option<Vec<OsvPackage>>,
}

#[derive(Debug, Deserialize)]
struct OsvPackage {
    package: OsvName,
    #[serde(default)]
    groups: Option<Vec<OsvGroup>>,
}

#[derive(Debug, Deserialize)]
struct OsvName {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OsvGroup {
    ids: Vec<String>,
    #[serde(default)]
    max_severity: Option<String>,
}

/// osv-scanner's findings in project `p`, one for each group of IDs (an
/// advisory and its aliases) in each package.
pub fn osv_findings(osv: &Osv, p: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    for r in osv.results.iter().flatten() {
        for k in r.packages.iter().flatten() {
            for g in k.groups.iter().flatten() {
                let ids = tsv(&g.ids.join(","));
                let pkg = tsv(k.package.name.as_deref().unwrap_or(""));
                let ver = tsv(k.package.version.as_deref().unwrap_or(""));
                let sev = g.max_severity.as_deref().map_or("?".to_owned(), tsv);
                found.push(Finding {
                    project: p.to_owned(),
                    key: format!("osv:{p}:{ids}:{pkg}"),
                    text: format!("{ids} in {pkg} {ver} (severity {sev})"),
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn package_names_are_store_names_without_their_versions() {
        let paths = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-openssl-3.0.15\n\
            /nix/store/0123456789abcdfghijklmnpqrsvwxyz-OpenSSH-9.9p1-bin\n\
            /nix/store/0123456789abcdfghijklmnpqrsvwxyz-python3.12-requests-2.32.3\n\
            /nix/store/0123456789abcdfghijklmnpqrsvwxyz-xz-5.6.3\n\
            /nix/store/0123456789abcdfghijklmnpqrsvwxyz-etc\n\
            \n\
            /nix/store/short-curl-8.11.0\n\
            /nix/store/0123456789ABCDFGHIJKLMNPQRSVWXYZ-zlib-1.3\n";
        assert_eq!(
            package_names(paths),
            set(&[
                "openssl",
                "openssh",
                "python3.12-requests",
                "xz",
                "etc",
                "/nix/store/short-curl",
                "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-zlib",
            ])
        );
    }

    #[test]
    fn fields_are_written_as_tsv_wrote_them() {
        assert_eq!(tsv("a\tb\nc\rd\\e"), "a\\tb\\nc\\rd\\\\e");
        assert_eq!(tsv("CVE-2024-1"), "CVE-2024-1");
    }

    fn kev(entries: serde_json::Value) -> Kev {
        serde_json::from_value(serde_json::json!({ "vulnerabilities": entries })).unwrap()
    }

    #[test]
    fn kev_counts_what_is_installed() {
        let list = kev(serde_json::json!([
            {"cveID": "CVE-1", "vendorProject": "OpenSSL", "product": "OpenSSL", "vulnerabilityName": "Heartbleed\tagain"},
            {"cveID": "CVE-2", "vendorProject": "Google", "product": "Chromium V8", "vulnerabilityName": "V8"},
            {"cveID": "CVE-3", "vendorProject": "Apache", "product": "HTTP Server", "vulnerabilityName": "httpd"},
            {"cveID": "CVE-4", "vendorProject": "Linux", "product": "Kernel", "vulnerabilityName": "a kernel bug"},
            {"cveID": "CVE-5", "vendorProject": "Microsoft", "product": "Kernel", "vulnerabilityName": "not Linux's"},
            {"cveID": "CVE-6", "vendorProject": "Exim", "product": "Exim", "dateAdded": "2024-01-01"},
        ]));
        let names = set(&["openssl", "apache-httpserver", "exim", "chromium"]);
        assert_eq!(
            kev_findings(&list, &names, "box"),
            vec![
                Finding::new(
                    "box",
                    "kev:CVE-1",
                    "CVE-1 (KNOWN EXPLOITED): Heartbleed\\tagain"
                ),
                Finding::new("box", "kev:CVE-3", "CVE-3 (KNOWN EXPLOITED): httpd"),
                Finding::new("box", "kev:CVE-4", "CVE-4 (KNOWN EXPLOITED): a kernel bug"),
                Finding::new("box", "kev:CVE-6", "CVE-6 (KNOWN EXPLOITED): "),
            ]
        );
    }

    #[test]
    fn vulnix_counts_cvss_7_and_up_outside_the_kernel() {
        let packages: Vec<Vulnix> = serde_json::from_value(serde_json::json!([
            {"pname": "openssl", "name": "openssl-3.0.15", "affected_by": ["CVE-1", "CVE-2", "CVE-3", "CVE-4"],
             "cvssv3_basescore": {"CVE-1": 9.8, "CVE-2": 6.9, "CVE-3": 7.0}},
            {"pname": "linux", "name": "linux-6.12.1", "affected_by": ["CVE-5"], "cvssv3_basescore": {"CVE-5": 9.0}},
            {"pname": "linux-firmware", "name": "linux-firmware-1", "affected_by": ["CVE-6"], "cvssv3_basescore": {"CVE-6": 9.0}},
            {"pname": "linuxptp", "name": "linuxptp-4", "affected_by": ["CVE-7"], "cvssv3_basescore": {"CVE-7": 10.0}},
            {"pname": "glibc", "name": "glibc-2.40", "affected_by": ["CVE-8"]},
            {"pname": "zlib", "name": "zlib-1.3", "affected_by": ["CVE-9"], "cvssv3_basescore": {"CVE-9": null}},
        ]))
        .unwrap();
        assert_eq!(
            vulnix_findings(&packages, "box"),
            vec![
                Finding::new(
                    "box",
                    "vulnix:CVE-1:openssl-3.0.15",
                    "CVE-1 in openssl-3.0.15 (CVSS 9.8)"
                ),
                Finding::new(
                    "box",
                    "vulnix:CVE-3:openssl-3.0.15",
                    "CVE-3 in openssl-3.0.15 (CVSS 7.0)"
                ),
                Finding::new(
                    "box",
                    "vulnix:CVE-7:linuxptp-4",
                    "CVE-7 in linuxptp-4 (CVSS 10.0)"
                ),
            ]
        );
    }

    #[test]
    fn osv_counts_each_group_in_each_package() {
        let osv: Osv = serde_json::from_value(serde_json::json!({"results": [
            {"source": {"path": "/src/app/Cargo.lock"}, "packages": [
                {"package": {"name": "rustls", "version": "0.23.1", "ecosystem": "crates.io"},
                 "groups": [
                    {"ids": ["GHSA-1", "RUSTSEC-1"], "max_severity": "7.5"},
                    {"ids": ["RUSTSEC-2"], "max_severity": ""},
                    {"ids": ["RUSTSEC-3"]},
                 ]},
                {"package": {"name": "idna", "version": "0.5.0"}},
            ]},
            {"packages": null},
            {},
        ]}))
        .unwrap();
        assert_eq!(
            osv_findings(&osv, "app"),
            vec![
                Finding::new(
                    "app",
                    "osv:app:GHSA-1,RUSTSEC-1:rustls",
                    "GHSA-1,RUSTSEC-1 in rustls 0.23.1 (severity 7.5)"
                ),
                Finding::new(
                    "app",
                    "osv:app:RUSTSEC-2:rustls",
                    "RUSTSEC-2 in rustls 0.23.1 (severity )"
                ),
                Finding::new(
                    "app",
                    "osv:app:RUSTSEC-3:rustls",
                    "RUSTSEC-3 in rustls 0.23.1 (severity ?)"
                ),
            ]
        );
        let none: Osv = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(osv_findings(&none, "app").is_empty());
    }
}
