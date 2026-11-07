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
  `probe.authors` (the inbox's, unless set), with a question a line:
  - `memory`: RAM, swap, the ZFS ARC, pressure, and the cgroups holding the most
  - `units`: failed units and jobs under way, the system's and the user's
  - `journal UNIT SINCE UNTIL`: a unit's last 400 lines in that span, for the
    units on `probe.journal.units` (and `userUnits`) only
  - `rc`: each Remote Control server's state, memory and restarts
  - `queue`: `proj status`
  - `deployed`: the running and booted systems, the generation, the inputs'
    revisions, and what `HOST-update` last found live
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
- **Done:** close the issue, or take the label off.
- **Who:** only `inbox.authors` can hand the box work; everyone else's issues
  and comments are ignored, labelled or not, except that a labelled issue
  someone else opened gets one comment saying no job will start. Claude jobs
  have the box user's permissions. Codex jobs use
  workspace-write with network access; sudo cannot elevate there.
- Jobs are headless (`claude -p` or `codex exec --json`).
  They can't stop to ask mid-turn: they end the turn with their question and
  pick up from your reply.
- On the box: `proj-inbox status` maps issues to jobs, `proj log ID` shows an
  answer.

Roles on the box live in `~/.config/proj/roles`: `ROLE ENGINE[:MODEL]`,
with `ENGINE` either `claude` or `codex`. Scope a key to one repo with
`ROLE@OWNER/REPO`:

```
steward-handed@octocat/project  codex
steward-handed                  claude
```

The scoped line wins for that repo, regardless of line order; repo names
match case-insensitively. Other repos, and jobs with no repo, use the bare
role. With neither, the role uses Claude's default. Duplicate keys fail the
turn and name both lines, including scopes spelled in different cases.
Other invalid lines fail the turn and name the line. Add
`over PCT ENGINE[:MODEL]` to move a turn to that engine when Claude's weekly
meter reaches PCT. Missing readings, or readings an hour old, move nothing.
`burrow.usageReset` supplies the meter and keeps its history in `meter.log`.
Each engine keeps its own conversation when a role moves between them.

Codex jobs share the owner's ChatGPT plan with the PR reviewer, but have
an independent login: `autoland-codex-login job` logs the job user in,
`autoland-codex-login review` logs the reviewer in. Each is a device-code
login; don't copy tokens between them. Codex jobs have their own small
pool (`burrow.proj.codex.slots`, two by default), leaving the reviewer first
claim on the plan. Usage refusals wait for the reset and resume themselves.
The job user's `.codex/config.toml` picks the default model. Machine
instructions and one-way command rules are generated alongside Claude's.
Codex commits carry `Engine: codex`; after the usual review, a Claude job
judges whether their PR lands. A steward judge always uses Claude.

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
