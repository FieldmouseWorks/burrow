# burrow

NixOS modules for a machine you run by talking to Claude. Claude Code gets the
whole box, with no permission prompts, because everything it can do is either
undone automatically or is one of the few one-way doors that still asks.

It's the generic half of a real box: one always-on "operator" Claude session
you talk to from your phone, which changes the system itself and hands project
work to Claude sessions inside each project.

MIT licensed. Status: in daily use on one machine; options may still move.

## What's in it

| Module | Option | What it does |
|---|---|---|
| `claude` | `burrow.claude` | Claude Code system-wide in `bypassPermissions`, set in `/etc/claude-code/managed-settings.json` so no project can override it. The only prompts are `permissions.ask` rules for one-way doors (R2: destroying pools, wiping disks, flashing firmware, deleting remote data, force-pushing `main`). A PreToolUse guard snapshots before anything deletes files (R1) and asks before force-pushing a default branch. Also writes the machine's `/etc/claude-code/CLAUDE.md` from what the other modules contribute. |
| `undo` | `burrow.undo` | The guard's `@pre-*` ZFS snapshots: lets the user take them without sudo, prunes them after a few days. |
| `backup` | `burrow.backup` | `HOST-backup`: each new snapshot of the datasets you name, off the box minutes after it's taken (when the units in `after`, sanoid say, finish; daily regardless). Restic through rclone, so to Google Drive, S3, another box or anything else rclone reaches: deduplicated, zstd-compressed, and encrypted with a password the box and you hold. It reads the snapshot itself at the datasets' own paths, so a run reads and sends only what changed, and holds that snapshot until the next one is off the box. Daily it forgets what `keep` doesn't keep, reads 1/`verifyDays` of the repository back, checks the newest backup holds everything its snapshot had, and restores the flake's checkout to scratch to compare with that snapshot, byte for byte (`HOST-backup drill PATH` for anything else). Failures go to a Claude job, and to you once the newest copy off the box is `staleAfter` hours old. |
| `switch` | `burrow.switch` | `HOST-switch` / `HOST-confirm`: a dead man's switch for system changes. The new system goes live but isn't the boot default; unless confirmed within 10 minutes the old one comes back by itself. So a change that cuts off SSH, the network or Claude undoes itself. `--abort` reverts at once; with `flakeInputs` it refuses a system built from an older nixpkgs. With `trialBoot` (GRUB), `HOST-boot` boots a new kernel, initrd or command line once at a quiet hour and makes it the default only after it came up; one that panics or hangs resets into the last good one and is never tried again. |
| `updates` | `burrow.updates` | Everything current, unattended. `HOST-update` (on each push to the flake's main or a watched input, and hourly for nixpkgs and Claude Code) bumps the flake's inputs and Claude Code, builds, runs your VM tests, switches with the dead man's switch, compares health checks before and after (`HOST-health`), confirms and pushes to main (as a PR the merge train lands, when the flake's repo has its train on); anything that breaks reverts and goes to a Claude job. It deploys main too, so a merged PR goes live the same way. Plus Renovate for every repo you own (`renovate`, merged on green CI, stuck PRs handed to Claude; a repo's push to its default branch and its Renovate PRs' CI start both for that repo, and all of them run hourly), `advisory-watch` (CISA KEV, vulnix, osv-scanner; new CVEs become Claude jobs the same hour), `firmware-check` (offers go to you: flashing is yours) and daily `rustup update`. |
| `notify` | `burrow.notify` | `HOST-notify`: what the owner must know, as one GitHub issue per problem in `burrow.notify.repo`, commented at most daily and closed when it clears. |
| `remote-control` | `burrow.remoteControl` | An always-on `claude remote-control` server for every folder in `burrow.projectsDir`, found by itself (a path unit rescans when folders come and go) and trusted by itself, so none waits on a prompt. Sessions started from claude.ai each get their own git worktree. The machine's own flake checkout is one of them: that session is the operator. The servers keep memory back when the machine runs short (`memoryLow`, 10% of it), and each server and its sessions share a backstop for a runaway (`memoryMax`, half the machine, or `servers.NAME.memoryMax`): past it the kernel kills the biggest process there, the session that grew or a build it ran, and nothing else. |
| `proj` | `burrow.proj` | `proj run PROJECT "task"`: the operator hands work to a headless Claude inside that project, as a systemd unit, and checks on it later (`status`, `log`, `reply`, `stop`). At most `maxJobs` run at once, `maxJobsPerProject` from one project, and the rest queue (`proj status --line` shows the whole line, next first, with each turn's wait; `--first` goes ahead, and `--pipeline` jobs, the steward's and renovate-sweep's and issues in `autoland.pipelineRepos`, go ahead of everything and have `pipelineSlots` slots of their own in those repos' projects, `autoland.leadRepos`' first of all, though they leave everyone else one slot while one of theirs waits and none runs, and go as `--first` in any other; an `--unstick` job, the one that takes what the pipeline is stuck on, waits in no line at all, in a slot of its own past `maxJobs`); every job and every Remote Control server lives in `claude.slice`, which has no ceiling of its own: the jobs are what the machine gives up first when it runs short (the kernel kills them first, and systemd-oomd watches their slice), and each may have a cap of its own (`jobMemoryMax`, none by default). With `inbox.enable`, an issue labelled `claude` in a project's GitHub repo, opened by one of `inbox.authors`, becomes a job too: the answer comes back as a comment and a reply there continues it. One that also has `inbox.unstickLabel` (the steward's stuck issue) is run `--unstick`. Every turn says what comes next before it answers: `proj await` a PR or issue (the job holds no slot and is resumed when it changes), `proj ask` a person (their reply resumes it) or `proj done`; one that won't is told once and answers again, and still silent is a stall. Only jobs whose starter acts on each turn's end (`--owned`: the steward's, the watchdog's) needn't say. |
| `events` | `burrow.events` | `burrow-events`: what happens on GitHub, or on the box, starts the work it calls for within seconds; nothing waits for a timer. Each repo's webhook (kept by autoland, on its own repos and on the projects' in `projectsDir`) posts to `https://HOST/PATH`, and every delivery GitHub signed starts the pass of each handler that takes its event, for that repo: the steward's, `proj-watch`'s, `proj-inbox`'s, `HOST-update`'s, Renovate's and `renovate-sweep`'s. A handler can take only some repos, only pushes to the default branch, only the other events about some branches, and only the issue events about issues with some titles. A turn ending, a review coming back and the passes' own wake-ups (a settle delay, a reviewer's timeout) kick them locally, and so does a switch that changes a handler's units. The one timer is an hourly catch-up, for deliveries GitHub never resends and repos too new to have a hook. |
| `autoland` | `burrow.autoland` | The PR steward for every repo you own: Codex reviews each ready PR, Claude jobs settle every finding, red check and conflict with its base, and once Codex finds nothing on the exact head and CI is green it merges and deletes the branch. A repo can land through [Conary](https://github.com/FieldmouseWorks/Conary)'s merge train instead (`train`, on `burrow.ledger`): settled PRs are merged onto the default branch together, and land once CI is green on that, a red batch split to find the PR that broke it. One-way paths get a Claude judge; only what can't be undone waits for you, in one issue, and what the pipeline itself is stuck on goes to a Claude job through proj's inbox. Watches each repo's Renovate dashboard too: an update stuck there without a PR (waiting days, or erroring) goes to a Claude job, and to that inbox if it's still there a day later. So does each flaky test a gate quarantines ([docs/flaky-tests.md](docs/flaky-tests.md)): a Claude job fixes it, which lifts the quarantine. Keeps every repo at [docs/repo-baseline.md](docs/repo-baseline.md). |
| `watchdog` | `burrow.watchdog` | `burrow-watchdog`: work that has stopped moving gets a look before anyone has to notice it. Each source keeps a file of what it waits on now and by when that should be over: the steward, every open PR's wait as its status says it (`stallAfterMinutes`, longer for a hold), due at once when one of its steps has failed three times in a row; proj, every job's turn waiting in line (`proj.stallAfterMinutes`), running past its timeout, awaiting GitHub past `awaitHours`, or ended without saying what comes next or in a failure (at once). Past a deadline it kicks that item's pass, then hands every stall that's ready to one Claude job ahead of proj's line, then puts what's still stuck on the steward's stuck issue, whose own `--unstick` job takes it from there. You hear only of what that issue can't take: the steward itself stuck, no steward, or that issue's own job stuck. A job that finds one legitimately slow gives it more time, once, with a reason. It books its own next pass for the next deadline: no timer, and no GitHub calls of its own. `burrow-watchdog status` shows what it watches. |
| `dispatch` | `burrow.dispatch` | `burrow-dispatch`: what every PR waits on together, the machine's Actions runners and its deploy, keeps moving. A runner (`runners`, by unit, with the repo and labels it takes jobs for) that sits idle while a job only it could take is queued is restarted after `quietMinutes`, or started at once if it failed: at most twice in six hours, and never while a switch is in flight. A push run on a runner repo's default branch that hasn't started is cancelled once a later run of its workflow on that branch is live, so the later one takes its place (a run that has started finishes, so the branch always gets a result; this takes Actions: read and write on the machinery's App). Each repo `HOST-update` watches has the commits the running system wasn't built from counted: its inputs' as `flake.lock` pins them, and the flake's own from the running system's `system.configurationRevision`, as `HOST-update` reads it. Both are stall watchdog sources, so a runner still idle after that, a commit not live after `deploy.afterMinutes`, or a run of `HOST-update` past `deploy.runMinutes`, goes up the watchdog's ladder like any other stall. Its passes start on the runner repos' job events, the watched repos' pushes, `HOST-update` starting and ending, and its own wake-ups, at most one every two minutes. It never merges, closes or changes code. `burrow-dispatch status` shows what it sees and the moves it has made. |
| `ledger` | `burrow.ledger` | [Conary](https://github.com/FieldmouseWorks/Conary)'s ledger of every PR on your repos: who made each change and why, its revisions, the checks and reviews on them, and where it landed. Each repo's PR events start its `cn sync`, the hourly catch-up syncs them all. It reads GitHub and writes only the ledger's anchor, a ref in `anchorRepo` that holds the log's head, and each repo's notes (`refs/notes/conary`), with session links only for the repos `sessionLinks` names. `cn why FILE:LINE` answers from the ledger here and from the notes in any other clone, and `cn verify` checks the ledger against the anchor. With `daemon`, conaryd serves the ledger's calls on `/run/conary/conaryd.sock`, read-only, as a user of its own, with no network, and every Claude of `burrow.user` gets them, and `why`, as the `conary` MCP server. |
| `agents` | `burrow.agents` | `agent new PROJECT`: a microVM per task (microvm.nix, QEMU) on a ZFS clone of the data dataset, Claude with permissions bypassed inside, egress only through an allowlist proxy, no GitHub token; the work comes back with `agent fetch` as `refs/agents/ID/*`. Idle ones stop after a day and are saved and deleted after a week. Opt-in per task. |
| `browser` | `burrow.browser` | A Chromium that stays up on the machine, signed in to your accounts by you, that every Claude there drives through Playwright's MCP server (`browser`, in the user's Claude config), which saves what Claude asks of it (a screenshot, a page's text) in the user's runtime dir, in RAM, rather than the checkout Claude works in. You watch it and sign in through noVNC on `viewer.interfaces` (a tailnet's, say), and nowhere else even where the firewall trusts an interface: no password, so only name interfaces that reach only you, and its WebSocket takes only pages from `viewer.origins` and its pages refuse to be framed, so another site open in your browser can't drive it or steer your clicks into it. Its profile holds your logins, which outlive a restart or reboot (it restores its last session, so cookies without an expiry date stay too, and it opens the same Chromium profile every time, `profile`, so a person added in the viewer doesn't leave it on the profile picker): its own user, mode 0700, outside the data dataset the sandboxes clone; persist `/var/lib/burrow-browser` if `/` is ephemeral, and keep it out of anything that leaves the machine unencrypted. Its DevTools port and viewer refuse every local user but `burrow.user` (an nftables table of its own under either firewall, so no other rule's accept gets past it, replaced without a gap on reload and gone with the module; nothing listens while it's down), so neither the sandboxes' proxy nor the reviewers nor CI can drive it. The machine's CLAUDE.md tells Claude to go at a person's pace and to ask before anything that posts, sends, pays or deletes as you. |
| `robot` | `burrow.robot` | `robot status / rescue / reset / wol` for a Hetzner dedicated server, through the Robot webservice. The way back in when the box is down. |
| `heartbeat` | `burrow.heartbeat` | Something off the box watches it: every 5 minutes it pings a [Healthchecks](https://healthchecks.io) check (healthchecks.io's free plan, or your own instance) with how it is. Success while every `HOST-health` check passes, a fail ping naming the check once one has failed two beats running, and when the pings stop (down, hung, cut off) Healthchecks alerts you by email or push after the check's grace. Nothing on the box has to be alive for that. |
| `usage-reset` | `burrow.usageReset` | `burrow-usage-reset`: the Claude plan's banked weekly reset gets used, once, when the week is nearly gone (under `below`, 2%, left), not left sitting while every Claude stops at the limit. It reads the weekly meter through Claude Code itself (its SDK's `get_usage`: no model call), and past the line types `/limit-reset weekly` into a Claude Code of its own in a tmux and confirms it, at most once a week; a reset that only works at the limit waits for it, and with nothing banked it does nothing. `HOST-notify` tells you when it used one, or when it couldn't. It books its own next look, sooner the faster the week is going: no timer. |
| `ops` | `burrow.ops` | perf, bpftrace, bcc, strace, gdb, tcpdump, nvme-cli, smartctl, fwupd and friends, a systemd hardware watchdog (with `watchdogIdentity`, the board's own one, whichever driver came up first, and a `HOST-health` check that it's the one armed), and systemd-oomd watching the whole machine: when swap is nearly full, or a slice is stalled on memory, it kills the unit that's drowning it (never sshd, tailscaled or a Remote Control server), long before the kernel's OOM killer would; and sshd, tailscaled, networkd, the journal and oomd itself keep memory back (`protect`), so you can still get in. |
| `cpu` | `burrow.cpu` | How the CPU is shared between repos and runners. The slices that hold gates (`gates.slices`, CI runners say) get `gates.weight` (400) against the 100 of every Claude and of nix-daemon's builds, so a gate keeps its share whatever else runs. `heavy -- COMMAND` runs a heavy job (a gate, a VM test, a replay, a large build) in one of `heavy.slots` machine-wide slots, for every repo and account alike, waiting while all are busy, and tells it its share of the threads in `HEAVY_JOBS` (and `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`); `heavy status` says which are busy. A repo's own queue (the steward's proof slots, a repo's gate lock) still picks which of its jobs goes next. |
| `toolbox` | `burrow.toolbox` | The dev tools projects and agents expect (gh, jj, rust, go, node, python, uv, qemu, nix-ld, direnv, `flaky`, which quarantines a gate's flaky tests: [docs/flaky-tests.md](docs/flaky-tests.md), and `flake-gate`, which runs a repo's CI gate here: the shared workflow's gate step, with the inputs the repo's `flake.yml` gives it). The sandboxes always get it. |
| `rust-cache` | `burrow.rustCache` | Every Rust build on the machine shares a compile cache: cargo runs rustc through [sccache](https://github.com/mozilla/sccache), so a dependency compiled once comes out of the cache in the next fresh worktree or CI checkout. One cache per user, never shared, since a user's builds fill it with what that user's next build trusts: `burrow.user`'s, and those of the `users` you list (CI runners' users, say). Each user's cache has one daemon, `rust-cache-USER`, on a socket only that user can reach (sccache's default TCP port lets any local user compile, and so run code, as whoever started it). The daemon only stores: the compiles run in the builds, under their own units' limits (sccache's client-side mode), so a runner's sandbox needs only to read the cache. A build that asks the daemon to compile anyway is refused (sccache is patched for that) and compiles itself, and the daemon runs in a chroot holding only sccache and the cache, since a client can ask it for any path. Whatever keeps the cache from working compiles without it. The caches live in `/var/cache/sccache`: keep them out of snapshots and backups. |
| `house` | `burrow.house` | `house-sync` (when the blocks change, and on each repo's pushes to its default branch): every repo's copies of the house blocks (the house rules, [house/rules.md](house/rules.md), and the Pull requests section, [house/prs.md](house/prs.md), each between its markers in AGENTS.md) kept the same as burrow's, as one PR per repo when a block changes. Broken markers, and repos it couldn't get through, go to the owner through `notify`. See [docs/house-style.md](docs/house-style.md). |
| `github` | `burrow.github` | Tokens of GitHub Apps of yours, for what on the machine should act as an App rather than as your own gh login. Each App's socket (`/run/gh-token/NAME.sock`, its `users` only) is answered by `gh-token` as a user of its own, with the App's key a credential only it can read: an installation token for the account or repo asked for, kept until it has fifty minutes left, so one lasts a job that takes it at the start. A process acts as the App with the App's `env`: `gh` then gets a token for the account its call names, and git's only credential helper gets one for the repo it pushes to, and neither can fall back to your login when the App's can't be had. |

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
servers of your own for every Claude session (`burrow.claude.mcpServers`), and, to
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
