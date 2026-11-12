# One chat for every project

You talk to one claude.ai project chat. The work runs on the box, inside each
project: its checkout, its agent instructions, its tools and logins. Nothing gets
copied to the cloud and back.

## Four doors, cheapest first

A thread takes the first door that fits. A probe and a run start no Claude on
the box; a job or a session holds an agent, and its memory, for as long as it
works.

1. **Probe:** a fact about the box that only needs reading (its memory, a
   unit's log, what's deployed, the job queue), answered in seconds
   ([below](#probes-facts-with-no-claude)).
2. **Run:** a build, gate or proof whose exit is its verdict. The repo names
   it in `proofBuilds` or `proofRuns`, and the steward runs it on each
   settled PR's head and posts its status
   ([the PR flow](repo-baseline.md#the-pr-flow-the-steward), step 7). A job
   told to run one command and report, or a `proofJobs` prompt that asks for
   no more, is a run that hasn't been written yet.
3. **Job:** code or judgment in a project, handed over as an issue
   ([the inbox](#jobs-the-inbox-nothing-to-approve)). It waits for a slot,
   and holds no agent until it gets one.
4. **Session:** work the owner steers live, or that can't wait for a slot:
   Remote Control on the project's server
   ([below](#sessions-remote-control-one-tap-per-thread)). It costs the owner
   a tap, and holds a Claude while it's open.

```
claude.ai project chat                   GitHub                  the box
──────────────────────                   ──────                  ──────────────────────
thread ── issue labelled `probe` ───▶ the machine's repo ──▶ burrow-probe (on its webhook)
       ◀── answers, then closed ────

thread ── issue labelled `engines` ──▶ the machine's repo ──▶ burrow-engines-door (on its webhook)
       ◀── active setup and engines, then closed

thread ── issue labelled `undo` ────▶ the machine's repo ──▶ burrow-ledger-door (on its webhook)
       ◀── the undo's PR, then closed

thread ── issue labelled `claude` ──▶ the project's repo ──▶ proj-inbox (on its webhook)
       ◀── answer as a comment ─────                     ──▶ proj run / proj reply

thread ── Remote Control (one tap) ───────────────────────────▶ claude-rc@PROJECT
       ◀── results, PRs, questions ────────────────────────────  (own worktree)
```

## Probes: facts with no Claude

`burrow.probe` (crates/burrow-probe) answers read-only questions about the
box on a private repo's issues (`burrow.probe.repo`, the machine flake's
own, say), in seconds and without starting a Claude.

- **Ask:** open an issue there labelled `probe` (`probe.label`), as one of
  `probe.authors` (the inbox's, unless set), or as the configured
  `github.machinery` App. A job uses the in-job path below. Ask with a question a line:
  - `memory`: RAM, swap, the ZFS ARC, pressure, and the cgroups holding the most
  - `units`: failed units and jobs under way, the system's and the user's
  - `journal UNIT SINCE UNTIL`: a unit's last 400 lines in that span, for the
    units on `probe.journal.units` (and `userUnits`) only. The system defaults
    include Renovate's full pass (`renovate.service`) and repository passes
    (`renovate@*.service`), so jobs can diagnose their failed runs.
  - `rc`: each Remote Control server's state, memory and restarts
  - `queue`: `proj status`
  - `deployed`: the running and booted systems, the generation, the inputs'
    revisions, and what `HOST-update` last found live
  - `contains REPO COMMIT`: whether a commit is an ancestor of the running
    input revision, using its repository identity and local history; unavailable
    or ambiguous history is reported as unknown
  - `usage [SINCE]`: engine turns by role, inbox label and opener category,
    or starter; defaults to `-5h`. Claude turns include stream token counts
    and cost. Result tokens are per invocation; only session cost totals are
    subtracted across resumes. Every appended invocation is accounted for,
    including unfinished runs and zeroed crash results. Partial input/cache
    tokens exclude duplicate message IDs; output counts completed runs only.
    Incomplete cost is labelled `known-cost`, with unavailable coverage and
    gaps carried into a later session total marked separately.
    Meter readings over the same span show five-hour and weekly utilization, including readings at 100%.
    No prompts or issue titles are reported. Attribution predating structured
    job metadata is marked unknown where unavailable. Filtering uses immutable
    first-execution records across allowance waits and retries. Invocation
    boundaries are written before the child starts; failed spawns undo their
    records without disturbing earlier executions. Historical
    turns without those records are counted separately, outside span totals;
    an empty stream without a start record is reported as having no recorded
    execution. All job-state reads pin regular files and directories without
    following symlinks, including requests through the sandbox host door.
  - `oom [SINCE]`: the kernel's and systemd-oomd's kills, a day back by default

  A time is `now`, `-30m`, `-2h`, `-3d`, `2026-11-01`, `2026-11-01T13:05Z`
  or `@EPOCH`, in UTC. An empty body asks its title.
- **Answer:** one comment with every answer, then the issue is closed.
  Reopen it to ask again. An issue whose body or title someone else changed
  (GitHub lets a collaborator edit anyone's) runs nothing: ask in a new one.
- **What keeps it safe:** no shell, and nothing of the issue's text reaches a
  command but a unit name or a time the grammar made; journals only of the
  units on the allowlist; no answer at all in a repo GitHub doesn't call
  private; and every answer through a secret scrubber first
  (`tests/probe.nix` plants a token to prove it). Anyone else's issue runs
  nothing.
- On the box, `burrow-probe ask QUESTION` answers one on the terminal, as
  the machine says it.
- Inside any job, use `proj probe QUESTION...` first. The host parses the same
  grammar, reads its own machine state and scrubs the answer before returning
  it to the job, including jobs inside sandboxes. Jobs never open probe issues.

A job's question uses `proj ask --default "TEXT" [--one-way]`; a bare ask is
refused. `burrow.proj.askTimeout` is its deadline in seconds (6h by default),
kept across unrelated updates. A reply resumes it normally. Without one, a
reversible default resumes the job with “no answer by TIME” and instructions
to take that recommendation and report it on the issue. A one-way default
completes without executing the recommendation: its issue stays open under
the owner label and its drain claim is released. Questions from drained,
bot-started or stuck-issue jobs carry the owner's waiting label and are listed
in the pipeline's owner issue, so they have a reader. Until a reply or a
reversible default resumes it, or `proj stop` ends it, the job is still at work:
`proj lifecycle ID` says how a job stands and what it holds open (its
question, a one-way one left for the owner, the PRs and issues it awaits),
read together under its turn-start lock, and the steward never takes one that
holds anything for ended, however its turn ended, or starts a second job on
its work.

## Engines with no agent

Use the engines door when the chat asks to move the machine between saved
engine setups, or back to the previous one. No job or session needs to start.
The machine flake enables `burrow.proj.engines.door` and sets its `repo`;
`label` defaults to `engines`, and `authors` to the inbox's authors.

Open an issue there with that label, as an author, whose body is one line:
`list`, `use NAME` or `use back`. The door uses proj's checked switch as the
job user, comments with the old and new active setup and each role/scope's
engine, then closes the issue. It never posts model choices. An unknown
setup, bad file or different request changes nothing; text anyone outside
the authors wrote or edited is ignored. Each open cycle is one request:
reopening asks again even with unchanged text. Before a switch, proj records
its resolved destination in a durable journal. The door and CLI recover it
under the same lock before another change; completed results stay attached
to that cycle, so interruption or a failed GitHub reply cannot repeat `back`.

On the machine, `proj engines` lists saved setups, marks the active one and
names the previous one. `proj engines save NAME` checks the current roles
file and saves it under `roles.d/NAME`, beside that file in the user's
untracked config. Names use lowercase letters, digits and hyphens; `back`
is reserved. Replacing a saved setup needs `--replace`.
`proj engines use NAME` checks it with the turn's parser, then atomically
replaces the roles path with a link to it. An unmatched plain roles file is
kept as `roles.d/unsaved-<UTC time>` first. `proj engines use back` returns
to the setup replaced by the last switch. Existing plain roles files keep
working without creating setups.

`proj check-config` checks the module's config and the current roles file.
The update's health check runs it as the job user after every switch; it
must pass before confirmation, and a failure notice names the bad line.
Malformed roles still fail every turn: the checked switch and deploy check
stop those mistakes before new turns have to discover them.

## Undo with no Claude

`burrow.ledger.door` (crates/burrow-ledger-door) asks Conary's ledger for an
undo on the same kind of issue: `undo`-labelled (`door.label`), in the
probe's repo and from its authors unless set otherwise (`door.repo`,
`door.authors`).

- **Ask:** one line, in `cn undo`'s words: `undo OWNER/REPO#N` (the change
  that PR is), `undo --actor SESSION_URL` (everything an agent session
  landed, one PR per repo) or `undo --since TIME --repo OWNER/REPO`
  (everything that landed there after an RFC 3339 time). Which repos it
  reaches is the ledger's to say: cn refuses a change or a repo it keeps
  nothing of.
- **Answer:** `cn undo` runs as the ledger's writers do, as the machine's
  App, and the comment links each undo's PR, with any change left out (its
  revert conflicts with the rest: ask again) or already gone from trunk.
  Then the issue is closed. GitHub being unavailable leaves it open for the
  next pass. When cn refuses, nothing was undone and its words say why;
  when it stops partway, the comment links what it found or opened before
  then, a PR it hadn't recorded yet included, and asking again finds those
  rather than open more.
- **What keeps it safe:** the probe's door, whose text runs only when its
  authors alone wrote it; no shell, and nothing of the issue's text reaches
  `cn` but the words the grammar checked. The PR is a change like any other:
  the steward settles it, and undoing an undo puts things back.

## Jobs: the inbox, nothing to approve

`burrow.proj.inbox` (proj-inbox, in `crates/burrow-proj`) turns GitHub issues
into project jobs.

- **Start:** open an issue in the project's own GitHub repo (one checked out in
  `burrow.projectsDir`) with a label from `inbox.labels`, as one of
  `inbox.authors`. Put the whole brief in the body: the job knows only the
  issue. Within seconds it's a `proj run` job in that project, and the
  issue gets an "On it" comment naming the job.
- **Engine:** `codex` for code work with a checkable bar: a gate or tests.
  `claude` for judgment, anything the owner reads, and one-way calls. If an
  issue has both, Codex takes it. VM jobs stay Claude-only.
- **Answer:** when the job finishes a turn, its answer is posted to the issue.
- **Follow-ups:** reply on the issue. Replies from `inbox.authors` go to the
  same job as its next turn (`proj reply`), with everything it already did in
  context. A reply sent while a turn runs waits for it to finish.
- **Done:** a turn that ends `proj done` closes the issue once its answer is
  posted, unless a reply is waiting or the issue changed since the job was
  handed it (its title or text edited, a comment edited or deleted): then the
  job is handed the issue as it stands instead. The stuck issue is excepted:
  the steward closes that. Reopen it to pick the work back up, as a fresh job from the
  issue as it stands. Closing it yourself, or taking the label off, ends the
  job too. If author replies arrived during its last turn and were never
  handed on, the inbox posts links to them before retiring the job. Reopen
  and restore the job label to pick them up. Notice delivery retries separately
  from stopping the old job, so a reopened issue starts afresh even while a
  notice is pending. Long lists of unread replies are delivered in batches.
- **Who:** only `inbox.authors` can hand the box work; everyone else's issues
  Author lists use `{ login = "NAME"; id = ACCOUNT_ID; }`, shared by the inbox,
  engines door, probe and undo door. The positive numeric account ID grants
  access; the login is only a display label. GitHub's authenticated REST
  `user.id` and GraphQL `databaseId` identify openers and every writer,
  including editors, title changers, commenters and edit deleters. A reused
  login never transfers access. An App's `accountId` is its bot user's ID,
  distinct from its App `id`.

  and comments are ignored, labelled or not, except that a labelled issue
  someone else opened gets one comment saying no job will start. Claude jobs
  have the box user's permissions. Codex jobs write
  only their project, scratch, job dir, git metadata and a /tmp of their own,
  and see nothing else but the machine's system paths, with network access
  (the App's token socket included) but no server of the user's own (the
  runtime dir's D-Bus and systemd manager, tmux, any socket the user serves
  outside those paths); sudo cannot elevate there.
- Jobs are headless (`claude -p` or `codex exec --json`).
  They can't stop to ask mid-turn: they end the turn with their question and
  pick up from your reply.
- On the box: `proj-inbox status` maps issues to jobs, `proj log ID` shows an
  answer.

### Triage and the drain: issues nobody labelled

With `inbox.triage.enable`, an issue doesn't need a label to get done. Every
open issue one of `inbox.authors` opened, with none of the labels below, an
engine's or one in `triage.skip` (probe, undo, flaky: other machinery's), is
sorted by a job of its own: the `triage` role's engine reads the issue and the
repo, changes nothing, and answers with a verdict the inbox carries out.

| Verdict | What happens |
| --- | --- |
| `ready` | labelled `ready`: an agent can finish it now and prove it |
| `later` | labelled `later`: real, but too big, vague or waiting on other work |
| `tracker` | labelled `tracker`: a checklist, milestone or dashboard |
| `owner` | labelled `triage.ownerLabel`, assigned to `triage.owner`, with the call they're asked |
| `duplicate #M` | closed as not planned, saying of what |
| `done` | closed as completed, saying where |

An answer with no verdict is `later`, saying so. A sorting job that ends with no
answer at all (its engine refused it, say) leaves the issue as it was: it's
sorted again once it changes, or once another issue's verdict shows sorting
works again. A verdict on an issue whose text or authors' comments changed since it
was sorted, even partway through carrying it out, is dropped and the issue
sorted again as it stands. One GitHub took only part of is finished on a later
pass, the same steps from where it stopped, with the sorting label last. Only the comments that close an
issue or ask the owner something are posted; a label alone says nothing, so a
backlog being sorted doesn't flood anyone's mail. Sorting jobs wait in line
behind work someone asked for, `triage.atOnce` at a time across every repo, and
each one's end runs a full pass, so a quiet repo's backlog is worked through
too. Take a label off to have an issue sorted again.

With `triage.drain` set to an engine, the drain hands `ready` issues to it: each
pass, if that engine has an idle slot and fewer than `triage.drainJobs` handed-on
issues have queued or running turns (or are awaiting their first job), the
first `ready` issue in any repo's last-listed open issues, checked with GitHub
as it stands, gets
the engine's inbox label and is a job like any other from there, closed when
it's done. A handed-on job takes its place in line rather than going first.
Jobs parked on awaits or questions release drain capacity; waking turns join the normal
queue. Paused repos reserve no drain capacity and their automated wakeups wait
for the pause to lift.
Rank issues with `priority: high`, `priority: medium` or `priority: low`.
Priority comes first (no label means medium), then pipeline repos, then oldest
first. Triage includes a priority in its verdict and adds it only if no priority
label exists when it acts. `proj status` and the probe's `queue` show drain
capacity used/total and the handoffs using it, then the next five candidates from the drain's cached lists; eligibility and slot availability
are checked when handing one on.
While a `ready` issue waits for a slot, every job turn's end kicks a pass, since
any of them frees one; a usage hold books one for when it resets.

Strangers' issues are never sorted or handed on: an agent reads them with the
box's logins. Nor is one labelled `inbox.outsideLabel` (`outside text`), whoever
filed it: whatever files text from outside the authors (a game server filing
its players' reports, say, even with an author's token) puts that label on as
it opens the issue, and no job ever starts from it, engine label or not. That
text gets to an agent only through an author who opens an issue of their own
for it. A job whose issue gets the label later, or whose opener
leaves `inbox.authors`, is stopped at once, even while it waits for a slot or
as its issue closes, and the issue hears why.

Roles on the box live in `~/.config/proj/roles`: `ROLE ENGINE`, with
`ENGINE` either `claude` or `codex`. Scope a key to one repo with
`ROLE@OWNER/REPO`:

```
steward-handed@octocat/project  codex
steward-handed                  claude
```

The scoped line wins for that repo, regardless of line order; repo names
match case-insensitively. Other repos, and jobs with no repo, use the bare
role. With neither, the role uses Claude. Duplicate keys fail the turn and
name both lines, including scopes spelled in different cases. Other invalid
lines fail the turn and name the line. `proj check-config` validates the
deployed configuration, all roles and all models lines without starting a
job; updates run it as the job user in their health check before confirming
([updates](../README.md)). Add
`over PCT ENGINE` to move a turn to that engine when its own engine
is short: any window on its meter (the five-hour session or the week) at
PCT with no credits to carry on, or, for Claude, the machine already out of
Claude's usage. It moves only when the other engine's meter has room:
credits, or no window used up, so Codex's credits count once its plan's
windows are spent. Missing readings, or readings an hour old, move nothing.
`burrow.usageReset` supplies both meters and keeps their history in
`meters/log`.
Each engine keeps its own conversation when a role moves between them.

The roles file picks only the engine, so a saved engine setup swaps it
whole. What each role runs at on each engine lives beside it, in
`~/.config/proj/models`, and stays put when setups switch:
`ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]`.

```
steward-conflictFixed  claude  SMALLER-MODEL-ID  medium
steward-judged         claude  -                  high
```

`-` keeps the engine's own model, and a line of just `ROLE ENGINE -` keeps
all of its own settings, so a repo's line can undo a bare one. Effort is
Claude Code's `--effort` (`low`, `medium`, `high`, `xhigh`, `max`) or
Codex's `model_reasoning_effort`, passed as written: any lowercase word,
since Codex's models define their own levels. The line for the engine the
turn ends on applies, overflow and the judge's Claude included; scopes,
duplicates and bad lines work as in the roles file. A role with no line
keeps the engine's own settings: for Claude, the job user's
`~/.claude/settings.json` `model` and the machine's `burrow.claude.effort`.
New sandbox guests (`agent new`, `proj run --vm`) copy the user's `model`
and its `env.CLAUDE_CODE_SUBAGENT_MODEL` (the model Claude Code's subagents
run on) with `env.CLAUDE_CODE_SUBAGENT_MODEL_FORCE` (which moves Explore,
whose own definition inherits the session's model) at creation; without them they use Claude Code's defaults. This is
runtime data, not part of the guest image. Model ids live only in these
untracked files: which model does what is the machine's business.

Codex jobs share the owner's ChatGPT plan with the PR reviewer, but have
an independent login: `autoland-codex-login job` logs the job user in,
`autoland-codex-login review` logs the reviewer in. Each is a device-code
login; don't copy tokens between them. Codex jobs have their own small
pool (`burrow.proj.codex.slots`, two by default), leaving the reviewer first
claim on the plan. Usage refusals wait for the reset and resume themselves.
The job user's `.codex/config.toml` picks the default model. Machine
instructions and one-way command rules are generated alongside Claude's.
Codex commits carry `Engine: codex` (a push of a commit without it is
refused); after the usual review, a Claude job judges whether their PR lands, as it does one GitHub won't list every commit of. A steward judge always uses Claude.

### For the Claude in the chat (coordinator and threads)

1. Open the issue (a cloud thread's GitHub tools can do it; no card, nothing
   for the owner to approve).
2. Check back with a scheduled reminder, spaced out (5 minutes, then 15, 30,
   an hour): read the issue's comments for the answer. When the job opens a PR,
   subscribe to the PR; its events wake you by themselves.
3. Tell the owner the result or the question in your own words. Post their
   answer as a reply on the issue, and check back again.
4. Close the issue when the owner has what they asked for.

## Sessions: Remote Control, one tap per thread

For work the owner wants to watch or steer live, a thread starts a Remote
Control session on the project's server (`modules/remote-control.nix`): the
project's Claude, in its own git worktree, in `burrow.remoteControl.permissionMode`
(bypassPermissions alongside `burrow.claude`, so only the one-way doors ask).
The owner's messages in the thread reach it directly. The catch: each thread
shows the owner an approval card first, and a box with no desktop app can't be
pre-approved.

- Every project in `burrow.projectsDir`, a git checkout with an origin, has an
  always-on `claude remote-control` server that trusts its folder by itself, so
  it never sits at a prompt nobody can see. The machine's own flake always has
  one. Any other folder, a linked git worktree included, gets none.
- With `burrow.remoteControl.spawn = "worktree"` (the default) each session
  started from claude.ai gets its own git worktree, so several threads can work
  in one project at once. A server holds no session until one is asked for
  (`burrow.remoteControl.idleSession`). The servers keep memory back when
  the machine runs short (`burrow.remoteControl.memoryLow`), and each server
  and its sessions have a backstop of half the machine
  (`burrow.remoteControl.memoryMax`): a session that grows past it is killed
  alone, and the thread can pick it up again from its transcript
  (`tests/remote-control-memory.nix`).
- A switch never restarts a server, since that would cut off the session
  confirming it. `claude-rc-scan` (on a switch, when a session's status changes, and when the
  quiet spell it's waiting on ends) restarts
  any server still running an older tmux or `claude-rc-serve` (older Claude
  Code or permission mode) once none of its sessions is working or has done anything
  in the last 30 minutes; a session that's open but idle doesn't count
  (`tests/remote-control.nix`).
- What the owner has approved in advance goes in `burrow.claude.standingAuthority`:
  a section of the machine CLAUDE.md, so no session asks them to approve it
  again just because the instruction came through a thread or an issue.
- A thread holds one Remote Control session at a time. To give it more to do
  after it started, start Remote Control again from the same thread with new
  instructions; they reach the running session.
- The machine's own flake folder is the operator. Its sessions stay in the
  folder itself, since that checkout is what `HOST-switch` builds.

## What's running

- Inbox and other proj jobs: `proj status` and `proj-inbox status` on the box;
  open issues labelled `claude` or `codex` on GitHub.
- Remote Control: the chat's thread list; on the box, `tmux -L rc-NAME attach`
  shows a project's server and its sessions.

## Worktrees and untracked files

A worktree has the tracked files only. If a project needs untracked ones to
build (`.env`, a local config), list them in `.worktreeinclude` at the repo
root and Claude Code copies them into each new Remote Control worktree.
Dependencies (`node_modules`, `target/`) get rebuilt there; set
`burrow.remoteControl.sameDir = [ "NAME" ]` for a project where that's too
slow.

A job ending with `proj ask --default "TEXT" [--one-way]` sends its question through `burrow.tools.notify`
when configured. The open-question state survives automated resumes (#549).
Each question has its own notification key and durable outbox beside the turn
log. `proj-watch` retries opens with `notify ensure` (no daily reminders),
since the notifier's zero exit does not acknowledge delivery. It retains closes
until `notify pending` confirms closure. A human reply closes the notice only
after its new turn is accepted: host-owned resume receipts distinguish preparatory
prompts and turn counts from acceptance. The launcher and `_exec` can both
record acceptance; interrupted cleanup is replayed before the watcher acts.
Failed or killed starters leave the outstanding question and its authority intact.
When its unit is known gone, an unaccepted preparation is retained as a failed
turn with its undelivered prompt; it no longer defers the original question
deadline. Active units and unknown lifecycle states stay protected.
Completion (native or VM) or an explicit stop also closes the notice; a later question gets its own key even if
an older close is still pending. Notification errors do not interrupt turn
finalization, watchdog updates, completion kicks or scratch cleanup.

### GitHub's installation quota

Jobs treat GitHub quota as a meter. The gh door samples `GET /rate_limit`
with the command's own installation token and records only the App, owner,
remaining REST budget, reset, reading time and any primary or secondary
hold. `proj usage` prints one JSON value: Claude's hold fields, when held, and these readings as its `github` array; the typed
`burrow_proj::github_quota::meters` view is available to the live snapshot.
App plus owner identifies the installation across repositories and token
renewals. A fresh reading cannot release a secondary hold before its reset.

The gh door runs each command with gh's own transport log (`GH_DEBUG=api`) and
reads the last response's status and headers from it, hiding those lines from the
job; only a failed command whose last response is a limit (429, a 403 with
Retry-After or an exhausted budget, or GraphQL `RATE_LIMITED` alone) refuses.
The meter's App is the one whose socket minted the token.

A turn that encounters a limit finishes its current run and parks until
GitHub's reset. It publishes a host quota notice, without opening a question
or a correction turn. Other running jobs continue; queued turns on that
installation wait for the reset before taking a slot. Permission denials
remain errors.

`proj await` retains targets when quota prevents taking their baselines.
The watcher registers them after reset without another engine turn, then
resumes the job on news from those targets. If registration meets another
limit it retains the intent and books that reset. A permission error or a
target closed meanwhile resumes the job with the failed registration's
cause. `proj stop` cancels these saved targets along with other waits.
