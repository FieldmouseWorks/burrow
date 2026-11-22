//! The docs the steward's config carries (modules/autoland.nix's
//! baselineDocs, listed in modules/autoland/baseline-docs.json): every
//! machine's build reads them, so none of them may take burrow's docs lane,
//! and the job that reads them follows their links, so those that point
//! where the bundle carries files land in the bundle. Every other doc under
//! docs/ takes the lane, and changing one rebuilds no machine.

use std::path::{Component, Path};

use regex::Regex;

use crate::harness::{read, repo_file, workflow};
use crate::lane::{CALLERS, Repo, files};

/// The bundle as the list has it: each entry a file or a directory, from the
/// repo's root.
fn bundle() -> Vec<String> {
    let list = read(&repo_file("modules/autoland/baseline-docs.json"));
    serde_json::from_str(&list).unwrap_or_else(|e| panic!("baseline-docs.json: {e}"))
}

/// Every file the bundle holds, from the repo's root.
fn bundled_files(bundle: &[String]) -> Vec<String> {
    fn walk(rel: &str, out: &mut Vec<String>) {
        let path = repo_file(rel);
        if path.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("{rel}: {e}"))
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            entries.sort();
            for name in entries {
                walk(&format!("{rel}/{name}"), out);
            }
        } else {
            assert!(
                path.is_file(),
                "{rel}: the bundle lists it, and it isn't there"
            );
            out.push(rel.to_string());
        }
    }
    let mut out = Vec::new();
    for entry in bundle {
        walk(entry, &mut out);
    }
    out
}

/// Whether `path` (from the repo's root) is in the bundle.
fn bundled(bundle: &[String], path: &str) -> bool {
    bundle
        .iter()
        .any(|e| path == e || path.starts_with(&format!("{e}/")))
}

/// `target`, a link in the file `from`, as a path from the repo's root, or
/// None when it climbs out of the repo.
fn resolve(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for c in Path::new(target).components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str().unwrap()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// The links in the bundle's Markdown that miss it: those that climb out of
/// the repo, and those into a directory the bundle carries files from (its
/// entries' first parts: docs/, house/, ...) that land on a file it leaves
/// out or that isn't there. A link to source elsewhere (crates/, .github/)
/// names code the reader can find in the repo, and the bundle leaves it out
/// on purpose: a machine's build carries no tests or workflows.
fn stray_links(bundle: &[String]) -> Vec<String> {
    let link = Regex::new(r"\]\(\s*([^)\s#]+)(?:#[^)\s]*)?(?:\s+[^)]*)?\)").unwrap();
    let scheme = Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").unwrap();
    let carried: Vec<&str> = bundle
        .iter()
        .map(|e| e.split('/').next().unwrap())
        .collect();
    let mut stray = Vec::new();
    for file in bundled_files(bundle).iter().filter(|f| f.ends_with(".md")) {
        let text = read(&repo_file(file));
        for m in link.captures_iter(&text) {
            let target = &m[1];
            if scheme.is_match(target) {
                continue;
            }
            match resolve(file, target) {
                None => stray.push(format!("{file} -> {target}: outside the repo")),
                Some(to) if carried.contains(&to.split('/').next().unwrap()) => {
                    if !bundled(bundle, &to) {
                        stray.push(format!("{file} -> {target}: {to} isn't in the bundle"));
                    } else if !repo_file(&to).exists() {
                        stray.push(format!("{file} -> {target}: {to} isn't there"));
                    }
                }
                Some(_) => {}
            }
        }
    }
    stray
}

/// Each link from the bundle into docs/, house/, templates/ or lib/ lands in
/// it, so the job that reads repo-baseline.md can follow it, and each doc in
/// it is one such a link reaches.
#[test]
fn the_baselines_links_land_in_its_bundle() {
    let bundle = bundle();
    assert!(
        bundle.iter().any(|e| e == "docs/repo-baseline.md"),
        "{bundle:?}"
    );
    assert_eq!(stray_links(&bundle), Vec::<String>::new());
    // Each doc but the baseline is there because a link reaches it: left
    // out, that link is found.
    for doc in bundle.iter().filter(|e| e.starts_with("docs/")) {
        if doc == "docs/repo-baseline.md" {
            continue;
        }
        let short: Vec<String> = bundle.iter().filter(|e| *e != doc).cloned().collect();
        let stray = stray_links(&short);
        assert!(
            stray
                .iter()
                .any(|s| s.contains(&format!(": {doc} isn't in the bundle"))),
            "{doc}: nothing in the bundle links it, so it needn't be there ({stray:?})"
        );
    }
}

/// No file of the bundle takes the docs lane, by burrow's own docs-paths run
/// through the lane's own step, a retargeted PR's too; a doc it leaves out
/// does.
#[test]
fn the_baseline_never_takes_the_docs_lane() {
    let bundled = bundled_files(&bundle());
    let (flake, retarget) = CALLERS[0];
    let mut docs_paths: Vec<String> = Vec::new();
    for caller in [flake, retarget] {
        let wf = workflow(caller);
        for job in wf["jobs"].as_mapping().unwrap().values() {
            if let Some(d) = job["with"]["docs-paths"].as_str()
                && !docs_paths.iter().any(|p| p == d)
            {
                docs_paths.push(d.to_string());
            }
        }
    }
    assert!(!docs_paths.is_empty(), "burrow's callers list no docs");
    let mut r = Repo::new();
    for docs in &docs_paths {
        for f in &bundled {
            assert!(
                !r.docs(docs, &files(&[f.as_str()])),
                "{f} is in the bundle every machine's build reads, and docs-paths ({docs}) puts it in the docs lane"
            );
        }
        // Any other doc does: that's what the lane is for.
        assert!(
            r.docs(docs, &files(&["docs/engines.md"])),
            "docs-paths ({docs}) leaves docs/engines.md out of the docs lane"
        );
        // One exclusion dropped, its doc takes the lane: the step reads them.
        let loose = docs.replace("!docs/repo-baseline.md", "");
        assert_ne!(
            &loose, docs,
            "docs-paths ({docs}) doesn't leave out docs/repo-baseline.md"
        );
        assert!(
            r.docs(&loose, &files(&["docs/repo-baseline.md"])),
            "{loose}"
        );
    }
}
