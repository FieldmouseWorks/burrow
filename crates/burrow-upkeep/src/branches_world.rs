//! What the branch watch's tests run it on: a GitHub that keeps a repo's
//! branches and closed PRs, and a git whose deletes take effect there.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use burrow_github::{Api, Failed, Response};
use serde_json::{Value, json};

use super::*;
use crate::config::Config;
use crate::pass::{DAY, Out, Pass};
use crate::testing::{self, States, T};

/// A branch: its tip, how far it's ahead of the default, and when its last
/// commit was made.
#[derive(Clone)]
pub struct Branch {
    pub sha: String,
    pub ahead: u64,
    pub date: f64,
}

/// A closed PR: its branch, the tip it closed at, the repo its branch is
/// in, when it closed and when it last changed.
pub struct Pr {
    pub number: u64,
    pub branch: String,
    pub sha: String,
    pub repo: String,
    pub closed: f64,
    pub updated: f64,
}

/// What GitHub answers instead of what it has: a status, and its message.
pub type No = (u16, &'static str);

#[derive(Default)]
pub struct World {
    pub default: String,
    pub branches: BTreeMap<String, Branch>,
    pub protected: BTreeSet<String>,
    pub closed: Vec<Pr>,
    /// What's under refs/archive, and the branches GitHub won't keep there.
    pub archived: BTreeMap<String, String>,
    pub unarchivable: BTreeSet<String>,
    /// What it answers instead: to the branches' listing, the closed PRs',
    /// and by branch, its closed PRs, its comparison and its archive.
    pub listing: Option<No>,
    pub feeding: Option<No>,
    pub closes: BTreeMap<String, No>,
    pub compares: BTreeMap<String, No>,
    pub archiving: BTreeMap<String, No>,
    /// The branches whose deletes git turns away, and what it says then.
    pub undeletable: BTreeSet<String>,
    /// What a look did: the deletes it asked git for, the branches whose
    /// closed PRs it read, and what each listing got (200 or 304).
    pub deleted: Vec<(String, String)>,
    pub looked: BTreeSet<String>,
    pub fresh: Vec<u16>,
    pub fed: Vec<u16>,
    /// Whether anything asked it to change anything.
    pub asked_to_change: bool,
}

pub const REFUSAL: &str =
    "remote: error: GH013: Repository rule violations found for refs/heads/empty.";

pub fn iso(t: f64) -> String {
    jiff::Timestamp::from_second(t.floor() as i64)
        .unwrap()
        .to_string()
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

pub fn reply(status: u16, body: Value, headers: Vec<(String, String)>) -> Response {
    Response {
        status,
        headers,
        body: if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    }
}

pub fn no(&(status, why): &No) -> Response {
    reply(status, json!({"message": why}), vec![])
}

/// One page of a listing, with its etag (`tag`, a 304 when it's `etag`)
/// and the next page's Link.
pub fn paged(
    all: &[Value],
    page: usize,
    path: &str,
    tag: Option<String>,
    etag: Option<&str>,
) -> Response {
    if tag.is_some() && tag.as_deref() == etag {
        return reply(304, Value::Null, vec![]);
    }
    let items = all.chunks(100).nth(page - 1).unwrap_or_default().to_vec();
    let mut headers: Vec<(String, String)> = tag.map(|t| ("ETag".into(), t)).into_iter().collect();
    if all.len() > page * 100 {
        let next = format!(
            "<https://api.github.com/{path}&page={}>; rel=\"next\"",
            page + 1
        );
        headers.push(("Link".into(), next));
    }
    reply(200, Value::Array(items), headers)
}

impl World {
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Response {
        if path == "rate_limit" {
            return reply(
                200,
                json!({"resources": {"core": {"remaining": 0, "reset": T + 7200.0}}}),
                vec![],
            );
        }
        let (path, page) = match path.split_once("&page=") {
            Some((path, n)) => (path, n.parse::<usize>().unwrap()),
            None => (path, 1),
        };
        let at = path
            .strip_prefix("repos/sam/r/")
            .unwrap_or_else(|| panic!("nothing said for {method} {path}"));
        match (method, at) {
            ("GET", "branches?per_page=100") => {
                if let Some(no_) = self.listing.filter(|_| page == 1) {
                    return no(&no_);
                }
                let all: Vec<Value> = self
                    .branches
                    .iter()
                    .map(|(n, b)| json!({"name": n, "commit": {"sha": b.sha}, "protected": self.protected.contains(n)}))
                    .collect();
                // What a listing shows: each branch's tip, not what it's
                // compared to.
                let tag = Value::Array(all.clone()).to_string();
                if page == 1 {
                    self.fresh.push(if etag == Some(&tag) { 304 } else { 200 });
                }
                paged(&all, page, path, Some(tag), etag)
            }
            ("GET", "pulls?state=closed&sort=updated&direction=desc&per_page=100") => {
                if let Some(no_) = self.feeding {
                    return no(&no_);
                }
                let mut feed: Vec<&Pr> = self.closed.iter().collect();
                feed.sort_by(|a, b| b.updated.total_cmp(&a.updated));
                let page: Vec<Value> = feed[..feed.len().min(100)]
                    .iter()
                    .map(|p| {
                        json!({"number": p.number, "updated_at": iso(p.updated),
                               "head": {"ref": p.branch, "sha": p.sha, "repo": {"full_name": p.repo}}})
                    })
                    .collect();
                let tag = Value::Array(page.clone()).to_string();
                self.fed.push(if etag == Some(&tag) { 304 } else { 200 });
                paged(&page, 1, path, Some(tag), etag)
            }
            ("GET", q) if q.starts_with("pulls?state=closed&per_page=100&head=sam:") => {
                let name = decode(&q["pulls?state=closed&per_page=100&head=sam:".len()..]);
                if page == 1 {
                    self.looked.insert(name.clone());
                }
                if let Some(no_) = self.closes.get(&name) {
                    return no(no_);
                }
                let prs: Vec<Value> = self
                    .closed
                    .iter()
                    .filter(|p| p.branch == name && p.repo == "sam/r")
                    .map(|p| json!({"number": p.number, "head": {"sha": p.sha}, "closed_at": iso(p.closed)}))
                    .collect();
                paged(&prs, page, path, None, None)
            }
            ("GET", q) if q.starts_with("compare/") => {
                let (base, head) = q["compare/".len()..].split_once("...").unwrap();
                // A default with a slash in it is one segment of the path.
                assert_eq!(base, burrow_github::encode(&self.default), "{q}");
                let (name, b) = self.branches.iter().find(|(_, b)| b.sha == head).unwrap();
                if let Some(no_) = self.compares.get(name) {
                    return no(no_);
                }
                let commits: Vec<Value> = match b.ahead {
                    0 => vec![],
                    _ => vec![json!({"commit": {"committer": {"date": iso(b.date)}}})],
                };
                reply(
                    200,
                    json!({"ahead_by": b.ahead, "commits": commits}),
                    vec![],
                )
            }
            ("POST", "git/refs") => {
                self.asked_to_change = true;
                let body = body.unwrap();
                let to = body["ref"].as_str().unwrap().to_string();
                let name = to.split('/').nth(2).unwrap().to_string();
                if let Some(no_) = self.archiving.get(&name) {
                    return no(no_);
                }
                if self.archived.contains_key(&to) || self.unarchivable.contains(&name) {
                    return no(&(422, "Reference already exists"));
                }
                let sha = body["sha"].as_str().unwrap().to_string();
                self.archived.insert(to.clone(), sha.clone());
                reply(201, json!({"ref": to, "object": {"sha": sha}}), vec![])
            }
            ("GET", q) if q.starts_with("git/ref/") => {
                let to = format!("refs/{}", decode(&q["git/ref/".len()..]));
                match self.archived.get(&to) {
                    Some(sha) => reply(200, json!({"ref": to, "object": {"sha": sha}}), vec![]),
                    None => no(&(404, "Not Found")),
                }
            }
            _ => panic!("nothing said for {method} {path}"),
        }
    }
}

pub struct Hub(pub Rc<RefCell<World>>);

impl Api for Hub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed> {
        Ok(self.0.borrow_mut().request(method, path, body, etag))
    }

    fn pause(&mut self, _: u64) {}
}

pub struct Hands(pub Rc<RefCell<World>>);

impl Push for Hands {
    fn delete(&mut self, repo: &str, name: &str, sha: &str) -> Result<Pushed, String> {
        assert_eq!(repo, "sam/r");
        let mut w = self.0.borrow_mut();
        w.deleted.push((name.into(), sha.into()));
        if w.undeletable.contains(name) {
            return Ok(Pushed {
                code: Some(1),
                said: REFUSAL.into(),
            });
        }
        w.branches.remove(name);
        Ok(Pushed {
            code: Some(0),
            said: String::new(),
        })
    }
}

/// sam/r on GitHub, and the steward's look at it.
pub struct SamR {
    pub world: Rc<RefCell<World>>,
    pub cfg: Config,
    pub now: f64,
    pub _dir: tempfile::TempDir,
}

pub fn sha(c: &str) -> String {
    c.repeat(40 / c.len())
}

impl SamR {
    pub fn new() -> SamR {
        let dir = tempfile::tempdir().unwrap();
        let cfg = testing::config(dir.path());
        std::fs::create_dir_all(&cfg.state_dir).unwrap();
        let world = World {
            default: "main".into(),
            ..World::default()
        };
        SamR {
            world: Rc::new(RefCell::new(world)),
            cfg,
            now: T,
            _dir: dir,
        }
    }

    pub fn ago(&self, days: f64) -> f64 {
        (self.now - days * DAY).floor()
    }

    pub fn branch(&self, name: &str, tip: &str, ahead: u64, date: f64) {
        let b = Branch {
            sha: sha(tip),
            ahead,
            date,
        };
        self.world.borrow_mut().branches.insert(name.into(), b);
    }

    /// A PR from `name` (in `repo`) closed at `tip`, `at`: GitHub's times
    /// are whole seconds.
    pub fn closed_at(&self, name: &str, number: u64, tip: &str, at: f64, repo: &str) {
        self.world.borrow_mut().closed.push(Pr {
            number,
            branch: name.into(),
            sha: sha(tip),
            repo: repo.into(),
            closed: at.floor(),
            updated: at.floor(),
        });
    }

    /// One closed now.
    pub fn close(&self, name: &str, number: u64, tip: &str) {
        self.closed_at(name, number, tip, self.now, "sam/r");
    }

    /// A pass's look at sam/r's branches, `open` its open PRs': what it
    /// says back.
    pub fn look(&mut self, open: &[&str], full: bool) -> Out {
        {
            let mut w = self.world.borrow_mut();
            w.deleted.clear();
            w.looked.clear();
            w.fresh.clear();
            w.fed.clear();
        }
        let mut api = Hub(self.world.clone());
        let mut git = Hands(self.world.clone());
        let mut jobs = States::new("running");
        let mut p = Pass {
            cfg: &self.cfg,
            repo: "sam/r",
            now: self.now,
            dry: false,
            searched: 0,
            api: &mut api,
            jobs: &mut jobs,
            out: Out::default(),
        };
        let open: BTreeSet<String> = open.iter().map(|s| s.to_string()).collect();
        let default = self.world.borrow().default.clone();
        let repo = Repo {
            default: &default,
            open: &open,
            full,
        };
        watch(&mut p, self.cfg.branches.as_ref().unwrap(), &repo, &mut git);
        p.out
    }

    /// What a look lists, a line each.
    pub fn lists(&mut self, open: &[&str], full: bool) -> Vec<String> {
        self.look(open, full)
            .stuck
            .into_iter()
            .map(|l| l.1)
            .collect()
    }

    pub fn deleted(&self) -> Vec<(String, String)> {
        let mut d = self.world.borrow().deleted.clone();
        d.sort();
        d
    }

    pub fn deleted_names(&self) -> Vec<String> {
        self.deleted().into_iter().map(|d| d.0).collect()
    }

    pub fn looked(&self) -> Vec<String> {
        self.world.borrow().looked.iter().cloned().collect()
    }

    pub fn state(&self) -> Value {
        let text = std::fs::read_to_string(self.cfg.state_dir.join("repo-sam_r.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    pub fn tips(&self) -> BTreeMap<String, Tip> {
        serde_json::from_value(self.state()["tips"].clone()).unwrap()
    }

    pub fn tip(&self, name: &str) -> Tip {
        self.tips()[name].clone()
    }
}

pub fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The repo as the tests start it: branches nothing needs (one already in
/// the default, at the tip a closed PR was closed at, a bot's included:
/// GitHub deletes on merge, never on close; one with no PR and no commit
/// for STALE_DAYS, one an earlier look archived too, one GitHub won't
/// archive), and branches that stay (one pushed to since its PR closed, an
/// open PR's, a bot's with no closed PR at its tip, the default and a kept
/// one).
pub fn fixture() -> SamR {
    let r = SamR::new();
    // Given up on a day past STALE_DAYS (3: agents open a PR when they
    // push), or fresh from today.
    let (old, young) = (r.ago(4.0), r.ago(1.0));
    for (name, tip, ahead, date) in [
        ("main", "ab", 0, young),
        ("in-main", "1", 0, young),
        ("closed", "2", 3, young),
        ("moved", "3", 1, young),
        ("open", "4", 2, old),
        ("renovate/x", "5", 1, old),
        ("gh-pages", "6", 9, old),
        ("old", "7", 4, old),
        ("young", "8", 4, young),
        ("renovate/closed", "9", 1, young),
        ("renovate/again", "a", 1, young),
        ("archived", "d", 2, old),
        ("refused", "e", 2, old),
    ] {
        r.branch(name, tip, ahead, date);
    }
    let five = r.ago(5.0);
    r.closed_at("closed", 3, "2", five, "sam/r");
    r.closed_at("moved", 4, "f", five, "sam/r");
    // Its match is the 101st closed PR on the name: past the first page.
    for n in 100..200 {
        r.closed_at("renovate/closed", n, "c", five, "sam/r");
    }
    r.closed_at("renovate/closed", 5, "9", five, "sam/r");
    // Its last PR merged at another tip, and the bot has pushed its next
    // update.
    r.closed_at("renovate/again", 6, "b", five, "sam/r");
    {
        let mut w = r.world.borrow_mut();
        w.archived
            .insert(format!("refs/archive/archived/{}", sha("d")), sha("d"));
        // GitHub won't keep it, and what it has by that name is another
        // commit.
        w.unarchivable.insert("refused".into());
        w.archived
            .insert(format!("refs/archive/refused/{}", sha("e")), sha("0"));
    }
    r
}

/// The fixture, past the first two looks: what nothing needs is gone,
/// bar the one GitHub won't archive; and then the open PR closed and its
/// branch went an hour on.
pub fn settled() -> SamR {
    let mut r = fixture();
    r.look(&["open"], true);
    r.now += GRACE;
    r.look(&["open"], false);
    r.close("open", 2, "4");
    r.look(&[], false);
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted_names(), ["open"]);
    r.look(&[], false);
    r
}
