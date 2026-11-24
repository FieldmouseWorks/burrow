# burrow

NixOS modules for a self-operating machine built around coding agents.

Instead of slowing agents down with permission prompts and manual PR
babysitting, burrow moves safety from **asking first** to **undoing after**:
ZFS snapshots before deletes, a dead man's switch and trial boot for system and
kernel changes, automated review and CI landing backed by an auditable ledger,
and per-engine microVM sandboxes with explicit capability grants. Only one-way
doors (wiping disks, flashing firmware, deleting remote data, force-pushing
`main`) stop to ask.

It's the generic half of a real daily-driver box: an interactive operator
session plus headless project jobs, a tailnet web dashboard, GitHub-issue front
doors, and pluggable agent engines (`anthropic`, `antigravity`, or any
[Conary](https://github.com/FieldmouseWorks/Conary) `conary-plugin v5` plugin).

MIT licensed. Status: in daily use on one machine; options may still move.

## Why

Agents write code faster than a person can approve it. On a machine set up the
usual way, the slow part is the person: clicking "allow", reading every diff
before it merges, babysitting every update. burrow trades pre-approval for
automatic recovery: a deleted file is a snapshot away, a broken system switch
reverts itself, an update that fails its VM tests never stays live, and a PR
merges only once review and CI pass on the exact commit.

## How it works

```text
  You (interactive session, tailnet dashboard, or GitHub issues)
   │
   ├─▶ Front doors (no agent needed)
   │     probe (facts) · engines (switch setups) · undo (ledger revert) · settings
   │
   ├─▶ Work & isolation
   │     proj (inbox, triage/drain, planning, queues, per-role routing)
   │     agents (per-engine QEMU microVMs on ZFS clones + filtered egress proxy)
   │
   ├─▶ Review & landing
   │     events (webhooks) ─▶ reviewer + fix jobs ─▶ autoland / Conary flow & train
   │     ledger (auditable history + undo) · builds (signed Nix cache) · watchdog · dispatch
   │
   └─▶ Self-updating & safety net
         updates (flake, Renovate, Cargo pins, CVEs) ─▶ VM tests ─▶ switch (dead man's switch)
         undo (ZFS snapshots) · backup (off-site restic) · heartbeat · notify
```

1. **You work through narrow doors.** Talk to an interactive operator session
   on the box, check live state in the passkey-protected tailnet dashboard, or
   drive the box from GitHub issues—using cheap, agent-free doors for status
   probes, engine switches, settings, and ledger undos, and opening issues when
   work needs a coding agent ([docs/front-door.md](docs/front-door.md)).
2. **Jobs run headless or fenced in microVMs.** `proj` turns issues and commands
   into queued agent turns—triaging backlogs, splitting large tasks into child
   issues, routing roles via live settings, and tracking per-engine usage holds.
   Tasks and engine plugins that should be isolated run in per-engine QEMU
   microVMs on ZFS clones with filtered egress proxies and explicit capability
   grants (`agents`).
3. **PRs land once proven.** GitHub webhooks (`events`) wake the landing
   pipeline in seconds. A reviewer checks each PR, agent jobs settle findings,
   red checks, and conflicts, and either the steward (`autoland`) or Conary's
   landing flow and merge train merges once review and CI pass on the exact
   head. Every change is recorded in Conary's verifiable ledger (`ledger`), and
   `watchdog` and `dispatch` unstick stalled jobs, failed units, and CI runners.
4. **The machine keeps itself current and recoverable.** `updates` bumps flake
   inputs, agent tooling, and repo dependencies, runs your VM tests, activates
   behind `switch`'s dead man's switch and health checks, and reverts + files a
   fix job on failure. Off-site encrypted backups (`backup`), external pings
   (`heartbeat`), and one-issue-per-problem alerts (`notify`) cover the rest.

## Modules

Every module can be imported on its own (`burrow.nixosModules.<name>`) or all at
once via `burrow.nixosModules.default`. For full per-module behavior and
operational notes, see [docs/modules.md](docs/modules.md).

### Engines

| Module | Option | What it does |
| --- | --- | --- |
| [`anthropic`](modules/engines/anthropic.nix) | `burrow.claude` | Host Claude Code guardrails (`bypassPermissions`, R2 one-way prompts, PreToolUse snapshot guard) plus sandboxed `claude` / `claude-api` engine surfaces and `conary-plugin v5` plugins. |
| [`antigravity`](modules/engines/antigravity.nix) | `burrow.engines.antigravity` | Isolated Antigravity engine surface and `conary-plugin v5` job plugin (auth, runtime models/efforts, egress allowlist, capabilities). |

### Safety net

| Module | Option | What it does |
| --- | --- | --- |
| [`undo`](modules/undo.nix) | `burrow.undo` | Unprivileged `@pre-*` ZFS snapshots taken by the guard before deletes, with automatic pruning. |
| [`switch`](modules/switch.nix) | `burrow.switch` | Dead man's switch (`HOST-switch` / `HOST-confirm`) that reverts unconfirmed system changes, plus GRUB trial boot (`HOST-boot`) for new kernels. |
| [`backup`](modules/backup.nix) | `burrow.backup` | Off-site encrypted ZFS-snapshot backups via restic + rclone, with daily verification and restore drills. |
| [`ops`](modules/ops.nix) | `burrow.ops` | Diagnostics toolset (`perf`, `bpftrace`, `strace`, `smartctl`, `fwupd`), hardware watchdog, and `systemd-oomd` protecting SSH/Tailscale. |
| [`heartbeat`](modules/heartbeat.nix) | `burrow.heartbeat` | Off-box Healthchecks.io pings gated on `HOST-health`, plus optional external site checks. |
| [`robot`](modules/robot.nix) | `burrow.robot` | Hetzner Robot CLI (`status`, `rescue`, `reset`, `wol`) for out-of-band recovery. |

### Work and interfaces

| Module | Option | What it does |
| --- | --- | --- |
| [`proj`](modules/proj.nix) | `burrow.proj` | Headless job scheduler, GitHub issue inbox, triage/drain, task planning, live engine setups, and per-engine usage/quota holds ([proj-design](docs/proj-design.md), [task-planning](docs/task-planning.md)). |
| [`agents`](modules/agents.nix) | `burrow.agents` | Per-engine QEMU microVMs (`microvm.nix`) on ZFS clones with filtered egress proxies, capability-enforced plugin sandboxes, and confined `agent fetch` ([engines](docs/engines.md)). |
| [`probe`](modules/probe.nix) | `burrow.probe` | Agent-free, scrubbed, read-only machine queries via private GitHub issues or CLI ([front-door](docs/front-door.md)). |
| [`dashboard`](modules/dashboard.nix) | `burrow.dashboard` | Passkey-authenticated web app (installable PWA) over `tailscale serve` for live machine and queue status. |
| [`browser`](modules/browser.nix) | `burrow.browser` | Persistent signed-in Chromium exposed to agents via Playwright MCP and to you over tailnet noVNC, isolated by user and nftables. |
| [`settings`](modules/settings.nix) | `burrow.settings` | Schema-backed pipeline options (`proj settings show`), live overlay, and independent GitHub settings door ([settings](docs/settings.md), [reference](docs/settings-reference.md)). |
| [`toolbox`](modules/toolbox.nix) | `burrow.toolbox` | Standard dev toolchains (`gh`, `jj`, Rust, Go, Node, Python, `uv`, QEMU, `flaky`, `flake-gate`) shared by host and sandboxes. |
| [`rust-cache`](modules/rust-cache.nix) | `burrow.rustCache` | Per-user chrooted `sccache` daemons over Unix sockets in client-side mode for shared Rust builds. |
| [`cpu`](modules/cpu.nix) | `burrow.cpu` | Slice CPU weights favoring CI gates plus `heavy`, a machine-wide slot queue for builds and VM tests. |
| [`github`](modules/github.nix) | `burrow.github` | Per-App GitHub installation-token broker over Unix sockets for `gh`, `git`, and LFS, with no fallback to personal logins. |
| [`usage-reset`](modules/usage-reset.nix) | `burrow.usageReset` | Tracks Claude plan usage meters for `proj` holds and automatically triggers a banked weekly reset near exhaustion. |

### Review and landing

| Module | Option | What it does |
| --- | --- | --- |
| [`autoland`](modules/autoland.nix) | `burrow.autoland` | PR steward, Renovate dashboard watcher, flaky-test fixer, repo baseline keeper, and Conary landing-flow shadow/plugins (`fix`, `job`, `proof`, `judge`) ([repo-baseline](docs/repo-baseline.md), [landing](docs/landing.md)). |
| [`ledger`](modules/ledger.nix) | `burrow.ledger` | Conary PR provenance ledger (`cn why`, `cn verify`), read-only MCP daemon, `undo` issue door, gate-reuse server, and isolated flow controller user. |
| [`events`](modules/events.nix) | `burrow.events` | Socket-activated GitHub webhook receiver and local event dispatcher that starts passes in seconds. |
| [`watchdog`](modules/watchdog.nix) | `burrow.watchdog` | Deadline-driven stall detector for PRs, jobs, failed systemd units, core dumps, and soft CI degradations ([degradations](docs/degradations.md)). |
| [`dispatch`](modules/dispatch.nix) | `burrow.dispatch` | Self-hosted GitHub Actions runner supervision, single-job JIT fresh runners, stale run cancellation, and deploy-lag tracking. |
| [`builds`](modules/builds.nix) | `burrow.builds` | Signed Nix binary cache served by nginx and writable only by OIDC-attested CI jobs ([gate-design](docs/gate-design.md)). |

### Upkeep and alerts

| Module | Option | What it does |
| --- | --- | --- |
| [`updates`](modules/updates.nix) | `burrow.updates` | Unattended `HOST-update` deploys, Renovate across owned repos, Cargo pin following, CVE scanning (`advisory-watch`), firmware notices, and daily `rustup update`. |
| [`house`](modules/house.nix) | `burrow.house` | Keeps shared `AGENTS.md` house rule blocks synced across owned repositories via PRs ([house-style](docs/house-style.md)). |
| [`notify`](modules/notify.nix) | `burrow.notify` | Owner alerts as one GitHub issue per active problem, rate-limited daily and auto-closed when resolved. |

## Quick start

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
              checks = [ "rehearsal" ]; # your flake's VM tests
            };
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

See [examples/minimal.nix](examples/minimal.nix) for a complete (diskless)
machine enabling every module.

### Assumptions

- **ZFS** with a data dataset for project checkouts and sandbox clones
  (`burrow.agents.dataDataset`, `burrow.projectsDir`).
- **One primary user** (`burrow.user`) with a fixed UID and passwordless sudo;
  undo and sandboxing provide safety rather than interactive prompts.
- **Local flake checkout** (`burrow.flake`) so the operator and `HOST-update`
  can edit, test, and switch the machine.
- **Rolling ZFS snapshots** (such as `sanoid`) configured by your machine;
  `undo` manages guard snapshots and `backup` ships yours off-site.

In a real machine flake you will also wire your own secrets (e.g. via
`sops-nix`), engine credentials, GitHub Apps (`burrow.github.machinery` and
`burrow.autoland.app`; see
[docs/repo-baseline.md](docs/repo-baseline.md#the-stewards-github-app)),
optional MCP servers (`burrow.mcpServers`), and persistence for
`burrow.stateDir` if `/` is ephemeral.

## Bringing your own engine

Every agent role is designed to sit behind Conary's `conary-plugin v5` contract
so the machine isn't tied to one vendor or CLI:

- **Today:** `anthropic` and `antigravity` provide isolated engine surfaces,
  credentials, egress proxies, and packaged plugins; `proj settings` routes
  roles live across the engines `proj` currently executes (`claude` and
  `claude-api`); and `lib.engineConformance` verifies any plugin inside its
  microVM sandbox against its declared capabilities.
- **In progress:** completing the generic plugin runner across all `proj` roles
  and `burrow-judge`, pinned-hash plugin installation via the settings door
  ([#668](https://github.com/FieldmouseWorks/burrow/issues/668)), and swappable
  PR review plugins.
- **Add an engine:** follow [docs/adding-an-engine.md](docs/adding-an-engine.md)
  (built on [docs/engines.md](docs/engines.md),
  [crates/burrow-example-plugin](crates/burrow-example-plugin), and
  [modules/engines/antigravity.nix](modules/engines/antigravity.nix)) — laid out
  so you can point an agent at this repo to build and verify a new provider.

## Documentation

- **Using the box:** [docs/front-door.md](docs/front-door.md) (probes, jobs, triage/drain, undo, engines door) · [docs/modules.md](docs/modules.md) (detailed module reference)
- **Configuration:** [docs/settings.md](docs/settings.md) · [docs/settings-reference.md](docs/settings-reference.md) (generated from schema)
- **Engines & plugins:** [docs/engines.md](docs/engines.md) · [docs/adding-an-engine.md](docs/adding-an-engine.md)
- **Landing & CI:** [docs/repo-baseline.md](docs/repo-baseline.md) · [docs/landing.md](docs/landing.md) · [docs/gate-design.md](docs/gate-design.md) · [docs/flaky-tests.md](docs/flaky-tests.md) · [docs/degradations.md](docs/degradations.md) · [docs/house-style.md](docs/house-style.md)
- **Job scheduler:** [docs/proj-design.md](docs/proj-design.md) · [docs/proj-deploy.md](docs/proj-deploy.md) · [docs/task-planning.md](docs/task-planning.md)

## Checks

```sh
flake-gate                                    # CI gate, evaluated per output (see AGENTS.md)
nix build .#checks.x86_64-linux.agents -L     # nested-KVM sandbox & plugin conformance tests
```

`checks.agents` boots a host VM with nested KVM to verify sandbox isolation,
egress filtering, `agent fetch`, `proj run --vm`, capability grants
(`agents.entries.plugins`), and plugin conformance
(`conformance-example`, `conformance-antigravity`, `conformance-claude`,
`conformance-claude-api`).

## License

MIT.
