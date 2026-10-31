# Flaky tests

A flaky test fails some runs and passes others on the same code. Rerunning the
whole gate until it's green wastes the run and hides the flake; skipping the
test by hand loses it. So a gate gives a failed test one more try, a Claude
job fixes what flaked, and a flake nobody fixed blocks again, with nobody in
the loop.

1. A test fails. The gate runs it once more, on the same tree.
2. It fails again: a real failure, and the gate fails. It passes: the test is
   flaky. The gate passes, and reports it.
3. The report opens an issue titled `flaky: TEST`, labelled `flaky`, saying
   where it flaked and the end of what the failed try printed. While that
   issue is open the test is quarantined: each time it flakes again it's
   noted on the issue, and a failure that fails both tries fails the gate,
   as any test's does.
4. The steward (`autoland`, [flakes.py](../modules/autoland/flakes.py)) hands
   the issue to a Claude job, which finds the cause and opens a PR that says
   `Closes #N`. Merged, it closes the issue. The job is the project's, not
   the pipeline's, and has hours (`flaky.jobTimeoutMinutes`). One still open
   a day after its job ends goes to the pipeline's stuck issue; at once, when
   the job was stopped or failed before it finished.
5. Two days after the issue opened, the quarantine runs out. Until the issue
   closes, the test gets no second try: any failure fails the gate at once.
   A flake nobody fixed blocks again instead of passing quietly for good.

The quarantine is the set of open issues, so it changes the moment one opens
or closes, on every branch at once, with no file to edit. Close an issue to
lift its quarantine by hand. A closed one stays lifted: reopened, it
quarantines nothing, and a test that flakes again gets a new issue. A gate
that began while a test was quarantined notes its flake on that issue, even
one closed since, and never quarantines the test again: only a new flake
does. A test with two open issues is as old as its oldest, and one whose
opening time can't be read has run out.

## What a gate does

A gate (a repo's test runner, or the script that drives its suites) reads two
environment variables, which the flaky tool always sets together. Unless both
are set, and neither is empty, it runs as it always did: no second tries, and
every failure fails.

- `FLAKY_EXPIRED`: a file of the names of the tests whose quarantine has run
  out, one a line.
- `FLAKY_REPORT`: a file it appends its report to, one JSON object a line for
  each test that failed, then passed when it ran again:

  ```json
  {"test": "browser/stream", "detail": "the end of what the failed try printed"}
  ```

  `detail` is optional; the issue shows its last 60 lines. It goes on an
  issue, so it carries no secrets and nothing the repo keeps private.

With both set:

- A test that fails runs once more, on its own if it can, the same way. A
  pass reports it and counts as a pass; a second failure fails the gate.
- A test named in `FLAKY_EXPIRED` gets no second try: its failure fails the
  gate at once.
- Anything else the gate does is unchanged: a build that breaks, a check
  that isn't a test, fails as before.

A test's name is what the gate calls it, the same from run to run: ASCII
letters, digits and `_ . : / @ + = - [ ]`, starting with a letter, a digit or
`_`, at most 200 bytes (`browser/thief-terrain`, `tests.test_lock.Units.test_churn`,
`psiv_core::scene::tests::walk`). The flaky tool refuses a report with any
other.

## Around a gate

The flaky tool (`crates/flaky`, Rust, on `gh`) reads the quarantine before the
gate and files its report after. It asks GitHub once a run for the open
`flaky` issues, once for each one's history, and once for each person in
them whether they can push, each conditionally with the last answer's etag
(kept in `$XDG_CACHE_HOME/flaky`), so a run with nothing new costs no rate
limit; it writes only when a test flaked. A quarantine it can't read sets
both variables empty, so the gate gives no test a second try: which tests'
quarantine has run out is unknown. A report it can't file fails a run that
passed, since an unrecorded flake is one nobody fixes. So does a report of a
test whose quarantine had run out: the gate gave it a second try it wasn't
allowed.

In GitHub Actions, burrow's step goes on each side of the gate, with the
job's token, which needs `issues: write`. It builds the tool from its own
commit with the runner's cargo:

```yaml
permissions:
  contents: read
  issues: write
steps:
  - uses: actions/checkout@<commit>
  - uses: FieldmouseWorks/burrow/.github/actions/flaky@<commit>
    with: { step: start }
  - run: ./gate
  - uses: FieldmouseWorks/burrow/.github/actions/flaky@<commit>
    if: ${{ !cancelled() }}
    with: { step: file }
```

A PR from a fork gets a token that can't open issues, and its report is the
fork's own code talking, so there the step shows what it found and files
nothing.

Anywhere else (a machine's own proof jobs, a laptop), `flaky run` wraps the
gate, with `gh`'s login or `GH_TOKEN`:

```sh
flaky run OWNER/REPO --where "PR #12 at abc123" -- ./gate
```

Its exit code is the gate's. `flaky quarantine OWNER/REPO` lists what's
quarantined.

## Who can quarantine a test

Only an issue opened by someone who can push to the repo (GitHub's `write`,
`maintain` or `admin`, asked of GitHub each time, since a member or
collaborator can have only triage, which labels and retitles issues but
pushes nothing), or by the repo's own CI (`github-actions[bot]`), quarantines
a test: anyone can open an issue on a public repo. No other app's issue
does. Every retitle since, and every time its `flaky` label went on, must be
by one of those too, or the issue quarantines nothing: triage could
otherwise rename the CI's issue to point its fix job at another test. Such
an issue stays that way; a test that flakes again gets a new one.

Running out fails closed, so it counts every open `flaky` issue, whoever
opened or changed it: two days after one opened, every test it names, or
named before a retitle, gets no second try. Opening, retitling or
relabelling an issue can take a test's second try away, never give it back.
Closing the issue, or taking its label off, lifts it, as the fix's merge
does; anyone GitHub lets close or unlabel an issue can, which on an
organization's repo includes triage. The tool doesn't read closed or
unlabelled issues, so it doesn't ask who did that. What it gives back is the
second try alone: a test that fails both still fails the gate.

The steward reads the quarantine as its App, as it makes every other call,
and hands a job only the issues its App, one of its authors or the CI
opened; one someone else opened is listed for the owner. The job is
handed each issue's body and what those same logins said on it, each only
if nobody else has edited it since (a body someone else edited is left out,
and the job is told so), and told to read nothing else there: anyone can
comment on a public repo's issue, and triage can edit its body. A job takes
as many of a repo's issues, oldest first, as fit 64 KiB of task, since the
task is one argument and Linux takes none over 128 KiB; the next job takes
the rest.
