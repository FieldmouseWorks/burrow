//! The probe's vocabulary: what a line of an issue may ask, and nothing else.
//! A line is words split on spaces; the first names the question, the rest
//! are its arguments, each held to its own shape here, so what reaches a
//! command (answer.rs) is a unit name or a time this file made, never the
//! issue's text.

use std::fmt;

/// One question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// RAM, swap, the ARC, pressure, and the cgroups holding the most.
    Memory,
    /// Failed units and jobs under way, the system's and the user's.
    Units,
    /// A unit's journal between two times: one on the allowlist only.
    Journal {
        unit: Unit,
        since: When,
        until: When,
    },
    /// The Remote Control servers: each one's state and memory.
    Rc,
    /// proj's jobs (`proj status`).
    Queue,
    /// What the machine runs: its system, its inputs' revisions, and what
    /// HOST-update last found live.
    Deployed,
    /// The kernel's and systemd-oomd's kills since a time (a day back
    /// unless one is given).
    Oom { since: When },
}

/// What the words are, for an answer that couldn't make sense of a line.
pub const VOCABULARY: &str = "memory | units | journal UNIT SINCE UNTIL | rc | queue | deployed | oom [SINCE], one a line; a time is `now`, `-30s`, `-15m`, `-2h`, `-3d`, `2026-11-01`, `2026-11-01T13:05Z`, `2026-11-01T13:05:30Z` or `@EPOCH`, in UTC";

/// The longest a time can reach back.
const MAX_BACK: i64 = 31 * 24 * 3600;

/// A systemd unit's name: letters, digits and `:_.@-`, `\xNN` escapes (what
/// systemd-escape writes), a known suffix, and no glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit(String);

impl Unit {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// NAME, with `.service` when it has no suffix of its own.
    pub fn parse(word: &str) -> Result<Unit, String> {
        const SUFFIXES: [&str; 8] = [
            ".service", ".timer", ".socket", ".path", ".slice", ".scope", ".mount", ".target",
        ];
        let name = if SUFFIXES.iter().any(|s| word.ends_with(s)) {
            word.to_string()
        } else {
            format!("{word}.service")
        };
        let bytes = name.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' | b'.' | b'@' | b'-' => {
                    i += 1
                }
                b'\\'
                    if bytes.len() >= i + 4
                        && bytes[i + 1] == b'x'
                        && bytes[i + 2].is_ascii_hexdigit()
                        && bytes[i + 3].is_ascii_hexdigit() =>
                {
                    i += 4
                }
                _ => return Err(format!("{word:?} isn't a unit's name")),
            }
        }
        let stem = name.len() - SUFFIXES.iter().find(|s| name.ends_with(*s)).unwrap().len();
        if stem == 0 || name.len() > 256 || !bytes[0].is_ascii_alphanumeric() {
            return Err(format!("{word:?} isn't a unit's name"));
        }
        Ok(Unit(name))
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A time, as the issue gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Now,
    /// So many seconds before the answer.
    Ago(i64),
    /// Seconds since the epoch.
    At(i64),
}

impl When {
    pub fn parse(word: &str) -> Result<When, String> {
        let bad = || format!("{word:?} isn't a time ({VOCABULARY})");
        if word == "now" {
            return Ok(When::Now);
        }
        if let Some(rest) = word.strip_prefix('-') {
            let Some((n, unit)) = rest.char_indices().last().map(|(i, _)| rest.split_at(i)) else {
                return Err(bad());
            };
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            let n: i64 = n.parse().map_err(|_| bad())?;
            let per = match unit {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                "d" => 86400,
                _ => return Err(bad()),
            };
            return match n.checked_mul(per) {
                Some(secs) if secs <= MAX_BACK => Ok(When::Ago(secs)),
                _ => Err(format!(
                    "{word:?} reaches back past {} days",
                    MAX_BACK / 86400
                )),
            };
        }
        if let Some(epoch) = word.strip_prefix('@') {
            return epoch.parse().map(When::At).map_err(|_| bad());
        }
        if word.len() == 10 {
            let date: jiff::civil::Date = word.parse().map_err(|_| bad())?;
            let t = date
                .to_zoned(jiff::tz::TimeZone::UTC)
                .map_err(|_| bad())?
                .timestamp();
            return Ok(When::At(t.as_second()));
        }
        // RFC 3339 in UTC, with or without seconds.
        let full = match word.strip_suffix('Z') {
            Some(t) if t.len() == 16 => format!("{t}:00Z"),
            Some(_) => word.to_string(),
            None => return Err(bad()),
        };
        let t: jiff::Timestamp = full.parse().map_err(|_| bad())?;
        Ok(When::At(t.as_second()))
    }

    /// Seconds since the epoch, `now` being the answer's.
    pub fn at(self, now: i64) -> i64 {
        match self {
            When::Now => now,
            When::Ago(s) => now - s,
            When::At(t) => t,
        }
    }
}

