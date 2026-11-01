# One chat for every project

You talk to one claude.ai project chat. The work runs on the box, inside each
project: its checkout, its CLAUDE.md, its tools and logins. Nothing gets
copied to the cloud and back.

## Four doors, cheapest first

A thread takes the first door that fits. A probe and a run start no Claude on
the box; a job and a session each hold one, and its memory, for as long as it
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
3. **Job:** work in a project that needs judgment, handed over as an issue
   ([the inbox](#jobs-the-inbox-nothing-to-approve)). It waits for a slot,
   and holds no Claude until it gets one.
4. **Session:** work the owner steers live, or that can't wait for a slot:
   Remote Control on the project's server
   ([below](#sessions-remote-control-one-tap-per-thread)). It costs the owner
   a tap, and holds a Claude while it's open.

```
claude.ai project chat                   GitHub                  the box
──────────────────────                   ──────                  ──────────────────────
thread ── issue labelled `probe` ───▶ the machine's repo ──▶ burrow-probe (on its webhook)
       ◀── answers, then closed ────

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

## Jobs: the inbox, nothing to approve

`burrow.proj.inbox` (proj-inbox, in `crates/burrow-proj`) turns GitHub issues
into project jobs.

- **Start:** open an issue in the project's own GitHub repo (one checked out in
  `burrow.projectsDir`) with the label `claude` (`inbox.label`), as one of
  `inbox.authors`. Put the whole brief in the body: the job knows only the
  issue. Within seconds it's a `proj run` job in that project, and the
  issue gets an "On it" comment naming the job.
- **Answer:** when the job finishes a turn, its answer is posted to the issue.
- **Follow-ups:** reply on the issue. Replies from `inbox.authors` go to the
  same job as its next turn (`proj reply`), with everything it already did in
  context. A reply sent while a turn runs waits for it to finish.
- **Done:** close the issue, or take the label off.
- **Who:** only `inbox.authors` can hand the box work; everyone else's issues
  and comments are ignored, labelled or not, except that a labelled issue
  someone else opened gets one comment saying no job will start. A job runs with everything the
  box's user can do.
- Jobs are headless (`claude -p`) and run in the box's normal permission mode.
  They can't stop to ask mid-turn: they end the turn with their question and
  pick up from your reply.
- On the box: `proj-inbox status` maps issues to jobs, `proj log ID` shows an
  answer.

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
  open issues labelled `claude` on GitHub.
- Remote Control: the chat's thread list; on the box, `tmux -L rc-NAME attach`
  shows a project's server and its sessions.

## Worktrees and untracked files

A worktree has the tracked files only. If a project needs untracked ones to
build (`.env`, a local config), list them in `.worktreeinclude` at the repo
root and Claude Code copies them into each new Remote Control worktree.
Dependencies (`node_modules`, `target/`) get rebuilt there; set
`burrow.remoteControl.sameDir = [ "NAME" ]` for a project where that's too
slow.
