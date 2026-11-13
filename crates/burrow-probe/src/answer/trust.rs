//! `trust`: who the deployed configuration lets ask or steer the machine
//! (the inbox's, the probe's and the engines door's authors, and the
//! steward's), and what each kind of job acts as on GitHub. The lists come
//! from the configuration modules/probe.nix writes, and the steward's from
//! its deployed config file, read through shown.rs. Which App a job acts as
//! is proj's to decide (`turn::github_app`, the inbox's `start`): `job_app`
//! says the same, and burrow-proj's tests hold the two together.

use serde_json::Value;

use super::Machine;
use crate::{App, Trust};

/// The largest steward config read.
const MAX_CONFIG: u64 = 1024 * 1024;

/// What a proj turn acts as on GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acts<'a> {
    /// The App of that name.
    App(&'a str),
    /// The user's own gh login.
    User,
    /// Nothing: the turn doesn't start.
    Refused,
}

/// The job's own App (`--app`), else on Codex the machinery's, else the
/// user's own login; a Codex turn with neither doesn't start. Mirrors
/// proj's `turn::github_app`, which its tests compare with this.
pub fn job_app<'a>(own: Option<&'a str>, codex: bool, machinery: Option<&'a str>) -> Acts<'a> {
    let named = |a: Option<&'a str>| a.filter(|a| !a.is_empty());
    match (named(own), codex) {
        (Some(app), _) => Acts::App(app),
        (None, false) => Acts::User,
        (None, true) => named(machinery).map_or(Acts::Refused, Acts::App),
    }
}

pub fn answer(m: &Machine) -> Result<String, String> {
    let Some(t) = &m.cfg.trust else {
        return Err(
            "this configuration doesn't say who it trusts (a probe from before `trust`)".into(),
        );
    };
    let user = &m.cfg.user;
    let machinery = t
        .machinery
        .as_deref()
        .and_then(|name| t.apps.iter().find(|a| a.name == name));
    let mut out = String::from(
        "Who may ask or steer, as deployed (an account's numeric ID is what counts; its login is a label):\n",
    );
    out += &match &t.inbox {
        Some(inbox) => format!(
            "- inbox (an issue or reply becomes a job): {}{}\n",
            people(&inbox.authors),
            machinery.map_or(String::new(), |a| format!(
                "; issues the machinery App {} opens run too, though its comments aren't heard",
                account(a)
            ))
        ),
        None => "- inbox: off\n".into(),
    };
    out += &format!(
        "- probe (issues in {}): {}{}\n",
        m.cfg.repo,
        people(&m.cfg.authors),
        m.cfg.machinery.as_ref().map_or(String::new(), |a| format!(
            ", and the machinery App {} ({})",
            a.login, a.id
        ))
    );
    out += &match &t.engines {
        Some(authors) => format!("- engines door: {}\n", people(authors)),
        None => "- engines door: off\n".into(),
    };
    out += &format!("- steward (autoland): {}\n", steward(m, t));

    out += "What each kind of job acts as on GitHub:\n";
    let app = |name: &str| match t.apps.iter().find(|a| a.name == name) {
        Some(a) => format!("the App {}", account(a)),
        None => format!("the App {name}, which isn't one of this machine's"),
    };
    let own = format!("{user}'s own gh login");
    let says = |acts: Acts| {
        match acts {
        Acts::App(a) => app(a),
        Acts::User => own.clone(),
        Acts::Refused => "nothing: with no machinery App a Codex job doesn't start, since its sandbox keeps the user's logins out".into(),
    }
    };
    let claude = says(job_app(None, false, t.machinery.as_deref()));
    let codex = says(job_app(None, true, t.machinery.as_deref()));
    out += &format!("- a Claude job with no --app: {claude}\n");
    out += &format!("- a Codex job with no --app: {codex}\n");
    out += &format!(
        "- a job with --app NAME: that App; this machine's are {}\n",
        if t.apps.is_empty() {
            "none".to_string()
        } else {
            t.apps
                .iter()
                .map(|a| format!("{} ({})", a.name, account(a)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    if let Some(inbox) = &t.inbox {
        if inbox.vm {
            out += "- an inbox job: in an agent sandbox (inbox.vm), with no GitHub login\n";
        } else {
            out += "- an inbox job on a person's issue: as their own job would, by its engine (above)\n";
            out += &format!(
                "- an inbox job on a bot's issue: {}\n",
                match &t.machinery {
                    Some(name) => format!("{} (Codex too)", app(name)),
                    None => format!("as a person's (no machinery App): {own} on Claude"),
                }
            );
        }
    }
    if t.sandboxes {
        out += "- a job in an agent sandbox (proj run --vm): no GitHub token; it reaches the host only through `proj probe`\n";
    }
    out += &format!(
        "- the machine's own calls (this probe, proj-inbox's posts, the dispatcher, Renovate): {}\n",
        t.machinery.as_deref().map_or(own.clone(), app)
    );
    Ok(out)
}

/// The steward's trusted authors, from its deployed config.
fn steward(m: &Machine, t: &Trust) -> String {
    let Some(path) = &t.autoland else {
        return "off".into();
    };
    let path = path.to_string_lossy();
    let config: Value = match m
        .read_small(&path, MAX_CONFIG)
        .map_err(|e| e.say())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| format!("unknown: {e}")))
    {
        Ok(v) => v,
        Err(why) => return format!("{path}: {why}"),
    };
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let authors = strings(&config["authors"]);
    let bots = strings(&config["bots"]);
    format!(
        "PRs by {}{} merge themselves once settled (by login); it acts as {}, and its reviewers hold no GitHub token",
        if authors.is_empty() {
            "nobody".to_string()
        } else {
            authors.join(", ")
        },
        if bots.is_empty() {
            String::new()
        } else {
            format!(" and the bots {}", bots.join(", "))
        },
        config["app"]["slug"]
            .as_str()
            .map_or("an App it doesn't name".to_string(), |s| format!(
                "{s}[bot]"
            ))
    )
}

fn people(authors: &[burrow_github::Author]) -> String {
    if authors.is_empty() {
        return "nobody".into();
    }
    authors
        .iter()
        .map(|a| format!("{} ({})", a.login, a.id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn account(a: &App) -> String {
    format!("{} ({})", a.login, a.id)
}

#[cfg(test)]
mod tests {
    use super::{Acts, job_app};

    #[test]
    fn the_app_a_job_acts_as() {
        assert_eq!(job_app(Some("x"), true, Some("m")), Acts::App("x"));
        assert_eq!(job_app(None, false, Some("m")), Acts::User);
        assert_eq!(job_app(None, true, Some("m")), Acts::App("m"));
        assert_eq!(job_app(Some(""), true, None), Acts::Refused);
        assert_eq!(job_app(None, true, Some("")), Acts::Refused);
    }
}
