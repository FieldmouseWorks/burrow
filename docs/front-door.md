# One chat for every project

You talk to one claude.ai project chat. The work runs on the box, inside each
project, by that project's own Claude: its checkout, its CLAUDE.md, its tools
and logins. Nothing gets copied to the cloud and back.

## How it fits together

```
claude.ai project chat            the box
──────────────────────            ───────────────────────────────────────────
coordinator ── starts ──▶ thread ── Remote Control ──▶ claude-rc@PROJECT
                          (one per task)               └─ a session in its own
                          ◀── results, PRs, questions ──   worktree of PROJECT
```

- Every folder in `burrow.projectsDir` has an always-on `claude remote-control`
  server (`modules/remote-control.nix`). It trusts its folder by itself, so it
  never sits at a prompt nobody can see. Linked git worktrees and folders
  matching `burrow.remoteControl.skip` get none.
- A thread in the claude.ai project starts a Remote Control session on the
  project's server. That session is the project's Claude. With
  `burrow.remoteControl.spawn = "worktree"` (the default) it gets its own git
  worktree, so several threads can work in one project at once. The session
  that's always there in the folder itself stays in the main checkout for you.
- What you type in the thread reaches the session. Its results, PR links and
  questions come back to the thread. Follow-ups are just more messages in the
  same thread, to the same session, with everything it already did in context.
- Long work runs on the box after the thread goes quiet. The session reports
  when it's done.
- The machine's own flake folder is the operator. Machine work goes there.
  Its sessions stay in the folder itself, since that checkout is what
  `HOST-switch` builds.

## For the Claude in the chat (coordinator and threads)

- One project per thread. To start: find the folder in your device list
  (`PROJECTS/NAME` on the box) and start Remote Control there, with the task as
  the instructions. The first time a folder is reached from the chat, the owner
  gets one approval card for it; after that it goes through without asking.
- After it starts, don't relay every step. Put progress on the thread's
  checklist and tell the owner the result, the PR, or the question.
- A thread holds one Remote Control session at a time. Work in a second
  project goes in its own thread.
- What's running: the project's thread list. On the box,
  `tmux -L rc-NAME attach` shows a project's server and its sessions.

## proj, for work nobody's watching

`proj run PROJECT "task"` (`modules/proj.nix`) is still how the box hands
itself work: Renovate follow-ups, update failures, advisories. Those jobs are
headless (`claude -p`), report to `proj status`, and go to the owner through
`HOST-notify` when they need them. Use Remote Control for anything the owner
starts or wants to talk through.

## Worktrees and untracked files

A worktree has the tracked files only. If a project needs untracked ones to
build (`.env`, a local config), list them in `.worktreeinclude` at the repo
root and Claude Code copies them into each new worktree. Dependencies
(`node_modules`, `target/`) get rebuilt there; set
`burrow.remoteControl.sameDir = [ "NAME" ]` for a project where that's too slow.
