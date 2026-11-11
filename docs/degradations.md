# Workflow degradations

A degradation means a workflow completed with reduced service: it lost a
cache or could not save work for the next run. It is independent of the
job's pass/fail result. The shared workflow records these as steps named
`degraded/KEY` and writes the same line in the step summary. Keys use letters,
digits, dots, underscores and hyphens; keep them stable across revisions.
A reporter runs only when its degradation occurs. Skipped reporters are
healthy; successful or failed reporters signal a degradation. This is
readable through the Actions jobs API without downloading logs or artifacts.

A `continue-on-error` step has an ID, a name `soft/KEY`, and an `always()`
reporter for its failed outcome. `burrow-workflows` tests every shared
workflow and rejects an unreported soft step. Its negative fixture removes
the declarations from the keep-builds steps and proves all three are rejected.
Caches can swallow errors as warnings, so outcome alone is insufficient:
lookup before restore distinguishes an empty cache from an entry that failed
to extract; exact-key lookup after save detects a save that produced no entry.
The negative job already makes extraction failure a hard failure when its
lookup found an entry.

Any machine importing burrow's watchdog module can set:

```nix
burrow.watchdog = {
  enable = true;
  degradations = {
    enable = true;
    repos."sam/homelab" = [ "flake.yml" ];
    prThreshold = 3;
  };
};
```

Use the caller workflow filename, not the reusable workflow it calls.
`burrow.github.machineGh` supplies the machine's GitHub identity, which needs
Actions read permission on those repositories. No runner token leaves CI.
The generic example enables this configuration; `watchdog-config` validates
the generated source configuration using its installed binary.

`workflow_run` events start the reader, with the existing events catch-up
covering lost deliveries. It reads the latest two completed default-branch
push runs per configured workflow and completed PR runs created in the last
24 hours. The same key in both default runs, or in `prThreshold` distinct PR
runs in that window, creates a stall. Matrix legs and attempts of one run
count once, and a run's signals are the current result of each of its jobs:
a partial rerun keeps the jobs it didn't rerun and replaces the ones it did.
PR runs count only when the PR resource names one of `authors` and GitHub's
run record associates the observed head SHA and repository with that PR.
The current head may have moved on; an earlier associated snapshot still
counts. Both repositories must be this repository. Event actors (including
reopen, edit and rerun actors) never substitute for PR authorship. Provenance
is rechecked before reading cached observations. Unknown provenance is ignored.
Repositories and workflows have separate keys.

The reader paginates GitHub's `jobs?filter=latest` endpoint, preserving every
returned job rather than inferring identity from display names. GitHub filters
superseded executions server-side, including partial reruns: see its
[API change description](https://github.blog/changelog/2020-03-09-new-filter-parameter-in-workflow-jobs-api/).
Duplicate names and names that change between attempts therefore cannot erase
another current job or preserve a historical result. A new cache namespace
invalidates observations produced by the former name-based reduction.

The reusable policy lives in `burrow_watchdog::degradations`; the
`burrow-degradations` binary supplies Actions observations and writes the
standard watchdog watch file. Completed observations are cached by run ID
and attempt. API failures fail the service and preserve the previous watch;
a reader that stops reporting becomes a source stall itself after two hours.
The watchdog kicks a reread, then hands persistent degradation to its fix job,
then escalates through its existing stuck-issue ladder. A fresh clean default
run clears the consecutive-default condition; PR signals expire with their
24-hour window. A stall that persists keeps its original start time.

A run that never starts has no step outcome to report. That is the separate
missing-runner case in [#598](https://github.com/FieldmouseWorks/burrow/issues/598),
not a degradation signal. Likewise a service that dies before writing its
source is covered by failed-unit and stale-source watches; this signal does
not replace those.
