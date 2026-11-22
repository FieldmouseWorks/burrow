# Engines and outside-service boundaries

Every surface in the pipeline that talks to an outside service (an LLM
harness or API, a review connector, a VCS host, a backup remote, a notifier)
belongs behind one narrow, replaceable boundary. Adding or swapping a
provider should touch one adapter or plugin and its Nix module, never the
scheduler, the inbox, the settings store, or the landing flow.

This page records what already keeps that boundary across burrow, Conary and
a machine flake, what still couples burrow to Claude Code, and how the
remaining work fits the active redesigns.

## What is already modular

- **Landing flow and plugin contract (Conary).** Conary pins no engine, model
  or effort ([`no-pinned-engines`](https://github.com/FieldmouseWorks/Conary/blob/main/docs/DESIGN.md#61-execution-choices-and-independent-judging),
  [`fresh-judge`](https://github.com/FieldmouseWorks/Conary/blob/main/docs/plugins.md#independence-and-execution-failures)).
  `conary-plugin v5` ([plugins.md](https://github.com/FieldmouseWorks/Conary/blob/main/docs/plugins.md))
  is a language- and harness-independent JSON contract over stdin/stdout with
  model/effort/capability/meter negotiation and session-independence checks.
  `conaryd` reloads its configured `plugins` and `execution` choices on every
  pass ([flow.md](https://github.com/FieldmouseWorks/Conary/blob/main/docs/flow.md)).
- **Live routing, offers and saved setups.** Per-role and per-repo engine
  routing, per-engine model and effort offers, and atomic setup switches
  (`proj engines use NAME` / `use back`) already live in `burrow-settings`
  and the engines door ([settings.md](settings.md#engines-models-and-efforts-per-role),
  [settings-reference.md](settings-reference.md)). Ordered engine chains for
  failover are tracked in [#1326](https://github.com/FieldmouseWorks/burrow/pull/1326).
- **Cross-engine conversation handoff.** When a job's next turn runs on an
  engine without a shared native session,
  [`engine/handoff.rs`](../crates/burrow-proj/src/engine/handoff.rs) already
  builds a handoff prompt carrying prior turns' prompts, bounded answers and
  transcript paths.
- **Capability grants.**
  [`engine/capabilities.rs`](../crates/burrow-proj/src/engine/capabilities.rs)
  resolves what a job is granted (workspace, git metadata, MCP servers, host
  sockets, environment) separately from how an engine renders those grants.
  [`burrow-grants`](../crates/burrow-grants/src/lib.rs) is the one list of
  those capabilities and decides [a plugin's grants](#a-plugins-grants).
- **Front doors.** All GitHub-issue doors (`probe`, `engines`, `undo`,
  `settings`, `inbox`; [front-door.md](front-door.md)) are harness-agnostic,
  and live interactive sessions start on demand rather than through standing
  Claude-only servers.

## A plugin's grants

An engine plugin is granted a capability only when its `describe` declares
it **and** the owner lists it in that engine's
`burrow.agents.engines.NAME.capabilities`. Neither alone grants anything, so
an engine with no list gets nothing.
[`burrow-grants`](../crates/burrow-grants/src/lib.rs) computes this for any
engine, and says for each refusal which capability and why.

Conary's `describe` declares capabilities in this way:

- `network` declares `network`, `mcp` declares `mcp`, and the device `kvm`
  declares `kvm`. Any other device is refused.
- Serving the `job` kind declares the rest of the job contract, which
  `describe` has no field for: `workspace`, `job-state`, `git-metadata`,
  `toolchains`, `system`, `probe`, `units-clear`, `heavy` and `nix`.
- `sandbox` says where the plugin runs and grants nothing.

Never granted, whatever a plugin declares or the owner lists: a GitHub token
or the App's installation authority (`github`, and `environment`, the login
environment that carries them), a write to the settings store, and plugin
installation. What a job is authorized to do that needs those reaches it only
through host-mediated services: proj's jobs socket, the probe, heavy slots
and the MCP servers below.

`mcp` by itself reaches no server. A plugin granted it reaches those of
`burrow.mcpServers` that its engine's
`burrow.agents.engines.NAME.mcpServers` lists, and never one whose
`actsAsOwner` is set, listed or not: the signed-in browser sets it, since
whatever drives that browser is the owner to every site it's signed in to.
A module that adds another such server sets it there. A granted server is
its command, arguments and the `env` the registry sets. Its `envVars` are
never filled for a plugin and its host paths aren't part of the grant. A
refused server is named `mcp:NAME`.

A grant is the capability's name. The runner that gives it to a sandbox
([below](#a-plugins-sandbox)) renders it there, and never copies the host
job's paths or environment for it.

## A plugin's sandbox

`burrow.agents.engines.NAME.plugin` configures an engine's plugin. It runs
only in a microVM of that engine, through `burrow.agents.plugins.NAME`
(installed as `agent-plugin-NAME`): an executable that takes no arguments
and speaks the plugin contract on stdio, so Conary's runner and
`cn plugin check` use it as they would the plugin. It hands the request to
the engine's socket, which only `burrow.user` can open, and the service
behind it ([`burrow-agent`](../crates/burrow-agent/src/plugin.rs)) does the
rest. A call lives as long as its connection: killing the caller ends it.

- `describe` runs with nothing granted: no login, workspace, service or
  network.
- A job's request runs the plugin's describe that way again and takes the
  grants from that answer and the owner's list, never from the caller. Then
  a second VM boots with them, so a job costs two boots, inside its caller's
  deadline.
- Only `describe` and `job` requests run. A plugin that declares
  `sandbox: brings` has its describe run here and its job refused: saying so
  never gets a plugin onto the host.
- The VM's user has root, as in every sandbox, so no guard is inside it.
  What isn't granted isn't attached: no share, forward, device or file. Its
  `/nix/store` holds its image's closure and its granted tools', not the
  host's store. It has no vsock, and no NIC without `network` or a service.
- A job gets its own engine's login (`credentialFile`), and no other's.

What each grant is in the VM:

| Grant | In the VM |
| --- | --- |
| `workspace` | A clone of the project the `workdir` is in, at the same path. The work comes back as `agent fetch` brings it, to `refs/agents/ID/`, and the clone is deleted; one that can't be saved is kept for `agent ls`. |
| `git-metadata` | That clone's `.git`, its configs cut to what names branches and remotes, without logins. Ungranted, it's covered from outside the VM. |
| `job-state` | proj's jobs socket, at `PROJ_HOST_JOBS_SOCKET`. Never the job's directory. |
| `probe` | proj's probe socket, at `PROJ_PROBE_SOCKET`. |
| `units-clear` | The unit watch's socket, at `UNITS_CLEAR_SOCKET`. |
| `heavy` | `heavy -- COMMAND`: the host takes a slot and holds it while the command runs. |
| `system` | The toolbox's everyday tools, git, curl and jq. |
| `toolchains` | The toolbox's languages, with a Cargo and rustup home of the VM's own. |
| `nix` | Nix on a store of the VM's own. Never the host's daemon, whose fixed-output builds would fetch around the proxy. |
| `kvm` | Nested virtualization. |
| `mcp` | `PROJ_MCP_SERVERS`: each approved server as a stdio command. The host runs the server as `burrow.user`. |
| `network` | The engine's filtered proxy and nothing else: the shared list and that engine's hosts. |

A service is carried to the host connection by connection, and the host
connects as `burrow.user`. The jobs and probe sockets are taken only from a
job's scratch, where proj makes them. A grant with nothing behind it in a
call (no such socket, no heavy slots) is logged and left out of
`PROJ_CAPABILITIES`.

The workspace is the project as it is, untracked files included: approving
`workspace` gives the plugin whatever is in that folder. A workdir outside
the projects folder, such as a worktree in a job's scratch, is refused.

## Remaining surfaces to make modular

Swapping in another LLM harness (for example, Antigravity or another agent
CLI) currently hits five places where burrow still assumes Claude Code:

### 1. Closed engine lists and built-in `proj` execution

- **Today:** `burrow_settings::roles::ENGINES`,
  `burrow_proj::engine::Engine`, `modules/proj.nix`'s inbox label enum, and
  `crates/burrow-proj/state.schema.json` enumerate a closed set of built-in
  engines (`claude`, `claude-api`).
  [`engine/command.rs`](../crates/burrow-proj/src/engine/command.rs),
  [`engine/capabilities.rs`](../crates/burrow-proj/src/engine/capabilities.rs)
  (`claude_flags`), and
  [`engine/stream.rs`](../crates/burrow-proj/src/engine/stream.rs) hardcode
  Claude Code's CLI flags, stream JSONL schema, and session extraction.
- **Target:** Replace the closed enum and per-engine branches with the Conary
  `job` plugin contract: each engine is an executable plugin that answers
  `describe` (models, efforts, capabilities, meters), passes
  `cn plugin check`, renders capability grants into its own CLI/env,
  normalizes stream output into `(session, result, error, usage)`, classifies
  refusals, and isolates its own credentials.
- **Tracked by:** [#668](https://github.com/FieldmouseWorks/burrow/issues/668)
  and its children
  [#744](https://github.com/FieldmouseWorks/burrow/issues/744),
  [#745](https://github.com/FieldmouseWorks/burrow/issues/745), planned at
  `proj` stage 8 ([proj-design.md](proj-design.md#the-stages),
  [#983](https://github.com/FieldmouseWorks/burrow/issues/983)).

### 2. Per-engine usage holds, meters and accounting

- **Today:** [`burrow-proj/src/usage.rs`](../crates/burrow-proj/src/usage.rs),
  [`slot/room.rs`](../crates/burrow-proj/src/slot/room.rs), and
  [`snapshot.rs`](../crates/burrow-proj/src/snapshot.rs) model one global
  Claude subscription hold (`proj.usageHold`, see the [settings reference](settings-reference.md)) plus a special-case `api: bool`
  split for `claude-api`.
  [`burrow-probe/src/answer/usage.rs`](../crates/burrow-probe/src/answer/usage.rs)
  parses Claude's stream records directly for token and cost totals.
- **Target:** Key holds, expired-login notices, and meter snapshots by engine
  name so a quota or login refusal on one engine never blocks another engine
  from draining work. Record normalized per-run usage/cost metadata at turn
  end (or from the plugin response) so `burrow-probe` and `burrow-dashboard`
  read one engine-agnostic format.

### 3. Judge producer (`burrow-judge`)

- **Today:** [`burrow-judge/src/look.rs`](../crates/burrow-judge/src/look.rs)
  pins `ENGINE = "claude"`, and
  [`burrow-judge/src/worker.rs`](../crates/burrow-judge/src/worker.rs)
  hardcodes Claude Code's `--restricted`, read-only `--tools`,
  `--json-schema`, output parsing, and OAuth token variable.
- **Target:** Dispatch a look's read-only fresh session through an
  engine-specific worker adapter (or per-engine judge plugin binary) while
  keeping `burrow-judge`'s producer checks, git mirror layout, egress gate,
  session-independence check, and Ed25519 signing unchanged.

### 4. Host guardrails (`burrow-guard`) and microVM sandboxes (`burrow.agents`)

- **Today:** Host sessions rely on `/etc/claude-code/managed-settings.json`
  and Claude Code's `PreToolUse` hook invoking
  [`burrow-guard`](../crates/burrow-guard/src/main.rs). A non-Claude CLI will
  not read those files or fire that hook format. The sandboxes are
  per-engine: [`burrow.agents.engines`](../modules/agents/engines.nix) is a
  table of each engine's guest packages and modules, its systemd credential
  (`agent.credential`, exported as its `credentialEnv`), its own egress
  hosts and an owner-approved capability list. Claude's entry is declared by
  [`modules/claude.nix`](../modules/claude.nix);
  [`modules/agents.nix`](../modules/agents.nix) builds one guest image and
  one filtered proxy per entry, so a VM started for an engine (`agent new
  --engine`) gets only that engine's packages and credential, and reaches
  only [the shared list](../modules/agents/allow.txt) and that engine's
  hosts. Still Claude-shaped there: `agent claude`, the host model settings
  `agent new` copies into a VM's `~/.claude`, and `proj run --vm`, which
  runs Claude in the default engine's VM.
- **Target:**
  - **Sandbox path (default for new engines):**
    [A plugin's sandbox](#a-plugins-sandbox) enforces its grants. proj
    doesn't run engines through it yet
    ([#745](https://github.com/FieldmouseWorks/burrow/issues/745)). Inside
    the microVM, the VM itself is the security boundary.
  - **Host path (when needed):** Keep `burrow-guard`'s ZFS-snapshot and
    one-way-door decision logic engine-agnostic and add a thin hook/policy
    envelope adapter per harness that runs on the host.

### 5. Reviews and other outside services

- **Reviews:** PR reviews currently come from Codex's GitHub connector
  ([Conary flow, Review](https://github.com/FieldmouseWorks/Conary/blob/main/docs/flow.md#review)).
  Conary's plugin contract retains the `review` kind, so switching to a local
  or alternate reviewer later requires only configuring a `review` plugin and
  producer without changing Conary's landing rules.
- **Other external surfaces:** Apply the same single-boundary rule to every
  outside integration—GitHub Apps (`modules/github.nix`), off-site backup
  remotes (`modules/backup.nix`), heartbeats (`modules/heartbeat.nix`), and
  notifiers (`modules/notify.nix`)—so provider changes stay isolated to one
  module or adapter crate.

## Sequencing against active work

Three redesigns are in flight under
[Conary's `m6-order.md`](https://github.com/FieldmouseWorks/Conary/blob/main/docs/m6-order.md):
M6 (Conary landing flow replacing the Python steward), `proj`'s state
redesign ([proj-design.md](proj-design.md)), and the CI gate redesign
([gate-design.md](gate-design.md)). To keep blast radius small while those
land:

1. **Do not add engine support to `modules/autoland/*.py`**—the steward is
   deleted in M6 switch S4, and Conary's flow is already engine-agnostic.
2. **Do not add a third engine as another `Engine` enum branch inside
   `burrow-proj` mid-refactor.** Wait for `proj`'s state/pass stages to
   settle, then cut over to the `conary-plugin` `job` boundary at stage 8
   ([#668](https://github.com/FieldmouseWorks/burrow/issues/668),
   [#983](https://github.com/FieldmouseWorks/burrow/issues/983)).
3. **Land the remaining decoupling as bounded follow-ups:**
   - per-engine holds, login notices, meters, and normalized `probe usage`
     accounting;
   - a conformance check for every engine plugin in
     [its sandbox](#a-plugins-sandbox)
     ([#1391](https://github.com/FieldmouseWorks/burrow/issues/1391));
   - pluggable worker dispatch in `burrow-judge`;
   - host-hook adapters into `burrow-guard` only for harnesses that need host
     execution outside a microVM.

## Done looks like

Adding a new LLM harness (such as Antigravity) to the pipeline requires:

1. One executable plugin/adapter crate passing `cn plugin check` (handling
   launch, stream normalization, refusal classification, and credential
   isolation).
2. One Nix module (or machine flake config) declaring its package, sops
   secret, egress host allowlist, and sandbox/guard wiring.
3. Zero changes to Conary's landing flow, `burrow-settings`, the inbox, the
   scheduler, or the front doors.
