//! The gate's report: what it appends to the file FLAKY_REPORT names, one
//! JSON object a line for each test that failed, then passed when it ran
//! again on the same tree: `{"test": TEST, "detail": TEXT}` (detail
//! optional).

use serde_json::Value;

use crate::test;

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub test: String,
    /// What the failed try said, as much as the gate chose to keep: never a
    /// secret, and nothing the repo keeps private, since it goes on an issue.
    pub detail: Option<String>,
}

/// The report's entries, or which line is wrong and why: a gate that writes
/// one it can't read has a bug to fix, not a line to skip.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = |why: &str| format!("line {}: {why}", i + 1);
        let v: Value = serde_json::from_str(line).map_err(|e| bad(&e.to_string()))?;
        let obj = v.as_object().ok_or_else(|| bad("not an object"))?;
        if let Some(k) = obj
            .keys()
            .find(|k| !["test", "detail"].contains(&k.as_str()))
        {
            return Err(bad(&format!("unknown key {k:?}")));
        }
        let name = obj
            .get("test")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no test"))?;
        if !test::valid(name) {
            return Err(bad(&format!("{name:?} can't name a test")));
        }
        let detail = match obj.get("detail") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(bad("detail isn't text")),
        };
        out.push(Entry {
            test: name.to_string(),
            detail,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads() {
        let got =
            parse("{\"test\": \"a/b\", \"detail\": \"boom\"}\n\n{\"test\": \"c\"}\n").unwrap();
        assert_eq!(
            got,
            vec![
                Entry {
                    test: "a/b".into(),
                    detail: Some("boom".into())
                },
                Entry {
                    test: "c".into(),
                    detail: None
                },
            ]
        );
        assert_eq!(parse("").unwrap(), vec![]);
    }

    #[test]
    fn refuses() {
        for (line, why) in [
            ("nope", "expected"),
            ("[1]", "not an object"),
            ("{\"detail\": \"x\"}", "no test"),
            ("{\"test\": \"a b\"}", "can't name a test"),
            ("{\"test\": \"a\", \"detail\": 3}", "detail isn't text"),
            ("{\"test\": \"a\", \"outcome\": \"flaky\"}", "unknown key"),
        ] {
            let err = parse(&format!("{{\"test\": \"ok\"}}\n{line}")).unwrap_err();
            assert!(
                err.starts_with("line 2: ") && err.contains(why),
                "{line}: {err}"
            );
        }
    }
}
