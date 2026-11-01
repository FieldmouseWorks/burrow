//! lock-age POLICY RELEASE-AGES BRANCH: see the library's docs.

use std::path::Path;
use std::process::ExitCode;

use lock_age::{lock_age, registry::Real};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, policy, resolver, branch] = &args[..] else {
        eprintln!(
            "lock-age POLICY RELEASE-AGES BRANCH: a lock file refresh takes only releases at\n\
             least the longest release age its Renovate config sets old, or fails."
        );
        return ExitCode::from(2);
    };
    let mut world = Real::new(vec![resolver.clone()]);
    let code = lock_age(
        Path::new("."),
        Path::new(policy),
        branch,
        &mut world,
        &mut |line| eprintln!("{line}"),
    );
    ExitCode::from(code as u8)
}
