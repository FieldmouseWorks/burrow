//! Require a routing role for unattended unit starts, while preserving the
//! ordinary CLI used in interactive shells and existing agent jobs.

use std::env;

use crate::Fail;

pub(super) fn check(role: Option<&str>) -> Result<(), Fail> {
    if role.is_some()
        || env::var_os("INVOCATION_ID").is_none()
        || env::var_os("PROJ_JOB").is_some_and(|id| !id.is_empty())
    {
        return Ok(());
    }
    Err(Fail::new(
        2,
        "proj run from a machine unit needs --role NAME",
    ))
}
