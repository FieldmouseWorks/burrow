//! burrow-codeql: which CodeQL languages a run of the shared code scanning
//! (.github/workflows/codeql.yml, its `pick` job) analyzes.
//!
//! A push to the default branch analyzes every language the project has, so
//! each merged tree is scanned whole. A PR analyzes the languages its changed
//! paths might be input to, deleted paths and renames' old paths included.
//! A path is matched against what each extractor reads (CodeQL's own
//! sources: the JavaScript extractor's FileExtractor.java, the Python
//! extractor's PY_EXTENSIONS and script check) and the configuration each
//! takes, by extension. A path with no extension, which an extractor reads
//! by its content, and a path whose extension nothing here classifies, as an
//! input or as one no extractor reads, count for every language: what this
//! doesn't know is scanned, never skipped.
//!
//! The workflow takes only the languages CodeQL analyzes on Linux with no
//! build (`build-mode: none`); any other is an error, on a push as on a PR,
//! rather than a language analyzed wrong or not at all.

use std::collections::BTreeSet;
use std::io::Write;

use burrow_github::{Api, pages};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Language {
    Actions,
    CCpp,
    JavascriptTypescript,
    Python,
    Ruby,
    Rust,
}

impl Language {
    pub const ALL: [Language; 6] = [
        Language::Actions,
        Language::CCpp,
        Language::JavascriptTypescript,
        Language::Python,
        Language::Ruby,
        Language::Rust,
    ];

    /// Its name, as CodeQL's `languages` input takes it.
    pub fn name(self) -> &'static str {
        match self {
            Language::Actions => "actions",
            Language::CCpp => "c-cpp",
            Language::JavascriptTypescript => "javascript-typescript",
            Language::Python => "python",
            Language::Ruby => "ruby",
            Language::Rust => "rust",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }

    /// Whether its extractor might read a path, given the path, its file
    /// name and its extension, all lowercase: extractors match without
    /// regard to case.
    fn reads(self, path: &str, name: &str, ext: &str) -> bool {
        match self {
            Language::Actions => {
                (matches!(ext, "yml" | "yaml")
                    && [".github/workflows/", ".github/actions/"]
                        .iter()
                        .any(|dir| path.starts_with(dir) || path.contains(&format!("/{dir}"))))
                    || matches!(name, "action.yml" | "action.yaml")
            }
            Language::CCpp => {
                C_CPP.contains(&ext)
                    // Build configuration: CMake's lists among any text.
                    || ext == "txt"
                    || matches!(name, "meson.build" | "compile_commands.json")
            }
            Language::JavascriptTypescript => JAVASCRIPT.contains(&ext),
            Language::Python => {
                matches!(ext, "py" | "pyi" | "pyw")
                    // Configuration, wherever it's kept and whatever it's
                    // called: pyproject.toml, setup.cfg, requirements files.
                    || matches!(ext, "toml" | "cfg" | "ini" | "txt")
                    || name == "pipfile.lock"
            }
            Language::Ruby => RUBY.contains(&ext) || name == "gemfile.lock",
            Language::Rust => {
                ext == "rs"
                    // Project configuration: Cargo.toml, .cargo/config.toml,
                    // rust-toolchain.toml and any other TOML; rust-project.json.
                    || ext == "toml"
                    || matches!(name, "cargo.lock" | "rust-project.json")
            }
        }
    }
}

const C_CPP: &[&str] = &[
    "c", "cc", "cp", "cpp", "cxx", "c++", "h", "hh", "hp", "hpp", "hxx", "h++", "inl", "ipp",
    "tcc", "tpp", "ixx", "cppm", "ii", "i",
];

/// The JavaScript extractor's file types: HTML (an `.erb` or `.dot` it
/// reads only as `.html.erb` or `.html.dot`, which this takes whole), JS,
/// JSON, TypeScript and YAML (`.lock` among them), and the templates it
/// reads as HTML.
const JAVASCRIPT: &[&str] = &[
    "htm",
    "html",
    "xhtm",
    "xhtml",
    "xht",
    "vue",
    "svelte",
    "hbs",
    "handlebars",
    "ejs",
    "njk",
    "erb",
    "jsp",
    "dot",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "es6",
    "es",
    "xsjs",
    "xsjslib",
    "json",
    "jsonc",
    "json5",
    "ts",
    "tsx",
    "mts",
    "cts",
    "lock",
    "raml",
    "yaml",
    "yml",
];

const RUBY: &[&str] = &[
    "rb", "erb", "rake", "gemspec", "ru", "rbw", "builder", "jbuilder", "thor",
];

