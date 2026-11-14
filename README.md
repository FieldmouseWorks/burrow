# burrow

NixOS modules for a machine you run by talking to Claude. Claude Code gets the
whole box, with no permission prompts, because everything it can do is either
undone automatically or is one of the few one-way doors that still asks.

It's the generic half of a real box: one always-on "operator" Claude session
you talk to from your phone, which changes the system itself and hands project
work to Claude sessions inside each project.

MIT licensed. Status: in daily use on one machine; options may still move.

## Why

Agents write code faster than a person can approve it. On a machine set up
the usual way, the slow part is the person: clicking "allow", reading every
diff before it merges, babysitting every update. burrow moves the safety from
asking first to undoing after. A deleted file is a snapshot away, a system
change that breaks the box reverts itself, an update that fails its tests
never stays live, and a PR merges only once a reviewer and CI have passed the
exact commit. What can't be undone (wiping a disk, flashing firmware, deleting
remote data, force-pushing `main`) still asks.

## How it fits together

You talk to one Claude session, the operator, from the Claude app or
claude.ai. It changes the machine through the machine's own flake checkout,
and hands project work to Claude jobs inside each project (`proj`), or to a
microVM when a task should be fenced off (`agents`). Work lands as pull
requests: the steward (`autoland`) has Codex review each one, has Claude jobs
fix every finding and red check, and merges once the head's own change is
reviewed clean and the head is green. A merge to the machine's flake goes live through
`HOST-update`, behind the dead man's switch (`switch`) and a health check, and
the same pipeline keeps the flake's inputs, Claude Code and every repo's
dependencies current (`updates`). Work starts within seconds of a GitHub
webhook or an event on the box (`events`); the watchdog and the dispatcher
(`watchdog`, `dispatch`) notice what has stopped moving, and you hear only
about what needs you, one issue per problem (`notify`).

The modules, by job:

- **The safety net:** `claude`, `undo`, `switch`, `backup`, `ops`,
  `heartbeat`, `robot`
- **Doing the work:** `remote-control`, `proj`, `probe`, `agents`, `browser`,
  `dashboard`, `toolbox`, `rust-cache`, `cpu`, `github`, `usage-reset`
- **Landing it:** `autoland`, `events`, `watchdog`, `dispatch`, `ledger`
- **Keeping it current:** `updates`, `house`, `notify`

Each has an entry below, and each can be imported on its own.

## What's in it

- **`claude`** (`burrow.claude`): Claude Code system-wide in
  `bypassPermissions`, set in `/etc/claude-code/managed-settings.json` so no
  project can override it. The only prompts are `permissions.ask` rules for
  one-way doors (R2: destroying pools, wiping disks, flashing firmware,
  deleting remote data, force-pushing `main`). A PreToolUse guard snapshots
  before anything deletes files (R1) and asks before force-pushing a default
  branch; `proj` makes the same guard Codex's managed hook for its Codex jobs
  (`/etc/codex/requirements.toml`), on their commands and apply_patch, refusing
  where it would ask. Also writes the machine's `/etc/claude-code/CLAUDE.md` from what the
  other modules contribute.
- **`undo`** (`burrow.undo`): The guard's `@pre-*` ZFS snapshots: lets the
  user take them without sudo, prunes them after a few days.
- **`backup`** (`burrow.backup`): `HOST-backup`: each new snapshot of the
  datasets you name, off the box minutes after it's taken (when the units in
  `after`, sanoid say, finish; daily regardless). Restic through rclone, so to
  Google Drive, S3, another box or anything else rclone reaches: deduplicated,
  zstd-compressed, and encrypted with a password the box and you hold. It
  reads the snapshot itself at the datasets' own paths, so a run reads and
  sends only what changed, and holds that snapshot until the next one is off
  the box. Daily it forgets what `keep` doesn't keep, reads 1/`verifyDays` of
  the repository back, checks the newest backup holds everything its snapshot
  had, and restores the flake's checkout to scratch to compare with that
  snapshot, byte for byte (`HOST-backup drill PATH` for anything else).
  Failures go to a Claude job, and to you once the newest copy off the box is
  `staleAfter` hours old.
