# The house style

What every repo shares beyond the [repo baseline](repo-baseline.md): the same
rules for agents and the same commit style. Each piece has one home, so a
change is made once and reaches every repo.

| Layer | Home | What's in it |
|---|---|---|
| The machine | `burrow.claude` | Permissions, the guard, the one-way doors, and which model does what. Every Claude on the box gets it; nothing in a repo repeats it. |
| Every repo | [`house/rules.md`](../house/rules.md), copied into AGENTS.md | The house rules and the commit style. It lives in the repo because Codex, cloud sessions and outside contributors never see the machine. |
| Every code repo | [`lib/project.nix`](../lib/project.nix), through its `flake.nix` | The toolchains, and the gate: format, lint and tests in `nix flake check`. CI only calls it. |
| Each project | the repo itself | Its domain rules, its own review risks, how to build and test it, its verification tools, ADRs, status docs and deploys. |

## The house block

[`house/rules.md`](../house/rules.md) is the source. Every repo's AGENTS.md
carries a copy of it, **verbatim, markers included**, from
`<!-- house:rules v1 begin ... -->` to `<!-- house:rules end -->`. The house
sync (`burrow.house`, coming next) finds the block by those markers and
rewrites it when this file changes, so:

- Copy it byte for byte. Don't reword, reflow or trim it, and keep both marker
  lines exactly as they are.
- Put it straight after the AGENTS.md's title and opening paragraph, before the
  repo's own sections.
- Anything the repo needs on top goes outside the markers, in its own sections.
  A repo-specific rule that's stricter than a house rule wins; say so where you
  write it.
- When a repo's AGENTS.md already says the same thing as a house rule, delete
  the repo's version in the same change: one fact, one home.
- A repo that keeps things private (keys, personal data, a ROM and the assets
  extracted from it) lists them under `## Private` in its AGENTS.md. The house
  rules point at that section.
- To change a house rule, change `house/rules.md` here. Edits inside the
  markers in a repo are overwritten.

## The gate is the flake

Every repo with code gets a `flake.nix` from the template
(`nix flake init -t github:FieldmouseWorks/burrow`), a few lines calling
[`burrow.lib.project`](../lib/project.nix) with the languages it has:

- `nix develop` gives the toolchains: Rust as `rust-toolchain.toml` pins it
  (with rust-analyzer), Python with ruff and uv, Node's LTS.
- `nix flake check` is the gate, the same on a laptop, in an agent's sandbox
  and in CI: rustfmt, clippy on all targets with warnings as errors, and the
  tests of the Cargo workspace, built as `cargo test` builds them (debug
  assertions on); ruff's error rules on Python (and `ruff
  format`, once a repo has been formatted).
- CI is `.github/workflows/flake.yml` from the template, which calls burrow's
  shared [`flake-check`](../.github/workflows/flake-check.yml) workflow
  (`runner` and `install-nix: false` put it on a self-hosted NixOS runner).
  On a public repo it also attests the gate's result for Conary's ledger, so
  the caller's job grants it `id-token: write` and `attestations: write`. A
  repo's heavier gates (VM replays, browser tiers, databases) stay its own
  jobs, alongside it, and run their tools from `nix develop`.
- Renovate keeps `flake.lock` current, so a repo's toolchains and gate move
  when burrow's do. Change a Rust version in `rust-toolchain.toml` only.
- The template also brings `.editorconfig`. Rust crates forbid `unsafe` in
  `[workspace.lints]` (or `[lints]`) unless the repo says why it needs it.

Bringing a repo onto it: keep the checks it already runs, move the ones that
are hermetic into the flake (its `checks` argument takes more), and delete
the CI steps the flake now covers, in the same PR.

## CLAUDE.md

One line, `@AGENTS.md`, then only what Claude alone needs (usually nothing).
Personal or machine-local notes, model choices included, go in an untracked
`CLAUDE.local.md`.

## Models

Repos never name a model. Model names go stale every few weeks and differ by
machine, so which model does what is set on the machine, not in a tracked file.

## Commits

`area: plain sentence`, as in the house rules. From now on, not by rewriting
history. DCO sign-off only where a repo takes outside contributions and its
CONTRIBUTING asks for it.

## What a repo keeps for itself

- Its domain contracts, and its own lines under the review rules.
- Its verification runner and gates. They share the house rules' vocabulary,
  not code.
- ADRs, status docs, PR and issue templates, CONTRIBUTING.
- Its license.