impl Query {
    /// One line of an issue.
    pub fn parse(line: &str) -> Result<Query, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((first, args)) = words.split_first() else {
            return Err(format!("an empty line ({VOCABULARY})"));
        };
        let none = |q: Query| {
            if args.is_empty() {
                Ok(q)
            } else {
                Err(format!("`{first}` takes nothing after it ({VOCABULARY})"))
            }
        };
        match first.to_ascii_lowercase().as_str() {
            "memory" => none(Query::Memory),
            "units" => none(Query::Units),
            "rc" => none(Query::Rc),
            "queue" => none(Query::Queue),
            "deployed" => none(Query::Deployed),
            "oom" => match args {
                [] => Ok(Query::Oom {
                    since: When::Ago(86400),
                }),
                [since] => Ok(Query::Oom {
                    since: When::parse(since)?,
                }),
                _ => Err(format!("`oom` takes one time at most ({VOCABULARY})")),
            },
            "journal" => match args {
                [unit, since, until] => Ok(Query::Journal {
                    unit: Unit::parse(unit)?,
                    since: When::parse(since)?,
                    until: When::parse(until)?,
                }),
                _ => Err(format!(
                    "`journal` takes a unit and two times ({VOCABULARY})"
                )),
            },
            _ => Err(format!("{first:?} isn't a question ({VOCABULARY})")),
        }
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Query::Memory => f.write_str("memory"),
            Query::Units => f.write_str("units"),
            Query::Rc => f.write_str("rc"),
            Query::Queue => f.write_str("queue"),
            Query::Deployed => f.write_str("deployed"),
            Query::Oom { since } => write!(f, "oom {}", show(*since)),
            Query::Journal { unit, since, until } => {
                write!(f, "journal {unit} {} {}", show(*since), show(*until))
            }
        }
    }
}

fn show(w: When) -> String {
    match w {
        When::Now => "now".into(),
        When::Ago(s) if s % 86400 == 0 => format!("-{}d", s / 86400),
        When::Ago(s) if s % 3600 == 0 => format!("-{}h", s / 3600),
        When::Ago(s) if s % 60 == 0 => format!("-{}m", s / 60),
        When::Ago(s) => format!("-{s}s"),
        When::At(t) => jiff::Timestamp::from_second(t).map_or_else(
            |_| format!("@{t}"),
            |t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{Query, Unit, When};

    #[test]
    fn each_word() {
        assert_eq!(Query::parse("memory"), Ok(Query::Memory));
        assert_eq!(Query::parse("  Units "), Ok(Query::Units));
        assert_eq!(Query::parse("rc"), Ok(Query::Rc));
        assert_eq!(Query::parse("queue"), Ok(Query::Queue));
        assert_eq!(Query::parse("deployed"), Ok(Query::Deployed));
        assert_eq!(
            Query::parse("oom"),
            Ok(Query::Oom {
                since: When::Ago(86400)
            })
        );
        assert_eq!(
            Query::parse("oom -2h"),
            Ok(Query::Oom {
                since: When::Ago(7200)
            })
        );
        assert_eq!(
            Query::parse("journal remi-update -2h now"),
            Ok(Query::Journal {
                unit: Unit("remi-update.service".into()),
                since: When::Ago(7200),
                until: When::Now,
            })
        );
    }

    #[test]
    fn nothing_else() {
        for line in [
            "",
            "memory please",
            "rm -rf /",
            "journal",
            "journal remi-update -1h",
            "journal remi-update -1h now extra",
            "journal 'claude-rc@*' -1h now",
            "journal claude-rc@* -1h now",
            "journal -u -1h now",
            "journal --since=x -1h now",
            "journal a;b -1h now",
            "journal a$(id) -1h now",
            "journal ../etc -1h now",
            r"journal a\x2 -1h now",
            "journal .service -1h now",
            "journal remi-update yesterday now",
            "journal remi-update -1y now",
            "journal remi-update -40d now",
            "journal remi-update --1h now",
            "oom -1h -2h",
        ] {
            assert!(Query::parse(line).is_err(), "{line:?} parsed");
        }
    }

    #[test]
    fn unit_names() {
        assert_eq!(
            Unit::parse(r"claude-rc@LoK\x2dweb").unwrap().as_str(),
            r"claude-rc@LoK\x2dweb.service"
        );
        assert_eq!(
            Unit::parse("nix-gc.timer").unwrap().as_str(),
            "nix-gc.timer"
        );
        assert_eq!(
            Unit::parse("user@1001.service").unwrap().as_str(),
            "user@1001.service"
        );
    }

    #[test]
    fn times() {
        assert_eq!(When::parse("now"), Ok(When::Now));
        assert_eq!(When::parse("-30s"), Ok(When::Ago(30)));
        assert_eq!(When::parse("-15m"), Ok(When::Ago(900)));
        assert_eq!(When::parse("-3d"), Ok(When::Ago(3 * 86400)));
        assert_eq!(When::parse("@1700000000"), Ok(When::At(1_700_000_000)));
        assert_eq!(When::parse("2026-11-01"), Ok(When::At(1_793_491_200)));
        assert_eq!(
            When::parse("2026-11-01T13:05Z"),
            Ok(When::At(1_793_491_200 + 13 * 3600 + 300))
        );
        assert_eq!(
            When::parse("2026-11-01T13:05:30Z"),
            Ok(When::At(1_793_491_200 + 13 * 3600 + 330))
        );
        assert!(
            When::parse("2026-11-01T13:05").is_err(),
            "a time with no zone"
        );
        assert!(
            When::parse("2026-11-01T13:05+02:00").is_err(),
            "a time outside UTC"
        );
        assert_eq!(When::Ago(60).at(1000), 940);
    }

    #[test]
    fn says_itself_back() {
        for line in [
            "memory",
            "oom -1d",
            "journal remi-update.service -2h now",
            "journal a.timer 2026-11-01T13:05:00Z now",
        ] {
            assert_eq!(Query::parse(line).unwrap().to_string(), line);
        }
    }
}
