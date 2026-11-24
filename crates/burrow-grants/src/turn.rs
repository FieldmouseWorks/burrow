//! What a job's turn tells its engine plugin besides the request and its
//! grants: the turn's instructions (what the job is, and that it ends by
//! declaring its next step). burrow-proj sets them, a sandbox's runner
//! carries them into the VM, and every engine plugin gives them to its model
//! the way its CLI takes extra instructions. docs/engines.md, "A turn's
//! instructions", states the rule.

/// The environment variable that carries a turn's instructions.
pub const INSTRUCTIONS: &str = "PROJ_APPEND_SYSTEM_PROMPT";

/// The turn's instructions, read through `env`; an empty value is none.
pub fn instructions(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env(INSTRUCTIONS).filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_are_the_variable_and_blank_is_none() {
        let given = |value: &'static str| {
            instructions(move |name| (name == INSTRUCTIONS).then(|| value.to_owned()))
        };
        assert_eq!(
            given("end with proj done").as_deref(),
            Some("end with proj done")
        );
        assert_eq!(given(""), None);
        assert_eq!(given(" \n"), None);
        assert_eq!(instructions(|_| None), None);
    }
}
