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
| `switch` | `burrow.switch` | `HOST-switch` / `HOST-confirm`: a dead man's switch for system changes. The new system goes live but isn't the boot default; unless confirmed within 10 minutes the old one comes back by itself. So a change that cuts off SSH, the network or Claude undoes itself. |
| `remote-control` | `burrow.remoteControl` | An always-on `claude remote-control` server for every folder in `burrow.projectsDir`, found by itself (a path unit rescans when folders come and go). The machine's own flake checkout is one of them: that session is the operator. |
| `proj` | `burrow.proj` | `proj run PROJECT "task"`: the operator hands work to a headless Claude inside that project, as a systemd unit, and checks on it later (`status`, `log`, `reply`, `stop`). |
| `agents` | `burrow.agents` | `agent new PROJECT`: a microVM per task (microvm.nix, QEMU) on a ZFS clone of the data dataset, Claude with permissions bypassed inside, egress only through an allowlist proxy, no GitHub token; the work comes back with `agent fetch` as `refs/agents/ID/*`. Idle ones stop after a day and are saved and deleted after a week. Opt-in per task. |
| `robot` | `burrow.robot` | `robot status / rescue / reset / wol` for a Hetzner dedicated server, through the Robot webservice. The way back in when the box is down. |
| `ops` | `burrow.ops` | perf, bpftrace, bcc, strace, gdb, tcpdump, nvme-cli, smartctl, fwupd and friends, and a systemd hardware watchdog. |
| `toolbox` | `burrow.toolbox` | The dev tools projects and agents expect (gh, jj, rust, go, node, python, uv, qemu, nix-ld, direnv). The sandboxes always get it. |

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

  outputs = { nixpkgs, burrow, ... }: {
    nixosConfigurations.mybox = nixpkgs.lib.nixosSystem {
      modules = [
        burrow.nixosModules.default
        ./configuration.nix
        {
          burrow = {
            owner = "Sam";
            claude.enable = true;
            undo = { enable = true; datasets = [ "rpool/safe/home" "rpool/safe/data" ]; };
            switch.enable = true;
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
identity for the sandboxes (`burrow.agents.guestModules`), and an intro for the
machine's CLAUDE.md (`burrow.claude.machineMd` with `lib.mkBefore`).

After the first boot, each project's Remote Control server needs its folder
trusted once by hand (see the top of `modules/remote-control.nix`).

## Checks

```sh
nix flake check -L    # the example builds; `agents` boots sandboxes (needs /dev/kvm)
```

`checks.agents` (`tests/agents.nix`) boots a host VM that boots sandboxes inside
itself (nested KVM) and checks isolation, egress, fetch, `proj run --vm` and
cleanup.
