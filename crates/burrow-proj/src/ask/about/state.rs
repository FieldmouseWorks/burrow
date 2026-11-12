//! One atomic document owns subject membership and policy. Moving a question
//! cannot expose a new key without its inherited constraints.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Member {
    pub id: String,
    pub question: String,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Group {
    pub due: i64,
    pub one_way: bool,
    pub members: Vec<Member>,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct State(pub BTreeMap<String, Group>);

impl State {
    pub fn load(cfg: &Config) -> Result<Self, String> {
        let path = dir(cfg).join("state.json");
        match fs::read_to_string(&path) {
            Ok(text) => return serde_json::from_str(&text).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        // Upgrade the earlier per-subject registry without rewriting any job.
        // Once state.json exists it is the sole authority, including empties.
        #[derive(Deserialize)]
        struct Legacy {
            due: i64,
            one_way: bool,
            members: Vec<String>,
        }
        let mut state = Self::default();
        let entries = match fs::read_dir(dir(cfg)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(state),
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let hex = entry.file_name().to_string_lossy().into_owned();
            if hex == "lock" || hex.ends_with(".new") {
                continue;
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|at| {
                    hex.get(at..at + 2)
                        .and_then(|s| u8::from_str_radix(s, 16).ok())
                        .ok_or("invalid subject registry name")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let key = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            let old: Legacy =
                serde_json::from_str(&fs::read_to_string(entry.path()).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let members = old
                .members
                .into_iter()
                .filter_map(|id| {
                    let job = Job::new(cfg, &id);
                    if read(&host_path(&job, FILE)).as_deref() != Some(&key) {
                        return None;
                    }
                    let question = identity(&job)?;
                    Some(Member { id, question })
                })
                .collect();
            state.0.insert(
                key,
                Group {
                    due: old.due,
                    one_way: old.one_way,
                    members,
                },
            );
        }
        Ok(state)
    }

    pub fn save(&self, cfg: &Config) -> Result<(), String> {
        swap(
            &dir(cfg).join("state.json"),
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    pub fn standing(&self, job: &Job) -> Option<(&str, &Group)> {
        let question = identity(job)?;
        self.0.iter().find_map(|(key, group)| {
            group
                .members
                .iter()
                .any(|m| m.id == job.id && m.question == question)
                .then_some((key.as_str(), group))
        })
    }

    pub fn live(&self, cfg: &Config, group: &Group) -> Vec<Member> {
        group
            .members
            .iter()
            .filter(|m| identity(&Job::new(cfg, &m.id)).as_deref() == Some(m.question.as_str()))
            .cloned()
            .collect()
    }

    pub fn apply(&self, job: &Job, q: &mut Question) -> Result<(), String> {
        if let Some((key, group)) = self.standing(job) {
            q.due = group.due;
            q.recommendation.one_way |= group.one_way;
            q.recommendation.about = Some(key.to_owned());
        } else if q.recommendation.about.is_some() || host_path(job, FILE).exists() {
            return Err(format!("{} has an unbound shared question", job.id));
        }
        Ok(())
    }
}
