//! Independent issue door. Receipts bind a request to its open cycle, so a
//! failed GitHub reply cannot replay an old allow over a newer host pause.
use crate::Config;
use burrow_door::{Door, asked, fenced};
use burrow_github::{Api, Author, Error};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::PathBuf,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoorConfig {
    pub repo: String,
    pub label: String,
    pub authors: Vec<Author>,
    pub state: PathBuf,
    pub settings: Config,
}

pub fn pass(api: &mut dyn Api, cfg: &DoorConfig) -> Result<(), Error> {
    let door = Door {
        repo: &cfg.repo,
        label: &cfg.label,
        authors: &cfg.authors,
        what: "automated work settings",
    };
    fs::create_dir_all(&cfg.state).map_err(|e| e.to_string())?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cfg.state.join("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    for issue in door.issues(api)? {
        let (text, cycle) = match door.written_cycle(api, &issue)? {
            Ok(value) => value,
            Err(_) => continue,
        };
        let digest: String = Sha256::digest(
            format!("{}:{}:{cycle}", cfg.repo.to_ascii_lowercase(), issue.number).as_bytes(),
        )
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
        let receipt = cfg.state.join(digest);
        let lines = asked(&text);
        let args = match lines.as_slice() {
            [line] => line
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let body = if let Err(why) = crate::parse(&args) {
            format!("Nothing changed. {why}")
        } else {
            cfg.settings.request(
                &receipt,
                &args,
                &format!("github:{}#{}:{}", cfg.repo, issue.number, issue.opener),
            )?
        };
        door.reply(api, issue.number, &fenced(&body), "completed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burrow_github::fake::{Fake, ok};
    use serde_json::json;

    fn github(author: &str, editor: Option<&str>, body: &str) -> Fake {
        let mut gh = Fake::default();
        let edits: Vec<_> = editor
            .into_iter()
            .map(|e| json!({"editor":burrow_github::fake::actor(e),"deletedBy":null}))
            .collect();
        gh.on(
            "GET",
            "repos/sam/box/issues",
            vec![ok(200, json!([{"number":1,"user":burrow_github::fake::actor(author)}]), &[])],
        )
        .on(
            "POST",
            "graphql",
            vec![ok(
                200,
                json!({"data":{"repository":{"issue":{
                  "id":"one", "author":burrow_github::fake::actor(author),"title":"settings","body":body,
                  "openCycle":{"nodes":[]},
                  "userContentEdits":{"totalCount":edits.len(),"nodes":edits},
                  "timelineItems":{"totalCount":0,"nodes":[]}
                }}}}),
                &[],
            )],
        )
        .on(
            "POST",
            "repos/sam/box/issues/1/comments",
            vec![ok(201, json!({}), &[])],
        )
        .on(
            "PATCH",
            "repos/sam/box/issues/1",
            vec![ok(200, json!({}), &[])],
        );
        gh
    }

    #[test]
    fn only_authorized_requests_change_admission() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DoorConfig {
            repo: "sam/box".into(),
            label: "settings".into(),
            authors: vec![burrow_github::fake::author("sam")],
            state: dir.path().join("door"),
            settings: Config {
                overlay: dir.path().join("overlay.json"),
                allowed: vec!["sam/code".into()],
                pipeline_repos: vec![],
            },
        };
        for (author, editor) in [("outsider", None), ("sam", Some("outsider"))] {
            pass(&mut github(author, editor, "pause sam/code"), &cfg).unwrap();
            assert!(cfg.settings.allows("sam/code").unwrap());
        }
        pass(&mut github("sam", None, "pause sam/code"), &cfg).unwrap();
        assert!(!cfg.settings.allows("sam/code").unwrap());
        cfg.settings
            .command(&["allow".into(), "sam/code".into()], "host")
            .unwrap();
        pass(&mut github("sam", None, "pause sam/code"), &cfg).unwrap();
        assert!(
            cfg.settings.allows("sam/code").unwrap(),
            "duplicate delivery cannot replay a pause either"
        );
        pass(&mut github("sam", None, "bad request"), &cfg).unwrap();
        assert!(cfg.settings.allows("sam/code").unwrap());
    }
}
