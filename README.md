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
| `switch` | `burrow.switch` | `HOST-switch` / `HOST-confirm`: a dead man's switch for system changes. The new system goes live but isn't the boot default; unless confirmed within 10 minutes the old one comes back by itself. So a change that cuts off SSH, the network or Claude undoes itself. `--abort` reverts at once; with `flakeInputs` it refuses a system built from an older nixpkgs. With `trialBoot` (GRUB), `HOST-boot` boots a new kernel, initrd or command line once at a quiet hour and makes it the default only after it came up; one that panics or hangs resets into the last good one and is never tried again. |
| `updates` | `burrow.updates` | Everything current, unattended. `HOST-update` (every 15 minutes) bumps the flake's inputs and Claude Code, builds, runs your VM tests, switches with the dead man's switch, compares health checks before and after (`HOST-health`), confirms and pushes to main; anything that breaks reverts and goes to a Claude job. It deploys main too, so a merged PR goes live the same way. Plus Renovate for every repo you own (`renovate`, merged on green CI, stuck PRs handed to Claude), `advisory-watch` (CISA KEV, vulnix, osv-scanner; new CVEs become Claude jobs the same hour), `firmware-check` (offers go to you: flashing is yours) and daily `rustup update`. |
| `notify` | `burrow.notify` | `HOST-notify`: what the owner must know, as one GitHub issue per problem in `burrow.notify.repo`, commented at most daily and closed when it clears. |
| `remote-control` | `burrow.remoteControl` | An always-on `claude remote-control` server for every folder in `burrow.projectsDir`, found by itself (a path unit rescans when folders come and go) and trusted by itself, so none waits on a prompt. Sessions started from claude.ai each get their own git worktree. The machine's own flake checkout is one of them: that session is the operator. |
| `proj` | `burrow.proj` | `proj run PROJECT "task"`: the operator hands work to a headless Claude inside that project, as a systemd unit, and checks on it later (`status`, `log`, `reply`, `stop`). At most `maxJobs` run at once and the rest queue (`--first` goes ahead); every job and every Remote Control server lives in `claude.slice`, capped by `burrow.claude.memory`, so Claude can only run out of its own memory, never the machine's. With `inbox.enable`, an issue labelled `claude` in a project's GitHub repo, opened by one of `inbox.authors`, becomes a job too: the answer comes back as a comment and a reply there continues it. |
| `agents` | `burrow.agents` | `agent new PROJECT`: a microVM per task (microvm.nix, QEMU) on a ZFS clone of the data dataset, Claude with permissions bypassed inside, egress only through an allowlist proxy, no GitHub token; the work comes back with `agent fetch` as `refs/agents/ID/*`. Idle ones stop after a day and are saved and deleted after a week. Opt-in per task. |
| `robot` | `burrow.robot` | `robot status / rescue / reset / wol` for a Hetzner dedicated server, through the Robot webservice. The way back in when the box is down. |
| `ops` | `burrow.ops` | perf, bpftrace, bcc, strace, gdb, tcpdump, nvme-cli, smartctl, fwupd and friends, and a systemd hardware watchdog. |
| `toolbox` | `burrow.toolbox` | The dev tools projects and agents expect (gh, jj, rust, go, node, python, uv, qemu, nix-ld, direnv). The sandboxes always get it. |
| `house` | `burrow.house` | `house-sync` (daily): every repo's copy of the house rules block ([house/rules.md](house/rules.md), between its markers in AGENTS.md) kept the same as burrow's, as one PR per repo when the rules change. Broken markers, and repos it couldn't get through, go to the owner through `notify`. See [docs/house-style.md](docs/house-style.md). |

## Assumptions

- ZFS, with the data you work on in one dataset (`burrow.agents.dataDataset`,
  default `rpool/safe/data` at `/data`) and projects under it
  (`burrow.projectsDir`, default `/data/dev/src`).
- One user for you and your agents (`burrow.user`, default `dev`) with a fixed
  uid and passwordless sudo. Undo is what keeps that safe, not prompts.
- The machine's own flake is checked out at `burrow.flake` (default
  `PROJECTS/HOSTNAME`), so the operator can edit and switch it.
- Rolling snapshots (sanoid or similar) of the datasets you care about are
  yours to set up; `undo` only covers the guard's.

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
machine's CLAUDE.md (`burrow.claude.machineMd` with `lib.mkBefore`), and, to
run Claude Code's newest release instead of nixpkgs', a release manifest in
your flake (`burrow.claude.manifest`, and its path for `HOST-update` to bump
in `burrow.updates.claudeManifest`). Persist `burrow.stateDir` if `/` is
ephemeral. `HOST-update` pushes to `origin` with the user's git login, and
everything that reaches GitHub (Renovate, notices) uses their gh login.

After the first boot, run `claude remote-control` by hand once, anywhere, to
log in and enable Remote Control. Every project folder after that is trusted
and served on its own.

To run every project from one claude.ai project chat, see
[docs/front-door.md](docs/front-door.md).

## Checks

```sh
nix flake check -L    # the example builds; `agents` boots sandboxes (needs /dev/kvm)
```

`checks.agents` (`tests/agents.nix`) boots a host VM that boots sandboxes inside
itself (nested KVM) and checks isolation, egress, fetch, `proj run --vm` and
cleanup.
