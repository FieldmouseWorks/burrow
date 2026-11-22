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
- **Front doors.** All GitHub-issue doors (`probe`, `engines`, `undo`,
  `settings`, `inbox`; [front-door.md](front-door.md)) are harness-agnostic,
  and live interactive sessions start on demand rather than through standing
  Claude-only servers.

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
  - **Sandbox path (default for new engines):** Enforce plugin capability
    grants inside the shared sandbox, from each engine's `capabilities`
    ([#743](https://github.com/FieldmouseWorks/burrow/issues/743)). Inside
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
   - capability grants in `burrow.agents`' per-engine sandboxes
     ([#743](https://github.com/FieldmouseWorks/burrow/issues/743));
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
