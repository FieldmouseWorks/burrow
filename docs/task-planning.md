# Inbox task planning

`burrow.proj.inbox.planning` defaults on, including when unlabelled-issue
triage is disabled. In that case only enrolled, ready and tracker issues
enter the planning queue; unrelated unlabelled issues remain untouched.
Planning uses a new job on the `planner` role, independently of the
implementation engine label. This role defaults to Codex. Its model and
effort come from the role configuration; a Claude selection requires an
explicit small model, so it cannot fall through to the Claude default.
Host planning jobs are required;
enabling planning with `inbox.vm` is rejected at configuration time.

The planner reads the verified issue and repository, including existing
issues and PRs. It receives no previous sessions' transcripts. A planning
prompt contains at most 64,000 bytes of issue input; larger input is retained
as `task-source` inside that job's own permitted directory, with a short
preview and instructions to inspect the complete source in bounded portions.

A `ready` verdict approves the exact verified input for implementation.
An engine label alone cannot bypass planning. A `split` verdict is a JSON
array of 2–16 tasks, each with `title`, `body` and `check`; an optional
`issue` number reuses an existing authorized issue in the same repository.
Titles and task bodies have fixed bounds, and a whole plan is validated
before any child is created. Exact generated or verbatim reused children
retain that size approval, rather than running another planner. Changed
input is checked again. The planner must preserve authorization
boundaries, unresolved work and existing PRs in self-contained children.

The inbox's `.plans/OWNER_REPO-N/tasks.json` is the authoritative ordered
task list. A single comment on the parent mirrors it as a checklist with
child and landed PR links. Children are created as `later`; the repository's
normal event pass makes only the first unfinished child `ready`. The normal
drain and worker admission both check the parent list. A child is offered
once; a later deferral is retained instead of repeatedly paying to replan
unchanged work. A manually applied
engine label cannot admit a later child. Nested splits complete through
their own lists. Issue, PR and turn-completion events advance the list
without a parent job.

A child is done when a merged PR is tied to it, whatever the PR's body
said. Tied means a merged PR from any repository that closes it by a
closing reference, a merged PR the child's own job awaited (`proj await`)
before it ended with its own `proj done`, or one an author recorded. The
job's awaits are kept in the child's plan folder (`.plans/OWNER_REPO-N/
awaited`) only once the child is closed on an accepted completion (the
checks that close an issue with its finished job, a `done` that isn't a
one-way question's timeout, no owner's answer owed), and bound to that
closure: when it closed, and the title and body it closed with. A child
reopened, edited or closed again doesn't inherit them, whether or not a
pass saw it open. The inbox closes an open child (repair-lane children
included) once one of its awaited PRs has merged, and keeps them in that
pass, on the closure GitHub answers the close with, before the job is let
go; a child closed first, by hand say, keeps them the same way before the
job's record goes. Either way the closure's text must be the text the
completion was checked against. An awaited PR counts whenever it
merges, a pass looking again hourly, and a source GitHub can't answer for
never vetoes another that proves the child done. A closed child with
nothing tied is not done: a hand close stays a no-op, but the checklist
says once that the list waits on it, and the owner's notice is ensured
every pass, and hourly, while the list waits, then closed until the
notifier confirms it, whether or not the parent is still open; a pass
scoped to other issues leaves it be. When the work provably landed some
other way, an author records it from the host with `proj plan done
OWNER/REPO#N PR-URL` (`.plans/OWNER_REPO-N/landed`), under the inbox's
pass lock. It refuses inside a job, a GitHub account outside
`inbox.authors`, a repo that isn't one of the machine's projects and a PR
that hasn't merged; the next pass advances the list and closes the child
if it's still open.

Generated children are admitted only through local durable creation receipts;
reused issues still require a configured author. Planning never changes
`inbox.authors`. Comments and edit histories always require configured
authors, including on generated children. Outside-text labels retain their
existing protection. An edited parent pauses child
admission and posts a notice; the task list and completed work remain on
disk for replanning rather than silently executing the old scope.

Each child creation has a durable receipt marker. A lost creation response
is reconciled against all issues, including closed ones, before any further
creation. An ambiguous POST is not repeated when reconciliation cannot find
it: the task remains pending and the inbox retries, reporting the failed
pass through its existing stall path. Definite refusals can be retried.
Checklist updates use the saved comment id, reconciled by marker if needed.

A worker finding an oversized task writes the same JSON array to a file and
runs `proj split FILE`, then ends its session. Its answer is posted before
the old issue job is retired; the new child jobs start fresh. Publish any
implementation progress before splitting, as before any job-ending action.
Planning happens once before a worker starts. Its ordinary PR replies,
questions and cutoff recovery retain the existing lifecycle; a worker
splits again if its task grows. A split declaration freezes new turns while
the host completes its durable child-creation transaction.
Pre-deployment jobs keep their existing recovery and scratch until the
explicit lane migration accounts for their outstanding work.
