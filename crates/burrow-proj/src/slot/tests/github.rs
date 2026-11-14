use super::*;

#[test]
fn github_quota_excludes_only_its_installation_from_admission() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 2);
    cfg.github_meters = Some(tmp.path().join("host-meters"));
    fs::create_dir_all(cfg.github_meters.as_ref().unwrap()).unwrap();
    let git = tmp.path().join("git");
    burrow_test::executable(&git, "#!/bin/sh\necho https://github.com/sam/r.git\n");
    cfg.tools.git = git;
    let job = Job::new(&cfg, "limited");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("app"), "workers").unwrap();
    fs::write(job.path("project"), tmp.path().to_str().unwrap()).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    let name = "2-100-limited";
    fs::write(
        jobs.join(".queue").join(name),
        std::process::id().to_string(),
    )
    .unwrap();
    let mut meter = burrow_github::quota::Meter::new("workers", "sam", now() as u64);
    meter.retry_at = Some((now() + 3600) as u64);
    meter
        .record(&cfg.github_meters.as_ref().unwrap().clone())
        .unwrap();
    assert!(
        look(&cfg, name, &job, Class::Normal)
            .unwrap()
            .line
            .is_empty()
    );
    // The same repository under another App still has its own budget.
    let app = |name: &str| {
        let mut request = crate::job::Request::load(&job).unwrap();
        request.app = Some(name.into());
        request.save(&job).unwrap();
    };
    app("other");
    assert_eq!(look(&cfg, name, &job, Class::Normal).unwrap().line.len(), 1);
    app("workers");
    // At the reset it rejoins without a correction, owner action or API call.
    meter.at += 1;
    meter.retry_at = Some(now().saturating_sub(1) as u64);
    // Replace this fixture directly: the normal writer preserves active holds.
    let file = fs::read_dir(cfg.github_meters.as_ref().unwrap().clone())
        .unwrap()
        .filter_map(Result::ok)
        .find(|f| f.path().extension().is_some_and(|s| s == "json"))
        .unwrap()
        .path();
    fs::write(file, serde_json::to_vec(&meter).unwrap()).unwrap();
    assert_eq!(look(&cfg, name, &job, Class::Normal).unwrap().line.len(), 1);
}
