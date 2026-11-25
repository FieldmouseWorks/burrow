# Modules

Detailed behavior and operational notes for each NixOS module in burrow. Each
module can be imported on its own (`burrow.nixosModules.<name>`) or together
via `burrow.nixosModules.default`. For the high-level overview and quick start,
see [README.md](../README.md).

## Engines

- **`anthropic`** (`modules/engines/anthropic.nix`, `burrow.claude`): Anthropic's
  Nix engine surface for both the host and the sandboxes. On the host, Claude
  Code runs system-wide in
  `bypassPermissions`, set in `/etc/claude-code/managed-settings.json` so no
  project can override it. The only prompts are `permissions.ask` rules for
  one-way doors (R2: destroying pools, wiping disks, flashing firmware,
  deleting remote data, force-pushing `main`). The standing-authority
  commands in `modules/engines/anthropic/standing.nix` (`proj reply`, `settings land`,
  taking a label off, `gh run rerun`) are `permissions.allow` rules, so a
  session whose mode puts Claude Code's auto-mode check in front of its
  commands doesn't have that check refuse them; the one-way doors still ask
  first. A PreToolUse guard snapshots
  before anything deletes files (R1) and asks before force-pushing a default
  branch. It refuses any branch's force-push with no lease on the head its
  pusher saw (`--force-with-lease --force-if-includes`, or one naming the
  commit), so a job can't drop what another pushed to the same PR. Also
  writes the machine's `/etc/claude-code/CLAUDE.md` from what the
  other modules contribute. In the sandboxes, wires both `claude` and
  `claude-api` through `burrow.agents.engines` with their packages,
  credentials, Anthropic egress hosts, and packaged `burrow-engine-anthropic`
  plugins (`agent-plugin-claude` and `agent-plugin-claude-api`).
- **`antigravity`** (`burrow.engines.antigravity`, `modules/engines/antigravity.nix`,
  `crates/burrow-engine-antigravity`): Isolated Antigravity engine surface and
  Conary `conary-plugin v5` `job` plugin. Wires the machine flake's
  Antigravity package or binary, isolated auth credential, runtime-configured
  models and efforts, egress host allowlist, and approved capabilities into
  `burrow.agents.engines.antigravity` without sharing provider code with
  Claude or touching `burrow-proj`.

## Safety net

- **`undo`** (`burrow.undo`): The guard's `@pre-*` ZFS snapshots: lets the
  user take them without sudo, prunes them after a few days.
- **`switch`** (`burrow.switch`): `HOST-switch` / `HOST-confirm`: a dead man's
  switch for system changes. The new system goes live but isn't the boot
  default; unless confirmed within 10 minutes the old one comes back by
  itself. So a change that cuts off SSH, the network or the operator agent
  undoes itself.
  An activation that hangs is killed before the window expires to let the
  revert run. One never waits on a job queued behind a run already going (a
  sweep ordered after a two-hour renovate): switch-to-configuration is
  patched to leave it queued, to run after that run
  ([held.rs](../patches/switch-to-configuration-ng/held.rs)). The setup
  sysinit-reactivation.target reruns is still waited for, as the rest counts
  on it. A user manager is re-executed only when it may need it (another
  systemd binary, or a config source changed since the re-execution last
  seen): a re-execution refuses every `systemctl --user` and
  `systemd-run --user` until it's done
  ([user_reexec.rs](../patches/switch-to-configuration-ng/user_reexec.rs)).
  Enabled services must have bounded stop timeouts (including the
  manager default and `TimeoutSec`): the module rejects zero, infinity and
  time spans it cannot establish as positive integer seconds, minutes, hours
  or days within systemd's microsecond range. Boolean and list values are
  rejected. Abort failure modes also require a bounded effective
  `TimeoutAbortSec`, inheriting the manager default or falling back to the
  stop timeout when empty. This also covers stopping a start that ignores
  SIGTERM. Finite
  stops can still delay the revert; existing generations and hand-written
  unit files need the same discipline. `--abort` reverts at once. `HOST-confirm SYSTEM` and `--abort SYSTEM` act only on that
  system, and exit 4 when another switch has replaced it, as `HOST-update` uses them. With `flakeInputs` it refuses a system
  built from an older nixpkgs. With `trialBoot` (GRUB), `HOST-boot` boots a
  new kernel, initrd or command line once when idle and makes it the
  default only after it came up; one that panics or hangs resets into the last
  good one and is never tried again. Trials wait for project jobs, sandboxes,
  runner jobs, updates and steward passes. Completion events retry promptly
  and always wait for work; `trialBoot.at` is the daily backstop, and only it
  overrides busy work, once a trial has waited two days. A kernel that failed
  is unqueued, never trialled.
  An unconfirmed switch always blocks a trial.
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
  Failures go to an agent job, and to you once the newest copy off the box is
  `staleAfter` hours old.