- **`switch`** (`burrow.switch`): `HOST-switch` / `HOST-confirm`: a dead man's
  switch for system changes. The new system goes live but isn't the boot
  default; unless confirmed within 10 minutes the old one comes back by
  itself. So a change that cuts off SSH, the network or Claude undoes itself.
  An activation that hangs is killed before the window expires to let the
  revert run. Enabled services must have bounded stop timeouts (including the
  manager default and `TimeoutSec`): the module rejects zero, infinity and
  time spans it cannot establish as positive integer seconds, minutes, hours
  or days within systemd's microsecond range. Boolean and list values are
  rejected. Abort failure modes also require a bounded effective
  `TimeoutAbortSec`, inheriting the manager default or falling back to the
  stop timeout when empty. This also covers stopping a start that ignores
  SIGTERM. Finite
  stops can still delay the revert; existing generations and hand-written
  unit files need the same discipline. `--abort` reverts at once; with `flakeInputs` it refuses a system
  built from an older nixpkgs. With `trialBoot` (GRUB), `HOST-boot` boots a
  new kernel, initrd or command line once when idle and makes it the
  default only after it came up; one that panics or hangs resets into the last
  good one and is never tried again. Trials wait for project jobs, sandboxes,
  runner jobs, updates and steward passes. Completion events retry promptly
  and always wait for work; `trialBoot.at` is the daily backstop, and only it
  overrides busy work, once a trial has waited two days. A kernel that failed
  is unqueued, never trialled.
  An unconfirmed switch always blocks a trial.
- **`updates`** (`burrow.updates`): Everything current, unattended.
  `HOST-update` (on each push to the flake's main or a watched input, and
  hourly for nixpkgs and Claude Code) bumps the flake's inputs and Claude
  Code, builds, runs your VM tests, switches with the dead man's switch,
  compares health checks before and after (`HOST-health`), confirms and pushes
  to main (as a PR the merge train lands, when the flake's repo has its train
  on); anything that breaks reverts and goes to a Claude job. It deploys main
  too, so a merged PR goes live the same way. The `proj-config` health check
  runs `proj check-config` as `burrow.user`, validating the deployed config
  and the role settings every turn reads before confirmation. Failed
  checks include stderr diagnostics in the rollback notice. Plus Renovate for every repo you
  own (`renovate`, merged on green CI, stuck PRs handed to Claude; a repo's
  push to its default branch and its Renovate PRs' CI start both for that
  repo, and all of them run hourly), `advisory-watch` (CISA KEV, vulnix,
  osv-scanner; new CVEs become Claude jobs the same hour), `firmware-check`
  (offers go to you: flashing is yours) and daily `rustup update`.
- **`notify`** (`burrow.notify`): `HOST-notify`: what the owner must know, as
  one GitHub issue per problem in `burrow.notify.repo`, commented at most
  daily and closed when it clears.
- **`remote-control`** (`burrow.remoteControl`): An always-on `claude
  remote-control` server for every project in `burrow.projectsDir` (a git
  checkout with an origin), found by itself (a path unit rescans when folders
  come and go) and trusted by itself, so none waits on a prompt. Sessions
  started from claude.ai each get their own git worktree. The machine's own
  flake checkout is one of them: that session is the operator. The servers
  keep memory back when the machine runs short (`memoryLow`, 10% of it), and
  each server and its sessions share a backstop for a runaway (`memoryMax`,
  half the machine, or `servers.NAME.memoryMax`): past it the kernel kills the
  biggest process there, the session that grew or a build it ran, and nothing
  else.
