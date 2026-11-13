//! One atomic document owns subject membership and policy. Moving a question
//! cannot expose a new key without its inherited constraints.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Member {
    pub id: String,
    pub question: String,
    /// Time already credited to this question, including at prior subjects.
    /// Moving it must not replay that credit onto a newly selected deadline.
    #[serde(default)]
    pub pauses: Pauses,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Group {
    pub due: i64,
    pub one_way: bool,
    pub members: Vec<Member>,
    /// Union of pause intervals reflected in this group's selected deadline.
    #[serde(default)]
    pub pauses: Pauses,
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
            let mut members = Vec::new();
            for id in old.members {
                let job = Job::new(cfg, &id);
                let binding = match crate::state_reads::bytes(&host_path(&job, FILE)) {
                    Ok(bytes) => String::from_utf8(bytes).map_err(|e| e.to_string())?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.to_string()),
                };
                if binding.trim_end_matches('\n') != key {
                    continue;
                }
                if let Some(question) = identity(&job)? {
                    members.push(Member {
                        id,
                        question,
                        pauses: Pauses::default(),
                    });
                }
            }
            state.0.insert(
                key,
                Group {
                    due: old.due,
                    one_way: old.one_way,
                    members,
                    pauses: Pauses::default(),
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

    pub fn standing(&self, job: &Job) -> Result<Option<(&str, &Group)>, String> {
        let Some(question) = identity(job)? else {
            return Ok(None);
        };
        Ok(self.0.iter().find_map(|(key, group)| {
            group
                .members
                .iter()
                .any(|m| m.id == job.id && m.question == question)
                .then_some((key.as_str(), group))
        }))
    }

    pub fn live(&self, cfg: &Config, group: &Group) -> Result<Vec<Member>, String> {
        let mut live = Vec::new();
        for member in &group.members {
            if identity(&Job::new(cfg, &member.id))?.as_deref() == Some(&member.question) {
                live.push(member.clone());
            }
        }
        Ok(live)
    }

    pub fn apply(&self, job: &Job, q: &mut Question) -> Result<(), String> {
        if let Some((key, group)) = self.standing(job)? {
            self.live(job.cfg, group)?;
            q.due = group.due;
            q.recommendation.one_way |= group.one_way;
            q.recommendation.about = Some(key.to_owned());
        } else if q.recommendation.about.is_some() || host_path(job, FILE).exists() {
            return Err(format!("{} has an unbound shared question", job.id));
        }
        Ok(())
    }

    pub fn member(&self, job: &Job, q: &Question) -> Result<Member, String> {
        let mut member = Member {
            id: job.id.to_owned(),
            question: q.id.clone().ok_or("question has no identity")?,
            pauses: q.pauses.clone(),
        };
        if let Some((_, group)) = self.standing(job)? {
            self.live(job.cfg, group)?;
            member = group
                .members
                .iter()
                .find(|m| m.id == job.id)
                .expect("standing names a member")
                .clone();
            member.pauses.inherit(&group.pauses);
        }
        Ok(member)
    }

    pub fn clock(&self, job: &Job, q: &Question) -> Result<Pauses, String> {
        Ok(self
            .standing(job)?
            .map(|(_, group)| group.pauses.clone())
            .unwrap_or_else(|| q.pauses.clone()))
    }
}
