//! An authenticated numeric account ID is the authority; a login is a label.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub login: String,
    pub id: u64,
}

impl Author {
    pub fn matches(&self, id: Option<u64>) -> bool {
        self.id > 0 && id == Some(self.id)
    }
}

pub fn author_names(authors: &[Author], separator: &str) -> String {
    authors
        .iter()
        .map(|a| a.login.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_authenticated_account_id_grants_authority() {
        let author = Author {
            login: "sam".into(),
            id: 42,
        };
        assert!(author.matches(Some(42)));
        assert!(!author.matches(Some(99)));
        assert!(!author.matches(None));
        let renamed = Author {
            login: "new-name".into(),
            ..author.clone()
        };
        assert!(renamed.matches(Some(42)));
        assert!(!Author { id: 0, ..author }.matches(Some(0)));
    }

    #[test]
    fn configuration_never_accepts_a_login_without_an_id() {
        for input in [
            r#""sam""#,
            r#"{"login":"sam"}"#,
            r#"{"login":"sam","id":"42"}"#,
        ] {
            assert!(serde_json::from_str::<Author>(input).is_err(), "{input}");
        }
    }
}