- **`proj`** (`burrow.proj`): `proj run PROJECT "task"`: the operator hands
  work to a headless agent inside that project, as a systemd unit, and checks
  on it later (`status`, `log`, `reply`, `stop`; `proj state --json` is the
  live snapshot those print from: the ready queue, every slot and what holds
  it, the meters and holds, its schema in `crates/burrow-proj/state.schema.json`). At most `maxJobs` Claude turns run at
  once, `maxJobsPerProject` from one project, and the rest queue (`proj status
  --line` shows the whole line, next first, with each turn's wait; `--first`
  goes ahead, and `--pipeline` jobs, the steward's and renovate-sweep's and
  issues in `autoland.pipelineRepos`, go ahead of everything and have
  `pipelineSlots` slots of their own in those repos' projects,
  `autoland.leadRepos`' first of all, and go ahead of the line in any other,
  within their project's `maxJobsPerProject`, whose last slot is kept for
  them, though they leave everyone else one slot while one of theirs waits and
  none runs; an `--unstick` job, the one that takes what the pipeline is stuck
  on, waits in no line at all, in a slot of its own past `maxJobs`); every job
  and every Remote Control server lives in `claude.slice`, which has no
  ceiling of its own: the jobs are what the machine gives up first when it
  runs short (the kernel kills them first, and systemd-oomd watches their
  slice), and each may have a cap of its own (`jobMemoryMax`, none by
  default). With `inbox.enable`, an issue labelled `claude` or `codex` in a project's
  GitHub repo, opened by one of `inbox.authors`, becomes a job too: the answer
  comes back as a comment and a reply there continues it; `proj done` closes its issue.
  Automated admission and runtime pauses use [the settings source](docs/settings.md).
  With `inbox.triage`, every other issue the
  authors open is sorted by a job of its own (`ready`, `later`, `tracker`, the
  owner's call, or closed as a duplicate or done), and `triage.drain` hands
  `ready` ones to an engine's idle slots ([front door](docs/front-door.md#triage-and-the-drain-issues-nobody-labelled)).
  Triage and the drain leave same-repo issue references in a queued, running
  or awaiting inbox job's handed brief alone until that job ends. One that also
  has `inbox.unstickLabel` (the steward's stuck issue) is run `--unstick`. Every
  turn says what comes next before it answers: `proj await` a PR or issue (the
  job holds no slot and is resumed when it changes; one already merged or
  closed is refused, so the job goes on), `proj ask --default "TEXT" [--one-way]`
  a person, or `proj done`. A bare ask is refused. The recommended answer runs
  after `proj.askTimeout` (seconds, default 6h) without a reply; a one-way
  recommendation instead ends the job, frees its claim and leaves the issue
  open under the owner label. Machine-started questions carry the owner's
  waiting label and appear in the pipeline's owner issue. Jobs first check
  machine facts with `proj probe QUESTION...`, using the host's probe and
  scrubber even inside a sandbox, without credentials or a probe issue.
  A turn that won't declare what comes next is told once and answers again,
  and still silent is a stall. Only jobs whose starter acts on each turn's end
  (`--owned`: the steward's, the watchdog's) needn't say. A run Claude Code
  refuses because the plan's usage is spent (a weekly or five-hour allowance,
  or extra-usage cap) holds Claude's queue until the limit
  resets: no Claude turn takes a slot till then, the refused turn waits in line and
  runs again by itself after, `proj status` says why and until when, and
  `HOST-notify` tells you once. With paid extra usage off
  (`burrow.proj.usage.paid`, in the [settings reference](docs/settings-reference.md)),
  limits mean waiting for reset, never asking the owner to raise a cap. A fresh engine meter requested after the latest refusal
  releases an ordinary usage hold and closes its notice when every window is below
  100% (or credits are available). Missing, stale or ambiguously ordered readings
  keep the hold. `proj usage back` also releases it once usage is available.
  `proj usage hold UNTIL [--reason TEXT]` starts a fresh owner pause until a
  future Unix timestamp, even after a previous release. Successful turns and
  fresh meters cannot release an owner pause; `proj usage back` can.
  A plain budget hold lets running turns continue and keeps Codex eligible.
  Add `--only pipeline` to vacate and park every excluded repository across
  both engines, reviews and proofs; [runtime settings](docs/settings.md) own
  the allow list and independent off switch.
  `burrow.proj.inbox.planning` defaults on: a fresh Codex
  planner sizes each inbox task before implementation. Large tasks become
  ordered child issues, one outstanding child per parent, with a checklist
  updated when each child's PR lands. Durable creation receipts admit
  generated children without widening `inbox.authors`. A worker can
  use `proj split FILE` when its task needs another split. See
  [task planning](docs/task-planning.md) for the state and retry protocol.
  After a reset, `burrow.proj.usageTrickle` caps concurrent Claude
  turns while the pre-reset queue drains, including pipeline turns. The emergency
  `--unstick` slot and Codex remain independent; ordinary concurrency returns
  once no pre-reset turn remains queued.
  Each role's engine (`claude` or `codex`, with an overflow to the other
  when its own is short), model and effort are live settings
  ([settings](docs/settings.md#engines-models-and-efforts-per-role)).
  Machine starters always name a role: `flaky`, `dependency-dashboard`,
  `baseline`, `stuck`, `advisory`, `backup`, `firmware`, `update`,
  `red-main`, `renovate-sweep`, `watchdog`, `triage`, `planner`, and the
  steward's `steward-handed`, `steward-redFixed`, `steward-failFixed`,
  `steward-conflictFixed`, `steward-proved`, and `steward-judged` roles. Inbox issues use `stuck` when they
  have `inbox.unstickLabel`; other bot issues use `machine-inbox`. Machines map
  their own labels to roles with `burrow.proj.inbox.roles` (for example
  career drift), including machines without an App; the unstick label
  wins over that map. Engine labels enroll
  these issues but their roles choose
  the engine each turn, at the declared default when no setting matches.
  The authors' own issues run as `burrow.proj.inbox.ownerRole` when it's
  set, so the settings can tune them; their engine label still picks
  the engine.
  A machine unit invoking `proj run` must supply `--role`, including units
  declared by the machine itself. Remote Control sessions and existing
  proj jobs retain the roleless form for work an agent hands on.
  Both engines consume the
  [job capability contract](crates/burrow-proj/src/engine/capabilities.rs): writable
  project and scratch, job state (`proj await`, `ask`, `done`), host facts (`proj probe`),
  git metadata,
  toolchains, system paths and meters, GitHub as the assigned App, heavy slots,
  Nix builds through the daemon, `/dev/kvm`, the machine's MCP servers
  (`burrow.mcpServers`, including each server's socket and output paths), and
  the network and login/App environment. The [parity check](tests/proj-capabilities.nix) runs the same
  capability probes inside a job on each engine and removes a Codex grant to
  prove a mismatch fails. Codex uses its own slot pool (`codex.slots`)
  and command sandbox; log it in with `autoland-codex-login job`, separately
  from `autoland-codex-login review`. A turn on a different engine receives
  the preceding turns' questions and answers.
  The deliberate authorship exception: a Codex-written head is judged by
  Claude, so its author never grades its own head.
  Local jobs inspect and recover other host jobs with the ordinary
  `proj status`, `state`, `lifecycle`, `log`, `find`, `list`, `reply`, `stop`, `withdraw` and `handoff`
  commands through a per-turn host socket. The broker keeps host paths,
  configuration and credentials outside the command sandbox, validates the
  recovery arguments and scrubs both output streams of what it sends back;
  the host's own commands, and the answers it stores, posts and hands on,
  are never redacted. A recovery reply retains
  open owner questions. Missing host access is an explicit error, rather than
  a claim that another job has no history. VM jobs have only the read-only
  probe mailbox and local lifecycle declarations.
  The [front door](docs/front-door.md#jobs-the-inbox-nothing-to-approve)
  says which label fits which work.
  Retained retries follow the deployed scheduler too;
  [the deployment protocol](docs/proj-deploy.md) keeps their turn and legacy
  usage records intact.

  Saved engine setups are named sets of those settings: `proj engines save
  NAME`, `proj engines use NAME` and `use back` check and switch them;
  `proj engines` lists them. `proj check-config` reads them as a turn does
  and must pass the update's health check before confirmation.
  With `engines.door.enable`, an author's `engines` issue in `engines.door.repo`
  switches setups without an agent ([engines door](docs/front-door.md#engines-with-no-agent)).

  `proj settings show [--json]` prints every pipeline option in effect, its
  value and where it came from (default, flake or live file), each declared
  once in burrow-settings' schema ([settings](docs/settings.md)). The
  [settings reference](docs/settings-reference.md) is written from that
  schema (`burrow-settings reference`, and a test fails when it's stale), and
  `burrow-guidance` fails the gate when other Markdown gives an option's
  value or file rather than linking it (its table of owned facts:
  crates/burrow-guidance).

- **`probe`** (`burrow.probe`): `burrow-probe`: read-only questions about the
  box, asked as an issue labelled `probe` in a private repo
  (`burrow.probe.repo`) and answered there in seconds, with no Claude:
  `memory`, `units`, `journal UNIT SINCE UNTIL`, `rc`, `queue`, `deployed`,
  `oom [SINCE]`, one a line, above any `---` rule (a signature under it is
  left be). No shell, journals only of the units on its allowlist, every
  answer through a secret scrubber before it's posted, and only `authors` get
  answers. `burrow-probe ask QUESTION` answers one on the box itself.
- **`events`** (`burrow.events`): `burrow-events`: what happens on GitHub, or
  on the box, starts the work it calls for within seconds; nothing waits for a
  timer. Each repo's webhook (kept by autoland, on its own repos and on the
  projects' in `projectsDir`) posts to `https://HOST/PATH`, and every delivery
  GitHub signed starts the pass of each handler that takes its event, for that
  repo: the steward's, `proj-watch`'s, `proj-inbox`'s, `HOST-update`'s,
  Renovate's and `renovate-sweep`'s. A handler can take only some repos, only
  pushes to the default branch, only the other events about some branches, and
  only the issue events about issues with some titles; one told issues has an
  issue's event's pass look at that issue alone (`issueNumbers`, the inbox's),
  rather than list every one. A repo's pass runs beside its handler's full
  pass, never folded into it (the steward's full pass yields after a slice),
  and a handler can yield to another (`yieldTo`, the steward's
  continuations), starting nothing while that one is busy. A turn ending, a review
  coming back and the passes' own wake-ups (a settle delay, a reviewer's
  timeout) kick them locally, and so does a switch that changes a handler's
  units. The one timer is an hourly catch-up, for deliveries GitHub never
  resends and repos too new to have a hook.
- **`autoland`** (`burrow.autoland`): The PR steward for every repo you own:
  Codex reviews each ready PR, Claude jobs settle every finding, red check and
  conflict with its base (what a job says it did goes up only once its push is
  the PR's head, and one whose PR someone else pushes to stands down), and
  once Codex finds nothing in the head's own change and CI is green on the
  head it merges and deletes the branch. A PR pushed again keeps its place: its reviews and
  Claude jobs wait in line by when it was opened, not when its latest head
  came. A repo can land through
  [Conary](https://github.com/FieldmouseWorks/Conary)'s merge train instead
  (`train`, on `burrow.ledger`): settled PRs are merged onto the default
  branch together, and land once CI is green on that, a red batch split to
  find the PR that broke it. One-way paths get a Claude judge; only what can't
  be undone waits for you, in one issue, and what the pipeline itself is stuck
  on goes to a Claude job through proj's inbox. Watches each repo's Renovate
  dashboard too: an update stuck there without a PR (waiting days, or
  erroring) goes to a Claude job, and to that inbox if it's still there a day
  later. So does each flaky test a gate quarantines
  ([docs/flaky-tests.md](docs/flaky-tests.md)): a Claude job fixes it, which
  lifts the quarantine. Keeps every repo at
  [docs/repo-baseline.md](docs/repo-baseline.md). Repos in `shadow` have
  Conary's landing flow, the steward's replacement, decide beside each pass
  without acting (`cn flow compare`, in `autoland-shadow@`'s journal after
  each pass), showing where the two part before a repo switches over.
- **`watchdog`** (`burrow.watchdog`): `burrow-watchdog`: work that has stopped
  moving gets a look before anyone has to notice it. Each source keeps a file
  of what it waits on now and by when that should be over: the steward, every
  open PR's wait as its status says it (`stallAfterMinutes`, longer for a
  hold), due at once when one of its steps has failed three times in a row;
  proj, every job's turn waiting in line (`proj.stallAfterMinutes`), running
  past its timeout, awaiting GitHub past `awaitHours`, or ended without
  saying what comes next or in a failure (at once). Past a deadline it kicks that item's pass, then hands
  every stall that's ready to one Claude job ahead of proj's line, then puts
  what's still stuck on the steward's stuck issue, whose own `--unstick` job
  takes it from there. A job of its own that's still waiting for a slot once
  none of its stalls need it (they cleared, or went on to the issue) is
  withdrawn (`proj withdraw`, which ends a job only while its turn waits for a
  slot, so one that has started is never stopped), so it never takes a slot
  for nothing. You hear only of what that issue can't take: the steward itself
  stuck, no steward, or that issue's own job stuck. A job that finds one
  legitimately slow gives it more time, once, with a reason. While proj says
  the machine is out of Claude usage, it holds what only a Claude can move (a
  turn in proj's line, a wait its source says is on a Claude job) and not
  already on the issue or yours, and takes each up again from its kick after
  the reset. Anything else that stalls meanwhile (a source stuck or never
  written, a turn past its deadline) still gets its kick, and since no Claude
  can take it, you hear of it by the deadline a job's would have had. It
  books its own next pass for the next deadline: no timer,
  and no GitHub calls of its own. `burrow-watchdog status` shows what it
  watches. With `units.enable` (on with the watchdog), what's broken on the
  machine itself is a source too (`burrow-units`): a failed unit, and a
  program that dumped core, whoever ran it, is a stall `units.graceMinutes`
  after it's seen, and a crash counts for `units.crashHours`; `units.ignore`
  takes globs of the units and programs that fail by design. A watcher
  follows the journal for systemd's word of a failure and systemd-coredump's
  of a dump and asks for a pass, touching no unit's own settings. The
  watchdog's job clears a crash whose cause it fixed with
  `burrow-units clear EXE`, and a later one is back. Clear uses a host
  acknowledgement socket, so sandboxed jobs need no host state or journal
  access: the host holds the units lock, records the clearance and requests
  a pass. Only executable names cross the socket. With
  `degradations.enable`, repeated soft CI failures feed the same ladder:
  [workflow degradations](docs/degradations.md) describes the signal and options.
- **`dispatch`** (`burrow.dispatch`): `burrow-dispatch`: what every PR waits
  on together, the machine's Actions runners and its deploy, keeps moving. A
  runner (`runners`, by unit, with the repo and labels it takes jobs for) that
  sits idle while a job only it could take is queued is restarted after
  `quietMinutes`, or started at once if it failed: at most twice in six hours,
  and never while a switch is in flight. A push run on a runner repo's default
  branch that hasn't started is cancelled once a later run of its workflow on
  that branch is live, so the later one takes its place (a run that has
  started finishes, so the branch always gets a result; this takes Actions:
  read and write on the machinery's App). With `fresh.enable`, each job queued
  in `fresh.repos` that asks for `fresh.label` and nothing else gets a runner
  of its own: registered for that one job with the organization `fresh.owner`
  (GitHub's JIT config, which takes the organization's Self-hosted runners
  permission, read and write, on the machinery's App) and started as
  `burrow-fresh-runner@INSTANCE`, a unit whose user and state directory
  nothing had before and nothing keeps after, at most `fresh.most` at once;
  one left idle for `quietMinutes` is stopped. A job that has to be its own
  evidence runs there: on a runner that takes job after job, a job before it
  could have asked for its OIDC token in its name. Each repo `HOST-update`
  watches has the commits the running system wasn't built from counted: its
  inputs' as `flake.lock` pins them, and the flake's own past the commit of
  main `HOST-update` last found in the system that runs (its `live.json`; the
  system's stamp can lag, since `HOST-update` doesn't switch to a system that
  differs only in the commit it's stamped with). Both are stall watchdog
  sources, so a runner still idle after that, a commit not live after
  `deploy.afterMinutes`, or a run of `HOST-update` quiet in its logs past
  `deploy.runMinutes`,
  goes up the watchdog's ladder like any other stall. Its passes start on the
  runner repos' job events, the watched repos' pushes, `HOST-update` starting
  and ending, and its own wake-ups, at most one every two minutes. It never
  merges, closes or changes code. `burrow-dispatch status` shows what it sees
  and the moves it has made.
- **`ledger`** (`burrow.ledger`):
  [Conary](https://github.com/FieldmouseWorks/Conary)'s ledger of every PR on
  your repos: who made each change and why, its revisions, the checks and
  reviews on them, and where it landed. Each repo's PR events start its `cn
  sync`, the hourly catch-up syncs them all. It reads GitHub and writes only
  the ledger's anchor, a ref in `anchorRepo` that holds the log's head, and
  each repo's notes (`refs/notes/conary`), with session links only for the
  repos `sessionLinks` names. `cn why FILE:LINE` answers from the ledger here
  and from the notes in any other clone, and `cn verify` checks the ledger
  against the anchor. With `daemon`, conaryd serves the ledger's calls on
  `/run/conary/conaryd.sock`, read-only, as a user of its own, with no
  network, and every Claude of `burrow.user` gets them, and `why`, as the
  `conary` MCP server. With `door`, an issue labelled `undo` in the probe's
  repo asks for a `cn undo` with no Claude in between
  ([front door](docs/front-door.md#undo-with-no-claude)). `policy` is
  Conary's class policy, which the steward asks for every PR it settles
  (`cn class`): which paths make a change R2 (CODEOWNERS and secrets by
  default), which are documentation (a change to them alone gets no review
  round, unless it's R2), and which repos go live when they land.
- **`agents`** (`burrow.agents`): `agent new PROJECT`: a microVM per task
  (microvm.nix, QEMU) on a ZFS clone of the data dataset, Claude with
  permissions bypassed inside, egress only through an allowlist proxy, no
  GitHub token; the work comes back with `agent fetch` as `refs/agents/ID/*`.
  Idle ones stop after a day and are saved and deleted after a week. Opt-in
  per task.
- **`browser`** (`burrow.browser`): A Chromium that stays up on the machine,
  signed in to your accounts by you, that job engines drive through
  Playwright's MCP server (`browser`, in the [shared registry](modules/mcp.nix)), which
  saves what an agent asks of it (a screenshot, a page's text) in the user's
  runtime dir, in RAM, outside the checkout. You watch it
  and sign in through noVNC on `viewer.interfaces` (a tailnet's, say), and
  nowhere else even where the firewall trusts an interface: no password, so
  only name interfaces that reach only you, and its WebSocket takes only pages
  from `viewer.origins` and its pages refuse to be framed, so another site
  open in your browser can't drive it or steer your clicks into it. Its
  profile holds your logins, which outlive a restart or reboot (it restores
  its last session, so cookies without an expiry date stay too, and it opens
  the same Chromium profile every time, `profile`, so a person added in the
  viewer doesn't leave it on the profile picker): its own user, mode 0700,
  outside the data dataset the sandboxes clone; persist
  `/var/lib/burrow-browser` if `/` is ephemeral, and keep it out of anything
  that leaves the machine unencrypted. Its DevTools port and viewer refuse
  every local user but `burrow.user` (an nftables table of its own under
  either firewall, so no other rule's accept gets past it, replaced without a
  gap on reload and gone with the module; nothing listens while it's down), so
  neither the sandboxes' proxy nor the reviewers nor CI can drive it. The
  machine's CLAUDE.md tells Claude to go at a person's pace and to ask before
  anything that posts, sends, pays or deletes as you.
- **`dashboard`** (`burrow.dashboard`): The machine's own web app, for your
  devices, on the tailnet alone ([crates/burrow-dashboard](crates/burrow-dashboard)).
  It listens on localhost and `tailscale serve` puts it on the machine's
  ts.net name (`host`) over HTTPS: no public name, no open port, no ACME
  token (the tailnet needs HTTPS certificates on). You sign in with a
  passkey bound to that name, as the GitHub account `owner`; a device is added
  with a one-time code printed on the machine, `sudo burrow-dashboard
  enroll`, as many as you like. Sessions are a cookie (Secure, HttpOnly,
  SameSite=Strict) that ends after `idleTimeout` unused or `maxAge` at most.
  It installs as an app (Safari on iOS too), its service worker keeps only
  the shell, never data, and it says when it's offline. Its passkeys and
  sessions are in `burrow.stateDir`/dashboard.
- **`robot`** (`burrow.robot`): `robot status / rescue / reset / wol` for a
  Hetzner dedicated server, through the Robot webservice. The way back in when
  the box is down.
- **`heartbeat`** (`burrow.heartbeat`): Something off the box watches it:
  every 5 minutes it pings a [Healthchecks](https://healthchecks.io) check
  (healthchecks.io's free plan, or your own instance) with how it is. Success
  while every `HOST-health` check passes, a fail ping naming the check once
  one has failed two beats running, and when the pings stop (down, hung, cut
  off) Healthchecks alerts you by email or push after the check's grace.
  Nothing on the box has to be alive for that. `sites` has it watch other
  sites the same way: each URL answering 2xx pings a check of its own, and its
  silence is the alert.
- **`usage-reset`** (`burrow.usageReset`): `burrow-usage-reset`: the Claude
  plan's banked weekly reset gets used, once, when the week is nearly gone
  (under `below`, 2%, left), not left sitting while every Claude stops at the
  limit. It reads the weekly meter through Claude Code itself (its SDK's
  `get_usage`: no model call), counts an answer from Claude Code's cached copy
  as no reading at all, and past the line types `/limit-reset weekly` into a
  Claude Code of its own in a tmux and confirms it, at most once a week; a
  reset that only works at the limit waits for it, and with nothing banked it
  does nothing. `HOST-notify` tells you when it used one, or when it couldn't.
  It books its own next look, sooner the faster the week is going, and never
  more than 15 minutes on: no timer. Each pass also keeps the engines' meters
  for proj's role overflow: Claude's five-hour session and week, and, with
  proj's Codex, Codex's windows and whether it has credits (read through its
  app-server's `account/rateLimits/read`). Each fresh reading replaces
  `meters/ENGINE.json` and appends to `meters/log` in its state dir.

- **`ops`** (`burrow.ops`): perf, bpftrace, bcc, strace, gdb, tcpdump,
  nvme-cli, smartctl, fwupd and friends, a systemd hardware watchdog (with
  `watchdogIdentity`, the board's own one, whichever driver came up first, and
  a `HOST-health` check that it's the one armed), and systemd-oomd watching
  the whole machine: when swap is nearly full, or a slice is stalled on
  memory, it kills the unit that's drowning it (never sshd, tailscaled or a
  Remote Control server), long before the kernel's OOM killer would; and sshd,
  tailscaled, networkd, the journal and oomd itself keep memory back
  (`protect`), so you can still get in.
- **`cpu`** (`burrow.cpu`): How the CPU is shared between repos and runners.
  The slices that hold gates (`gates.slices`, CI runners say) get
  `gates.weight` (400) against the 100 of every Claude and of nix-daemon's
  builds, so a gate keeps its share whatever else runs. `heavy -- COMMAND`
  runs a heavy job (a gate, a VM test, a replay, a large build) in one of
  `heavy.slots` machine-wide slots, for every repo and account alike, waiting
  while all are busy, and tells it its share of the threads in `HEAVY_JOBS`
  (and `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`); `heavy --all -- COMMAND`
  takes every slot instead, for a run that must have the machine to itself
  (a long gate with a deadline, a nightly); `heavy status` says which are
  busy. A repo's own queue (the steward's proof slots, a repo's gate lock)
  still picks which of its jobs goes next.
- **`toolbox`** (`burrow.toolbox`): The dev tools projects and agents expect
  (gh, jj, rust, go, node, python, uv, qemu, nix-ld, direnv, `flaky`, which
  quarantines a gate's flaky tests:
  [docs/flaky-tests.md](docs/flaky-tests.md), and `flake-gate`, which runs a
  repo's CI gate here: the shared workflow's gate step, with the inputs the
  repo's `flake.yml` gives it). The sandboxes always get it.
- **`rust-cache`** (`burrow.rustCache`): Every Rust build on the machine
  shares a compile cache: cargo runs rustc through
  [sccache](https://github.com/mozilla/sccache), so a dependency compiled once
  comes out of the cache in the next fresh worktree or CI checkout. One cache
  per user, never shared, since a user's builds fill it with what that user's
  next build trusts: `burrow.user`'s, and those of the `users` you list (CI
  runners' users, say). Each user's cache has one daemon, `rust-cache-USER`,
  on a socket only that user can reach (sccache's default TCP port lets any
  local user compile, and so run code, as whoever started it). The daemon only
  stores: the compiles run in the builds, under their own units' limits
  (sccache's client-side mode), so a runner's sandbox needs only to read the
  cache. A build that asks the daemon to compile anyway is refused (sccache is
  patched for that) and compiles itself, and the daemon runs in a chroot
  holding only sccache and the cache, since a client can ask it for any path.
  Whatever keeps the cache from working compiles without it. The caches live
  in `/var/cache/sccache`: keep them out of snapshots and backups.
- **`house`** (`burrow.house`): `house-sync` (when the blocks change, and on
  each repo's pushes to its default branch): every repo's copies of the house
  blocks (the house rules, [house/rules.md](house/rules.md), and the Pull
  requests section, [house/prs.md](house/prs.md), each between its markers in
  AGENTS.md) kept the same as burrow's, as one PR per repo when a block
  changes. Broken markers, and repos it couldn't get through, go to the owner
  through `notify`; a pass GitHub's rate limit holds off is booked again for
  when it lifts, not a failure. See [docs/house-style.md](docs/house-style.md).
- **`github`** (`burrow.github`): Tokens of GitHub Apps of yours, for what on
  the machine should act as an App rather than as your own gh login. Each
  App's socket (`/run/gh-token/NAME.sock`, its `users` only) is answered by
  `gh-token` as a user of its own, with the App's key a credential only it can
  read: an installation token for the account or repo asked for, kept until it
  has fifty minutes left, so one lasts a job that takes it at the start. A
  process acts as the App with the App's `env`: `gh` then gets a token for the
  account its call names, and git's only credential helper gets one for the
  repo it pushes to, and neither can fall back to your login when the App's
  can't be had. LFS uses the same broker for GitHub HTTP requests, ignoring
  user headers, netrc, cookies and client certificates in the App's env.
  Object storage uses only the signed URLs and headers the batch API returns.
  Action URLs cannot change origin through rewrites or redirects. Custom
  transfer programs are refused in the App's environment.

## Assumptions

- ZFS, with the data you work on in one dataset (`burrow.agents.dataDataset`,
  default `rpool/safe/data` at `/data`) and projects under it
  (`burrow.projectsDir`, default `/data/dev/src`).
- One user for you and your agents (`burrow.user`, default `dev`) with a fixed
  uid and passwordless sudo. Undo is what keeps that safe, not prompts.
- The machine's own flake is checked out at `burrow.flake` (default
  `PROJECTS/HOSTNAME`), so the operator can edit and switch it.
- Rolling snapshots (sanoid or similar) of the datasets you care about are
  yours to set up; `undo` only covers the guard's, and `backup` sends yours
  off the box.

## Use

```nix
{
  inputs.burrow = {
    url = "git+https://github.com/FieldmouseWorks/burrow";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, burrow, ... }@inputs: {
    nixosConfigurations.mybox = nixpkgs.lib.nixosSystem {
      modules = [
        burrow.nixosModules.default
        ./configuration.nix
        {
          burrow = {
            owner = "Sam";
            claude.enable = true;
            undo = { enable = true; datasets = [ "rpool/safe/home" "rpool/safe/data" ]; };
            switch = { enable = true; flakeInputs = inputs; trialBoot.enable = true; };
            notify.repo = "sam/homelab";
            updates = {
              enable = true;
              origin = "https://github.com/sam/homelab";
              checks = [ "rehearsal" ];  # your flake's VM tests
            };
            remoteControl.enable = true;
            proj.enable = true;
            agents.enable = true;
            toolbox.enable = true;
            ops.enable = true;
          };
        }
      ];
    };
  };
}
```

`examples/minimal.nix` is a complete (if diskless) machine with everything on.
Each module is also importable on its own (`burrow.nixosModules.agents`, ...).

Things you'll want to set yourself: a Claude login for the sandboxes
(`burrow.agents.claudeTokenFile`, a `claude setup-token` token, for example
from sops-nix), extra allowed hosts (`burrow.agents.extraAllow`), a git
identity for the sandboxes (`burrow.agents.guestModules`), an intro for the
machine's CLAUDE.md (`burrow.claude.machineMd` with `lib.mkBefore`), MCP
servers of your own for every job engine and Claude session (`burrow.mcpServers`), and, to
run Claude Code's newest release instead of nixpkgs', a release manifest in
your flake (`burrow.claude.manifest`, and its path for `HOST-update` to bump
in `burrow.updates.claudeManifest`). Persist `burrow.stateDir` if `/` is
ephemeral. `HOST-update` pushes to `origin` with the user's git login. The
rest of the machine's own GitHub traffic (Renovate, notices, the ledger,
house-sync, and the Claude jobs it starts by itself) acts as a GitHub App of
yours when `burrow.github.machinery` names one, and as the user's gh login
otherwise; work the user asks for (an issue of theirs in the inbox, a session
of theirs) acts as them either way. The autoland steward acts as an App of its
own (`burrow.autoland.app`;
[docs/repo-baseline.md](docs/repo-baseline.md#the-stewards-github-app) says
how to make one).

After the first boot, run `claude remote-control` by hand once, anywhere, to
log in and enable Remote Control. Every project folder after that is trusted
and served on its own.

To run every project from one claude.ai project chat, see
[docs/front-door.md](docs/front-door.md).

## Checks

```sh
flake-gate                                    # the gate, as CI runs it (AGENTS.md)
nix build .#checks.x86_64-linux.agents -L     # boots sandboxes (needs /dev/kvm)
```

`checks.agents` (`tests/agents.nix`) boots a host VM that boots sandboxes inside
itself (nested KVM) and checks isolation, egress, fetch, `proj run --vm` and
cleanup.