/// Extensions no extractor here reads, by CodeQL's sources and their
/// configuration: no extractor sniffs a path with an extension for a
/// shebang, so these can't be anyone's input.
const INERT: &[&str] = &[
    "md", "markdown", "rst", "adoc", "nix", "sh", "bash", "css", "scss", "svg", "png", "jpg",
    "jpeg", "gif", "ico", "webp", "pdf", "docx", "patch", "diff", "sql", "csv", "tsv",
];

/// The languages that might read a path, or None when nothing here
/// classifies it, which counts for every language.
pub fn readers(path: &str) -> Option<BTreeSet<Language>> {
    let path = path.to_ascii_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    // Leading dots start no extension, as Python's `os.path.splitext` has
    // it (`.babelrc`, `..md` and `...` have none), which is the widest
    // reading any extractor takes: one with none counts for every language.
    let stem = name.trim_start_matches('.');
    let ext = match stem.rfind('.') {
        Some(i) => &stem[i + 1..],
        None => "",
    };
    if ext.is_empty() {
        // The JavaScript and Python extractors read a path with no
        // extension by its content (a shebang), which a name can't rule
        // out, and nothing here reads content: so it counts for every
        // language, `LICENSE` and `Gemfile` as much as `bin/deploy`.
        return None;
    }
    let found: BTreeSet<Language> = Language::ALL
        .into_iter()
        .filter(|l| l.reads(&path, name, ext))
        .collect();
    if found.is_empty() && !INERT.contains(&ext) {
        None
    } else {
        Some(found)
    }
}

/// The project's languages, from the caller's JSON array.
pub fn languages(json: &str) -> Result<Vec<Language>, String> {
    let names: Vec<String> = serde_json::from_str(json)
        .map_err(|e| format!("languages: {json:?} isn't a JSON array of names: {e}"))?;
    names
        .iter()
        .map(|name| {
            Language::parse(name).ok_or_else(|| {
                let takes: Vec<&str> = Language::ALL.iter().map(|l| l.name()).collect();
                format!(
                    "{name}: this workflow doesn't analyze it (it takes {}); give it a setup of its own.",
                    takes.join(" ")
                )
            })
        })
        .collect()
}

/// The languages as the workflow's matrix takes them, a JSON array.
pub fn matrix(languages: &[Language]) -> String {
    Value::from(languages.iter().map(|l| l.name()).collect::<Vec<_>>()).to_string()
}

pub enum Event {
    /// A push to the default branch, or anything but a PR.
    Push,
    /// A PR, by its number.
    PullRequest(u64),
}

/// GitHub lists at most this many of a PR's files; a PR that big scans
/// every language.
pub const MOST_FILES: usize = 3000;

/// The languages this run analyzes, of the project's, in the project's
/// order, saying why each is or isn't to `log`. A PR's files come from
/// `repo`'s GitHub; when GitHub can't say what changed, it's an error,
/// never an empty pick.
pub fn pick(
    api: &mut dyn Api,
    repo: &str,
    event: &Event,
    project: &[Language],
    log: &mut dyn Write,
) -> Result<Vec<Language>, String> {
    let say = |log: &mut dyn Write, line: String| {
        writeln!(log, "{line}").map_err(|e| format!("writing the log: {e}"))
    };
    let all = matrix(project);
    let Event::PullRequest(number) = event else {
        say(log, format!("Not a PR: every language, {all}."))?;
        return Ok(project.to_vec());
    };
    let mut paths = Vec::new();
    for page in pages(
        api,
        &format!("repos/{repo}/pulls/{number}/files?per_page=100"),
    )
    .map_err(|e| format!("listing PR #{number}'s files: {e}"))?
    {
        let files = page
            .as_array()
            .ok_or_else(|| format!("PR #{number}'s files: GitHub's page isn't a list"))?;
        for file in files {
            for key in ["filename", "previous_filename"] {
                if let Some(path) = file[key].as_str() {
                    paths.push(path.to_owned());
                }
            }
        }
    }
    if paths.len() >= MOST_FILES {
        say(
            log,
            format!("{MOST_FILES} or more changed paths: every language, {all}."),
        )?;
        return Ok(project.to_vec());
    }
    let mut touched = BTreeSet::new();
    for path in &paths {
        match readers(path) {
            Some(languages) => touched.extend(languages),
            None => {
                say(
                    log,
                    format!(
                        "{path}: nothing here says which extractor reads it, so every language, {all}."
                    ),
                )?;
                return Ok(project.to_vec());
            }
        }
    }
    let mut picked = Vec::new();
    for &language in project {
        let name = language.name();
        if touched.contains(&language) {
            say(log, format!("{name}: the PR changes its files."))?;
            picked.push(language);
        } else {
            say(
                log,
                format!(
                    "{name}: the PR changes none of its files, so not scanned here (the merge's push scans it)."
                ),
            )?;
        }
    }
    Ok(picked)
}

#[cfg(test)]
mod tests;
