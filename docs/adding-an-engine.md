# Adding a new engine plugin

If you want burrow running on another provider, CLI, or API, you do not need to
modify the scheduler, the inbox, the settings store, or Conary's landing flow.
Every engine sits behind two pieces:

1. A Rust plugin binary (`crates/burrow-engine-<name>`) speaking
   [Conary's `conary-plugin v5` contract](https://github.com/FieldmouseWorks/Conary/blob/main/docs/plugins.md)
   (`job` kind) over stdin/stdout with structured JSONL diagnostics on stderr.
2. A NixOS module (`modules/engines/<name>.nix`) that registers the engine's
   package, isolated credential delivery, egress allowlist, unique proxy port,
   approved capabilities, and wrapped plugin under `burrow.agents.engines.<name>`.

An agent pointed at this repository can build, wire, and verify a new engine
plugin end-to-end by following this guide and the in-tree reference
implementations.

## Reference files to read first

Read these before writing code; they define the exact wire types, Nix surface,
and conformance harness:

- [docs/engines.md](engines.md) — capability grants, sandbox isolation rules,
  and conformance contract (`cn plugin check` + capability probe).
- [crates/burrow-example-plugin/src/lib.rs](../crates/burrow-example-plugin/src/lib.rs) —
  minimal `conary-plugin v5` plugin showing the bare `describe` and `job`
  request/response contract.
- [crates/burrow-engine-antigravity/src/plugin.rs](../crates/burrow-engine-antigravity/src/plugin.rs)
  and [crates/burrow-engine-antigravity/src/normalize.rs](../crates/burrow-engine-antigravity/src/normalize.rs) —
  complete standalone CLI-wrapping engine plugin (config loading, `describe`,
  `Task::Job` execution, stream normalization, refusal classification, isolated
  credential delivery, and outer process-group deadline).
- [crates/burrow-engine-anthropic](../crates/burrow-engine-anthropic) —
  multi-mode plugin supporting both subscription (`claude`) and credit-metered
  API (`claude-api`) execution.
- [modules/engines/antigravity.nix](../modules/engines/antigravity.nix) —
  cleanest template for a standalone Nix engine module.
- [modules/agents/engines.nix](../modules/agents/engines.nix) —
  the `burrow.agents.engines` schema (how each engine gets its own microVM
  image, systemd `agent.credential`, and filtered egress proxy).
- [checks/agents.nix](../checks/agents.nix) — how `lib.engineConformance` tests
  an engine plugin inside its sandbox.

## 1. Implement the plugin crate (`crates/burrow-engine-<name>`)

Add a new workspace crate under `crates/burrow-engine-<name>` (register it in
`crates/Cargo.toml` and expose its binary package in [flake.nix](../flake.nix)).

Your binary reads one JSON `conary_core::plugin::Request` from stdin, writes
one JSON `conary_core::plugin::Response` to stdout, and exits `0` on a valid
response:

- **`Task::Describe`:**
  - Reply with `contract: "conary-plugin v1"`, a stable `by.engine` matching
    the configured engine name, `versions` advertising `"conary-plugin v5"`,
    and `v2_kinds: vec![Kind::Job]`.
  - Declare `capabilities` (`network`, `mcp`, `devices`, and `sandbox`:
    `Sandbox::Needs` for microVM-only harnesses that bypass their own
    permissions, or `Sandbox::None` if safe for host execution).
  - Leave `models` empty by default so model and effort identifiers live in
    runtime `proj settings` rather than tracked files.
  - Declare any usage `meters` (for example `["quota"]`).
- **`Task::Job`:**
  - Read `repo`, `workdir`, `prompt`, `model`, `effort`, and
    `execution.context` (`Fresh` or `Continue { session }`), plus optional
    `PROJ_APPEND_SYSTEM_PROMPT` and `PROJ_STREAM_EVENTS=1` from the
    environment.
  - Deliver credentials strictly from the isolated engine secret (an
    environment variable or a private file inside an isolated `HOME` / config
    directory — never from interactive desktop or host user logins).
  - Enforce an outer wall-clock deadline on the provider's process group so
    hung child processes cannot hold a job slot indefinitely.
  - Return `Answer::Job` with a valid summary, `next` declaration, and
    matching `by` / `execution` / `negotiated` provenance.
- **Structured stderr diagnostics (when `PROJ_STREAM_EVENTS=1`):**
  Emit one JSON object per line on stderr so `proj` tracks live activity,
  session resumption, usage accounting, and per-engine holds without any
  provider-specific parsing in `burrow-proj`:
  - `{"event":"stream",...}` — live output chunks
  - `{"event":"session","session_id":"..."}` — native session ID for
    `Context::Continue`
  - `{"event":"activity"}` — heartbeat while working
  - `{"event":"usage","session":"...","usage":{...}}` — token/cost accounting
  - `{"event":"refusal","kind":"usage_limit"|"login_expired"|"api_key_rejected"|"budget_exhausted"|"credit_exhausted","reason":"...","resets_at":...}`
  - `{"event":"error","message":"..."}`

## 2. Add the NixOS module (`modules/engines/<name>.nix`)

Copy the structure of [modules/engines/antigravity.nix](../modules/engines/antigravity.nix):

- Declare `options.burrow.engines.<name>` with:
  - `enable`, `package` / `binary`, `extraPackages`, `guestModules`
  - Isolated credential options (`credentialFile`, `credentialEnv`, and any
    `home-file` delivery path needed by the provider CLI)
  - `hosts` — tinyproxy fnmatch allowlist for this engine's API endpoints alone
  - `proxyPort` — fixed loopback port that does not collide with existing
    engine proxies (`3128` Claude, `3129` open proxy, `3135` Antigravity)
  - `models` / `defaultModel` / `defaultEffort` (defaulting to empty / `null`
    so runtime settings own model selection)
  - `capabilities`, `meters`, `deadlineSeconds`, and read-only `pluginPackage`
- Wrap the crate binary with `makeBinaryWrapper` pointing at its generated
  config file and assign `burrow.agents.engines.<name>` when enabled.
- Register the module in [flake.nix](../flake.nix) (`moduleFiles`, `names`, and
  its standalone evaluation check in `standalone-<name>`).

## 3. Add the conformance check (`checks/agents.nix`)

Wire a `conformance-<name>` entry in [checks/agents.nix](../checks/agents.nix)
using `self.lib.engineConformance` with a stand-in CLI binary and placeholder
config (never real credentials or real model IDs). This boots a microVM and
proves both `cn plugin check` and capability isolation for your plugin.

## 4. Enable it on a machine flake and route roles

In your machine flake (for example `remi`):

1. Package or pin the provider's CLI if it is not already in `nixpkgs`.
2. Store an isolated engine credential in sops-nix and gate enablement on the
   secret file's presence so secretless builds stay green.
3. Set `burrow.engines.<name>` (`enable`, `package`, `credentialFile`,
   `hosts`) without hard-coding model names in tracked files.
4. Configure per-engine model/effort offers and per-role routing live through
   `proj settings` and save a named setup (`proj engines save NAME`,
   `proj engines use NAME`; see [docs/settings.md](settings.md) and
   [docs/settings-reference.md](settings-reference.md)).

## 5. Verify before opening a PR

Run locally before pushing:

```sh
cargo fmt --manifest-path crates/Cargo.toml --all -- --check
cargo clippy --manifest-path crates/Cargo.toml -p burrow-engine-<name> --all-targets -- -D warnings
cargo test --manifest-path crates/Cargo.toml -p burrow-engine-<name>
nix build --no-link .#checks.x86_64-linux.standalone-<name>
nix build --no-link .#checks.x86_64-linux.agents.entries.conformance-<name>   # needs /dev/kvm
nix build --no-link .#checks.x86_64-linux.guidance
```

## Non-negotiable rules

- **No model names in tracked files.** Live `proj settings` chooses models and
  efforts; CI's `house/model-names` check rejects model identifiers in tracked
  code and docs.
- **Strict credential and egress isolation.** Never reuse a desktop or host
  session login, never commit plaintext secrets, and allowlist only the
  provider's API hosts on the engine's own `proxyPort`.
- **Never grant forbidden capabilities.** A plugin is never granted `github`,
  `environment`, settings-store writes, plugin installation, or `actsAsOwner`
  MCP servers ([docs/engines.md](engines.md#a-plugins-grants)).
- **No provider logic in core crates.** Keep provider-specific flags, stream
  parsing, and refusal heuristics inside `crates/burrow-engine-<name>`, never
  in `burrow-proj`, `burrow-settings`, or Conary.
- **Role coverage status.** Project jobs, sandboxes, and live per-role routing
  already target modular plugins (`#745` replaces the last built-in `proj`
  enum); moving `burrow-judge`, live plugin installation (`#668`), and PR
  review (currently Codex) onto the same swappable plugin contract is in
  progress.
