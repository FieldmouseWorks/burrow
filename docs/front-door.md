# One chat for every project

You talk to one claude.ai project chat. The work runs on the box, inside each
project, by a Claude in that project: its checkout, its CLAUDE.md, its tools
and logins. Nothing gets copied to the cloud and back.

A thread in that chat can hand a project work two ways. Use the first one
unless you need the second.

```
claude.ai project chat                   GitHub                  the box
──────────────────────                   ──────                  ──────────────────────
thread ── issue labelled `claude` ──▶ the project's repo ──▶ proj-inbox (on its webhook)
       ◀── answer as a comment ─────                     ──▶ proj run / proj reply

thread ── Remote Control (one tap) ───────────────────────────▶ claude-rc@PROJECT
       ◀── results, PRs, questions ────────────────────────────  (own worktree)
```

## 1. The inbox (default): nothing to approve

`burrow.proj.inbox` (`modules/proj/inbox.sh`) turns GitHub issues into project
jobs.

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
  and comments are ignored, labelled or not. A job runs with everything the
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

## 2. Remote Control: hands-on, one tap per thread

For work the owner wants to watch or steer live, a thread starts a Remote
Control session on the project's server (`modules/remote-control.nix`): the
project's Claude, in its own git worktree, in `burrow.remoteControl.permissionMode`
(bypassPermissions alongside `burrow.claude`, so only the one-way doors ask).
The owner's messages in the thread reach it directly. The catch: each thread
shows the owner an approval card first, and a box with no desktop app can't be
pre-approved.

- Every folder in `burrow.projectsDir` has an always-on `claude remote-control`
  server that trusts its folder by itself, so it never sits at a prompt nobody
  can see. Linked git worktrees and folders matching
  `burrow.remoteControl.skip` get none.
- With `burrow.remoteControl.spawn = "worktree"` (the default) each session
  started from claude.ai gets its own git worktree, so several threads can work
  in one project at once. A server holds no session until one is asked for
  (`burrow.remoteControl.idleSession`), and all of them, with the proj jobs,
  share the memory cap `burrow.claude.memory` sets. Each session, and each
  server itself, has a cap of its own too
  (`burrow.remoteControl.sessionMemoryMax`): a session that grows past it is
  killed alone, and the thread can pick it up again from its transcript
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