- **`ops`** (`burrow.ops`): perf, bpftrace, bcc, strace, gdb, tcpdump,
  nvme-cli, smartctl, fwupd and friends, a systemd hardware watchdog (with
  `watchdogIdentity`, the board's own one, whichever driver came up first, and
  a `HOST-health` check that it's the one armed), and systemd-oomd watching
  the whole machine: when swap is nearly full, or a slice is stalled on
  memory, it kills the unit that's drowning it (never sshd or tailscaled),
  long before the kernel's OOM killer would; and sshd,
  tailscaled, networkd, the journal and oomd itself keep memory back
  (`protect`), so you can still get in.
- **`heartbeat`** (`burrow.heartbeat`): Something off the box watches it:
  every 5 minutes it pings a [Healthchecks](https://healthchecks.io) check
  (healthchecks.io's free plan, or your own instance) with how it is. Success
  while every `HOST-health` check passes, a fail ping naming the check once
  one has failed two beats running, and when the pings stop (down, hung, cut
  off) Healthchecks alerts you by email or push after the check's grace.
  Nothing on the box has to be alive for that. `sites` has it watch other
  sites the same way: each URL answering 2xx pings a check of its own, and its
  silence is the alert.
- **`robot`** (`burrow.robot`): `robot status / rescue / reset / wol` for a
  Hetzner dedicated server, through the Robot webservice. The way back in when
  the box is down.

## Work and interfaces

- **`proj`** (`burrow.proj`): `proj run PROJECT "task"`: the operator hands
  work to a headless agent inside that project (or the repo's own clone, for
  `proj run OWNER/NAME "task"`), as a systemd unit, and checks
  on it later (`status`, `log`, `reply`, `stop`; `proj state --json` is the
  live snapshot those print from: the ready queue, every slot and what holds
  it, the meters and holds, its schema in `crates/burrow-proj/state.schema.json`). At most `maxJobs` turns run at
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
  lives in `claude.slice`, which has no
  ceiling of its own: the jobs are what the machine gives up first when it
  runs short (the kernel kills them first, and systemd-oomd watches their
  slice), and each may have a cap of its own (`jobMemoryMax`, none by
  default). With `inbox.enable`, an issue carrying a configured engine label
  (such as `claude`) in a project's
  GitHub repo, opened by one of `inbox.authors`, becomes a job too: the answer
  comes back as a comment and a reply there continues it; `proj done` closes its issue.
  Automated admission and runtime pauses use [the settings source](settings.md).
  With `inbox.triage`, every other issue the
  authors open is sorted by a job of its own (`ready`, `later`, `tracker`, the
  owner's call, or closed as a duplicate or done), and `triage.drain` hands
  `ready` ones to an engine's idle slots ([front door](front-door.md#triage-and-the-drain-issues-nobody-labelled)).
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
  (`--owned`: the steward's, the watchdog's) needn't say. A run an engine
  refuses because its usage is spent (a weekly or five-hour allowance,
  or extra-usage cap) holds that engine's queue until the limit
  resets: no turn on that engine takes a slot till then, the refused turn waits
  in line and runs again by itself after, `proj status` says why and until when, and
  `HOST-notify` tells you once. With paid extra usage off
  (`burrow.proj.usage.paid`, in the [settings reference](settings-reference.md)),
  limits mean waiting for reset, never asking the owner to raise a cap. A fresh engine meter requested after the latest refusal
  releases an ordinary usage hold and closes its notice when every window is below
  100% (or credits are available). Missing, stale or ambiguously ordered readings
  keep the hold. `proj usage back` also releases it once usage is available.
  `proj usage hold UNTIL [--reason TEXT]` starts a fresh owner pause until a
  future Unix timestamp, even after a previous release. Successful turns and
  fresh meters cannot release an owner pause; `proj usage back` can.
  A plain budget hold lets running turns continue.
  Add `--only pipeline` to vacate and park every excluded repository's jobs
  and proofs; [runtime settings](settings.md) own
  the allow list and independent off switch.
  `burrow.proj.inbox.planning` defaults on: a fresh
  planner sizes each inbox task before implementation. Large tasks become
  ordered child issues, one outstanding child per parent, with a checklist
  updated when each child's PR lands. Durable creation receipts admit
  generated children without widening `inbox.authors`. A worker can
  use `proj split FILE` when its task needs another split. See
  [task planning](task-planning.md) for the state and retry protocol.
  After a reset, `burrow.proj.usageTrickle` caps concurrent turns on that
  engine while the pre-reset queue drains, including pipeline turns. The emergency
  `--unstick` slot stays independent; ordinary concurrency returns
  once no pre-reset turn remains queued.
  Each role's model and effort are live settings
  ([settings](settings.md#engines-models-and-efforts-per-role)).
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
  set, so that role chooses the engine and tuning each turn while engine
  labels only enroll them; leaving `ownerRole` unset keeps label routing.
  A machine unit invoking `proj run` must supply `--role`, including units
  declared by the machine itself. Interactive shells and existing
  proj jobs retain the roleless form for work an agent hands on.
  Jobs consume the
  [job capability contract](../crates/burrow-proj/src/engine/capabilities.rs): writable
  project and scratch, job state (`proj await`, `ask`, `done`), host facts (`proj probe`),
  git metadata,
  toolchains, system paths and meters, GitHub as the assigned App, heavy slots,
  Nix builds through the daemon, `/dev/kvm`, the machine's MCP servers
  (`burrow.mcpServers`, including each server's socket and output paths), and
  the network and login/App environment. Anthropic-specific CLI flags, stream
  JSONL parsing, usage/login refusal classification and `claude-api` credit
  admission live in
  [crates/burrow-engine-anthropic](../crates/burrow-engine-anthropic). An engine plugin is granted only
  [part of that contract](engines.md#a-plugins-grants). A turn whose conversation is lost
  receives the preceding turns' questions and answers.
  Local jobs inspect and recover other host jobs with the ordinary
  `proj status`, `state`, `lifecycle`, `log`, `find`, `list`, `reply`, `stop` and `withdraw`
  commands through a per-turn host socket. The broker keeps host paths,
  configuration and credentials outside the command sandbox, validates the
  recovery arguments and scrubs both output streams of what it sends back;
  the host's own commands, and the answers it stores, posts and hands on,
  are never redacted. A recovery reply retains
  open owner questions. Missing host access is an explicit error, rather than
  a claim that another job has no history. VM jobs have only the read-only
  probe mailbox and local lifecycle declarations.
  The [front door](front-door.md#jobs-the-inbox-nothing-to-approve)
  says which label fits which work.
  Retained retries follow the deployed scheduler too;
  [the deployment protocol](proj-deploy.md) keeps their turn and legacy
  usage records intact.

  Saved engine setups are named sets of those settings: `proj engines save
  NAME`, `proj engines use NAME` and `use back` check and switch them;
  `proj engines` lists them. `proj check-config` reads them as a turn does
  and must pass the update's health check before confirmation.
  With `engines.door.enable`, an author's `engines` issue in `engines.door.repo`
  switches setups without an agent ([engines door](front-door.md#engines-with-no-agent)).

  `proj settings show [--json]` prints every pipeline option in effect, its
  value and where it came from (default, flake or live file), each declared
  once in burrow-settings' schema ([settings](settings.md)). The
  [settings reference](settings-reference.md) is written from that
  schema (`burrow-settings reference`, and a test fails when it's stale), and
  `burrow-guidance` fails the gate when other Markdown gives an option's
  value or file rather than linking it (its table of owned facts:
  `crates/burrow-guidance`).
- **`agents`** (`burrow.agents`): `agent new PROJECT [--engine NAME]`: a
  microVM per task (microvm.nix, QEMU) on a ZFS clone of the data dataset,
  its engine with nothing to ask inside, egress only through an allowlist
  proxy, no GitHub token; the work comes back with `agent fetch` as
  `refs/agents/ID/*`. Each engine is an entry of `burrow.agents.engines`
  (Anthropic's `claude` and `claude-api` entries are declared by the
  `anthropic` module): its guest packages and
  modules, its login (`credentialFile`, handed in as the systemd credential
  `agent.credential`), its own egress hosts on its own filtered proxy, and
  the capabilities and MCP servers the owner approved for it
  ([a plugin's grants](engines.md#a-plugins-grants)). A VM for one engine gets none
  of another's. An engine's `plugin` runs in
  [a VM of its own per call](engines.md#a-plugins-sandbox), as
  `agent-plugin-NAME`, with its grants and nothing else, and passes
  [its conformance check](engines.md#a-plugins-conformance-check)
  (`lib.engineConformance`) there. `agent fetch` runs
  git confined to the sandbox's clone and the project's repository, so a
  link planted in a clone reaches nothing else. Idle ones stop after a day
  and are saved and deleted after a week. Opt-in per task.
- **`probe`** (`burrow.probe`): `burrow-probe`: read-only questions about the
  box, asked as an issue labelled `probe` in a private repo
  (`burrow.probe.repo`) and answered there in seconds, with no agent:
  `memory`, `units`, `journal UNIT SINCE UNTIL`, `rc`, `queue`, `why OWNER/REPO#N`, `deployed`,
  `oom [SINCE]`, one a line, above any `---` rule (a signature under it is
  left be). No shell, journals only of the units on its allowlist, every
  answer through a secret scrubber before it's posted, and only `authors` get
  answers. `burrow-probe ask QUESTION` answers one on the box itself.
- **`dashboard`** (`burrow.dashboard`): The machine's own web app, for your
  devices, on the tailnet alone ([crates/burrow-dashboard](../crates/burrow-dashboard)).
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
- **`browser`** (`burrow.browser`): A Chromium that stays up on the machine,
  signed in to your accounts by you, that job engines drive through
  Playwright's MCP server (`browser`, in the [shared registry](../modules/mcp.nix)), which
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
  machine's instructions tell agents to go at a person's pace and to ask before
  anything that posts, sends, pays or deletes as you.
- **`settings`** (`burrow.settings`): Installs `burrow-settings`, publishes the
  deployed effective settings schema alongside the live overlay, and optionally
  enables the independent GitHub settings issue door (`burrow.settings.door`)
  so operational changes work even when the pipeline and event dispatcher are
  down. See [settings](settings.md) and the
  [settings reference](settings-reference.md).
- **`toolbox`** (`burrow.toolbox`): The dev tools projects and agents expect
  (gh, jj, rust, go, node, python, uv, qemu, nix-ld, direnv, `flaky`, which
  quarantines a gate's flaky tests:
  [docs/flaky-tests.md](flaky-tests.md), and `flake-gate`, which runs a
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
- **`cpu`** (`burrow.cpu`): How the CPU is shared between repos and runners.
  The slices that hold gates (`gates.slices`, CI runners say) get
  `gates.weight` (400) against the 100 of agent jobs and of nix-daemon's
  builds, so a gate keeps its share whatever else runs. `heavy -- COMMAND`
  runs a heavy job (a gate, a VM test, a replay, a large build) in one of
  `heavy.slots` machine-wide slots, for every repo and account alike, waiting
  in line while all are busy (the longest waiter gets the next one freed,
  after HOST-update's VM tests, which ask with `--first`), and tells it its
  share of the threads in `HEAVY_JOBS` (and `CARGO_BUILD_JOBS`,
  `RUST_TEST_THREADS`); `heavy --all -- COMMAND`
  takes every slot instead, for a run that must have the machine to itself
  (a long gate with a deadline, a nightly); `heavy status` says which are
  busy. A repo's own queue (the steward's proof slots, a repo's gate lock)
  still picks which of its jobs goes next.
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
- **`usage-reset`** (`burrow.usageReset`): `burrow-usage-reset`: the Claude
  plan's banked weekly reset gets used, once, when the week is nearly gone
  (under `below`, 2%, left), not left sitting while every Claude stops at the
  limit. It reads the weekly meter through Claude Code itself (its SDK's
  `get_usage`: no model call), counts an answer from Claude Code's cached copy
  as no reading at all, except that when its endpoint refuses (every session
  of the account shares its rate limit), a copy another session fetched since
  its last reading can say the week still has room, never that the reset is
  due or took. Past the line it types `/limit-reset weekly` into a
  Claude Code of its own in a tmux and confirms it, at most once a week; a
  reset that only works at the limit waits for it, and with nothing banked it
  does nothing. `HOST-notify` tells you when it used one, or when it couldn't.
  It books its own next look, sooner the faster the week is going, and never
  more than 15 minutes on: no timer. Each pass also keeps Claude's meters,
  its five-hour session and week, for proj's usage holds. Each fresh reading replaces
  `meters/ENGINE.json` and appends to `meters/log` in its state dir.

## Review and landing

- **`autoland`** (`burrow.autoland`): The PR steward for every repo you own:
  an AI reviewer checks each ready PR through its GitHub connector (Codex on
  the owner's ChatGPT account, set up once per repo), agent jobs settle every
  finding, red check and
  conflict with its base (what a job says it did goes up only once its push is
  the PR's head, and one whose PR someone else pushes to stands down), and
  once the reviewer finds nothing in the head's own change and CI is green on
  the head it merges and deletes the branch. A PR pushed again keeps its place:
  its reviews and fix jobs wait in line by when it was opened, not when its
  latest head came. A repo can land through
  [Conary](https://github.com/FieldmouseWorks/Conary)'s merge train instead
  (`train`, on `burrow.ledger`): settled PRs are merged onto the default
  branch together, and land once CI is green on that, a red batch split to
  find the PR that broke it. One-way paths get an automated judge; only what can't
  be undone waits for you, in one issue, and what the pipeline itself is stuck
  on goes to an agent job through proj's inbox. Watches each repo's Renovate
  dashboard too: an update stuck there without a PR (waiting days, or
  erroring) goes to an agent job, and to that inbox if it's still there a day
  later. So does each flaky test a gate quarantines
  ([docs/flaky-tests.md](flaky-tests.md)): an agent job fixes it, which
  lifts the quarantine. Keeps every repo at
  [docs/repo-baseline.md](repo-baseline.md). Repos in `shadow` have
  Conary's landing flow, the steward's replacement, decide beside each pass
  without acting (`cn flow compare`, in `autoland-shadow@`'s journal after
  each pass), showing where the two part before a repo switches over.
  A shadowed repo in `burrow.landing.conaryd` switches with one live
  setting, `settings landing OWNER/REPO conaryd` (and back with `steward`):
  burrow-landing hands it over once the one letting go has nothing at work
  there, and the two never land it at once
  ([docs/landing.md](landing.md)).
  `plugins.fix.enable` (off) runs Conary's fix plugin and its broker
  (`crates/burrow-fix`): the flow's fixes run as proj jobs on the fixing role
  (`plugins.fix.role`), one live fix a PR, stopped when the flow's request
  is; it needs `burrow.ledger.controllerUser`. `plugins.job.enable` (off)
  has the same broker serve the flow's `job` kind through a plugin of its
  own: a PR the flow can't settle, handed to the stuck role
  (`plugins.job.role`) as one proj job a PR, with the handoff and answer
  `crates/burrow-fix/src/stuck.rs` gives; it needs the machinery's App too,
  the account the job opens the PR's follow-up issue as, and the broker
  hands the job only a follow-up issue that App or the flow opened.
  `plugins.proof.enable` (off)
  runs Conary's proof plugin and its producer (`crates/burrow-proof`): a
  check's recipe from the repo's flow settings, run as a program on the
  exact commit, each run as a user systemd makes for it, and its verdict
  signed with the producer's own key (`plugins.proof.keyFile`) and posted
  as a check run; it needs `burrow.ledger.controllerUser` too.
  `plugins.judge.enable` (off) runs Conary's judge plugin and its producer
  (`crates/burrow-judge`): each request held to the PR's head, base,
  description and discussion on GitHub, judged in a look of the producer's
  own: Claude Code on the judging role's model (`plugins.judge.role`), in
  a fresh session, as a user systemd makes for it, reading a checkout the
  producer fetched, with its own login (`plugins.judge.claudeTokenFile`)
  and no network but Anthropic's API, none of it burrow.user's (a head no
  CI reported on is built first: each check the flow gives a program for,
  run as the proof worker runs one, in a unit of its own with no network
  and no Nix daemon, and the look shown what came of it); then
  published by the producer, a user of its own, as the sealed comment
  (`plugins.judge.sealFile`) and then `conary/judgment` signed with its own
  key (`plugins.judge.keyFile`); it needs `burrow.ledger.controllerUser`.
  Each enabled plugin is in a shadowed repo's flow config
  (`modules/autoland/shadow.nix`): its executable, its choice (the proof plugin's
  its own, in the base the build renders; a role's as the role settings say
  now, rendered on the machine into `burrow.stateDir`/flow-configs by
  `autoland-flow-configs`, a user of its own that alone can write there,
  at boot, each switch and each change to the role settings, so `proj
  settings model` reaches conaryd's next pass with no deploy, and a role
  its jobs would refuse fails the render, which keeps the last good
  configs), and, where the repo has
  evidence, its producer's public key (`plugins.proof.publicKey`,
  `plugins.judge.publicKey`) in the producers file, trusted for the repo's
  proofs or `conary/judgment` alone. The judge's seal key, and
  renovate-sweep's where it runs, reach the flow by their files alone
  (`judge_key_file`, `major_key_file`): `autoland-flow-keys`'s copies of
  its credentials, which only the controller can read and which every unit
  that runs the flow requires. The sweep's is made first
  (`renovate-sweep-key`), and taken again whenever the sweep makes a new
  one; in a repo burrow-landing's record gives conaryd, the sweep seals a
  major's verdicts as Conary's v2 issuances under it.
- **`ledger`** (`burrow.ledger`):
  [Conary](https://github.com/FieldmouseWorks/Conary)'s ledger of every PR on
  your repos: who made each change and why, its revisions, the checks and
  reviews on them, and where it landed. Each repo's PR events start its `cn
  sync`, the hourly catch-up syncs them all. It reads GitHub and writes only
  the ledger's anchor, a ref in `anchorRepo` that holds the log's head, and
  each repo's notes (`refs/notes/conary`), with session links only for the
  repos `sessionLinks` names. `cn why FILE:LINE` answers from the ledger here
  and from the notes in any other clone, and `cn verify` checks the ledger
  against the anchor. Its `cn` and conaryd are Conary at the rev burrow's
  crates link (`crates/Cargo.lock`, `burrow.lib.conary`), so the flow and
  the judge that checks it compute alike: bumping that pin moves both, and a
  `package` built from another rev fails the build. With `daemon`, conaryd serves the ledger's calls on
  `/run/conary/conaryd.sock`, read-only, as a user of its own, with no
  network, and every agent session of `burrow.user` gets them, and `why`, as the
  `conary` MCP server. With `door`, an issue labelled `undo` in the probe's
  repo asks for a `cn undo` without an agent in between
  ([front door](front-door.md#undo-with-no-claude)). With `gateReuse`,
  a conaryd of its own, as `burrow.user` with the ledger's GitHub tokens,
  answers flake-check.yml's gate-reuse lookups on `listen` (loopback, for
  this machine's runners), and over HTTPS at `path` on the events host for
  GitHub's (Conary's docs/gate-reuse.md); the socket's conaryd stays as it
  is. `policy` is
  Conary's class policy, which the steward asks for every PR it settles
  (`cn class`): which paths make a change R2 (CODEOWNERS and secrets by
  default), which are documentation (a change to them alone gets no review
  round, unless it's R2), and which repos go live when they land. With
  `controllerUser`, Conary's flow (its passes, workers and plugins) runs as a
  user of its own that no authoring job, sandbox or proj run shares, on state
  only it reads, carried to it from burrow.user's at the switch that turns it
  on (and on, or back, at one that changes it or turns it off: a fresh copy,
  refused while one of the flow's processes still runs as the user it had,
  or when both places hold state),
  and only it may reach a producer's socket under `/run/conary-producers`.
  It signs the ledger as burrow.user does, through ACLs on `dir` naming it,
  so its file system must support POSIX ACLs.
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
  continuations): the two go in the order their passes fell due, and neither
  starts one while one of the other's runs. A turn ending and the
  passes' own wake-ups (a settle delay, a review that hasn't come) kick them
  locally, and so does a switch that changes a handler's
  units. systemd holds the socket deliveries come in on, so one sent while
  burrow-events restarts waits for it, and one it had taken is answered
  before it stops. The one timer is an hourly catch-up,
  for deliveries GitHub never resends and repos too new to have a hook.
- **`watchdog`** (`burrow.watchdog`): `burrow-watchdog`: work that has stopped
  moving gets a look before anyone has to notice it. Each source keeps a file
  of what it waits on now and by when that should be over: the steward, every
  open PR's wait as its status says it (`stallAfterMinutes`, longer for a
  hold), due at once when one of its steps has failed three times in a row;
  proj, every job's turn waiting in line (`proj.stallAfterMinutes`), running
  past its timeout, awaiting GitHub past `awaitHours`, or ended without
  saying what comes next or in a failure (at once). Past a deadline it kicks
  that item's pass (one kick for all that share it, and none again when the
  run it started fails), then hands
  every stall that's ready to one agent job ahead of proj's line, then puts
  what's still stuck on the steward's stuck issue, whose own `--unstick` job
  takes it from there. A job of its own that's still waiting for a slot once
  none of its stalls need it (they cleared, or went on to the issue) is
  withdrawn (`proj withdraw`, which ends a job only while its turn waits for a
  slot, so one that has started is never stopped), so it never takes a slot
  for nothing. You hear only of what that issue can't take: the steward itself
  stuck, no steward, or that issue's own job stuck. A job that finds one
  legitimately slow gives it more time, once, with a reason. While proj says
  an engine is out of usage, it holds what only that engine can move (a
  turn in proj's line, a wait its source says is on a job using it) and not
  already on the issue or yours, and takes each up again from its kick after
  the reset. Anything else that stalls meanwhile (a source stuck or never
  written, a turn past its deadline) still gets its kick, and when no engine
  can take it, you hear of it by the deadline a job's would have had. It
  books its own next pass for the next deadline: no timer,
  and no GitHub calls of its own. `burrow-watchdog status` shows what it
  watches. With `units.enable` (on with the watchdog), what's broken on the
  machine itself is a source too (`burrow-units`): a failed unit, and a
  program that dumped core, whoever ran it, is a stall `units.graceMinutes`
  after it's seen (but a crash in a Nix build, in a proj job's slice,
  `units.ownedSlices`, or of a program built in a work tree,
  `units.workTrees` (projects and job scratch), which what ran it sees
  for itself), and a crash counts for `units.crashHours`; `units.ignore`
  takes globs of the units and programs that fail by design. A watcher
  follows the journal for systemd's word of a failure and systemd-coredump's
  of a dump and asks for a pass, touching no unit's own settings. The
  watchdog's job clears a crash whose cause it fixed with
  `burrow-units clear EXE`, and a later one is back. Clear uses a host
  acknowledgement socket, so sandboxed jobs need no host state or journal
  access: the host holds the units lock, records the clearance and requests
  a pass. Only executable names cross the socket. With
  `degradations.enable`, repeated soft CI failures feed the same ladder:
  [workflow degradations](degradations.md) describes the signal and options.
- **`dispatch`** (`burrow.dispatch`): `burrow-dispatch`: what every PR waits
  on together, the machine's Actions runners and its deploy, keeps moving. A
  runner (`runners`, by unit, with the repo and labels it takes jobs for) that
  sits idle while a job only it could take is queued is restarted after
  `quietMinutes`, or started at once if it failed: at most twice in six hours,
  and never while a switch is in flight. One with its `registration` (its
  `.runner`) set is checked against GitHub's list of runners where it was
  registered: one missing from it is reported as its registration gone,
  never restarted, and stopped with the `UNIT.path` that would start it
  again, since it exits as it starts (anything else that starts it is named
  in its wait, not stopped); it's registered again by hand. A push run on a runner repo's default
  branch that hasn't finished, started or not, is cancelled once a later run
  of its workflow on that branch is live, so the later one takes its place
  and gives the branch its result (a run someone ran again is left; this
  takes Actions: read and write on the machinery's App). With `fresh.enable`, each job queued
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
  `deploy.afterMinutes` with no run of `HOST-update` at work, or a run quiet in
  its logs past `deploy.runMinutes`,
  goes up the watchdog's ladder like any other stall. Its passes start on the
  runner repos' job events, the watched repos' pushes, `HOST-update` starting
  and ending, and its own wake-ups, at most one every two minutes. It never
  merges, closes or changes code. `burrow-dispatch status` shows what it sees
  and the moves it has made.
- **`builds`** (`burrow.builds`): The machine's build store
  ([gate design](gate-design.md#2-one-build-store-written-by-whoever-may-sign)):
  a Nix binary cache nginx serves at `https://HOST/PATH`, signed with a key
  the machine makes on first start and never hands out (its public half at
  `PATH/public-key`). A run writes it through the door
  ([crates/burrow-builds](../crates/burrow-builds)) at `PATH/upload`: its
  manifest, NARs and its job's OIDC token, whose audience names the
  manifest. The door asks Conary's `cn attest job` whether the job may sign
  (from the request's head first, before nginx takes the body, so a client
  without a job's token can't make nginx buffer an upload; then for the
  manifest's audience), takes only a public repo in `repos`, writes each NAR only once its hash is
  the manifest's, and past `maxSize` drops what nginx served longest ago.
  flake-check.yml writes it, in shadow for now, once a repo's or its
  organization's Actions variables name it: `BURROW_BUILDS_URL`
  (`https://HOST/PATH`) and `BURROW_BUILDS_PUBLIC_KEY` (`PATH/public-key`).

## Upkeep and alerts

- **`updates`** (`burrow.updates`): Everything current, unattended.
  `HOST-update` (on each push to the flake's main or a watched input, and
  hourly for nixpkgs and, when `claudeManifest` is configured, Claude Code)
  bumps the flake's inputs and Claude Code manifest, builds, runs your VM
  tests, switches with the dead man's switch,
  compares health checks before and after (`HOST-health`), confirms and pushes
  to main (as a PR the merge train lands, when the flake's repo has its train
  on); anything that breaks reverts and goes to an agent job. A failed VM test
  is a verdict, not a failed unit: the next run is the next push's or the
  hour's, not a retry. It deploys main
  too, so a merged PR goes live the same way. It never tests or switches to a
  main that has moved on: it looks again before each VM test (and in its
  heavy slot) and before the switch, and starts over from the new head. Waiting
  for a slot doesn't count against its time limit. The `proj-config` health check
  runs `proj check-config` as `burrow.user`, validating the deployed config
  and the role settings every turn reads before confirmation. Failed
  checks include stderr diagnostics in the rollback notice. A check that
  fails for its own reason excuses what can't pass without it
  (`burrow.updates.healthExplains`), so a known cause already failing before a
  switch doesn't revert a good deploy. Plus Renovate for every repo you
  own (`renovate`, merged on green CI, stuck PRs handed to an agent job; a repo's
  push to its default branch and its Renovate PRs' CI start both for that
  repo, and all of them run hourly; it acts as `burrow.updates.renovate.app`,
  the machinery's App unless it names one of its own, so its GitHub allowance
  stalls only Renovate, and its commits are that App's bot, whose PRs the
  steward knows as Renovate's), `cargo-follow` (`follow`: a Cargo pin on
  another repo's main kept at its head as one PR, opened or refreshed on
  either repo's push; the manifest's
  `metadata.follow` says what changes there are inert, so a commit that only
  moves the upstream's own pin back on the follower opens none, and two
  repos pinning each other don't chase one another), `advisory-watch` (CISA KEV, vulnix,
  osv-scanner; new CVEs become agent jobs the same hour), `firmware-check`
  (offers go to you: flashing is yours) and daily `rustup update`.
- **`house`** (`burrow.house`): `house-sync` (when the blocks change, and on
  each repo's pushes to its default branch): every repo's copies of the house
  blocks (the house rules, [house/rules.md](../house/rules.md), and the Pull
  requests section, [house/prs.md](../house/prs.md), each between its markers in
  AGENTS.md) kept the same as burrow's, as one PR per repo when a block
  changes. Broken markers, and repos it couldn't get through, go to the owner
  through `notify`; a pass GitHub's rate limit holds off is booked again for
  when it lifts, not a failure. See [docs/house-style.md](house-style.md).
- **`notify`** (`burrow.notify`): `HOST-notify`: what the owner must know, as
  one GitHub issue per problem in `burrow.notify.repo`, commented at most
  daily and closed when it clears. One closed by hand while its problem
  stands comes back a day later, saying so. One opened `--handled` (its
  opener settles it, with a job of its own where there is one) is never
  sorted or drained by the inbox.

## Machine configuration notes

Things you'll want to set yourself in your machine flake: an isolated login for
each sandbox engine (for example `burrow.agents.engines.claude.credentialFile`
or `burrow.engines.antigravity.credentialFile`, from sops-nix), extra allowed
hosts (`burrow.agents.extraAllow`), a git identity for the sandboxes
(`burrow.agents.guestModules`), an intro for the machine's instructions
(`burrow.claude.machineMd` with `lib.mkBefore` when using the Anthropic
surface), MCP servers of your own for every job engine and interactive session
(`burrow.mcpServers`), and, if you run Claude Code's newest release instead of
nixpkgs', a release manifest in your flake (`burrow.claude.manifest`, and its
path for `HOST-update` to bump in `burrow.updates.claudeManifest`). Persist
`burrow.stateDir` if `/` is ephemeral.

`HOST-update` pushes to `origin` with the user's git login. The rest of the
machine's own GitHub traffic (Renovate, notices, the ledger, house-sync, and
the agent jobs it starts by itself) acts as a GitHub App of yours when
`burrow.github.machinery` names one, and as the user's gh login otherwise; work
the user asks for (an issue of theirs in the inbox, a session of theirs) acts
as them either way. The autoland steward acts as an App of its own
(`burrow.autoland.app`;
[docs/repo-baseline.md](repo-baseline.md#the-stewards-github-app) says how to
make one).
