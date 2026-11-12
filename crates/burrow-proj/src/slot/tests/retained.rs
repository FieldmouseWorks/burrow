use super::*;

#[test]
fn retained_queue_renames_occupy_one_logical_project_place() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 2);
    let job = crate::job::Job::new(&cfg, "renamed");
    fs::create_dir(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), "/ordinary").unwrap();
    crate::retry::Checkpoint::retry(&job, "1", false, crate::now_nanos(), "refused").unwrap();
    let queue = jobs.join(".queue");
    for name in ["0-100-renamed", "1-100-renamed", "2-100-renamed"] {
        fs::write(queue.join(name), "dead\n").unwrap();
    }
    let seen = look(&cfg, "1-100-renamed", &job.dir, Class::First).unwrap();
    assert_eq!(seen.line.len(), 1);
    assert_eq!(seen.line[0].name, "1-100-renamed");
    assert_eq!(fs::read_dir(queue).unwrap().count(), 1);
}
