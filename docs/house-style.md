# The house style

What every repo shares beyond the [repo baseline](repo-baseline.md): the same
rules for agents and the same commit style. Each piece has one home, so a
change is made once and reaches every repo.

| Layer | Home | What's in it |
|---|---|---|
| The machine | `burrow.claude` | Permissions, the guard, the one-way doors, and which model does what. Every Claude on the box gets it; nothing in a repo repeats it. |
| Every repo | The blocks in `house/`, copied into AGENTS.md | The house rules and the commit style ([`house/rules.md`](../house/rules.md)), and how a PR lands ([`house/prs.md`](../house/prs.md)). They live in the repo because Codex, cloud sessions and outside contributors never see the machine. |
| Every code repo | [`lib/project.nix`](../lib/project.nix), through its `flake.nix` | The toolchains, and the gate: format, lint and tests in `nix flake check`. CI only calls it. |
| Each project | the repo itself | Its domain rules, its own review risks, how to build and test it, its verification tools, ADRs, status docs and deploys. |

## The house blocks

Each file in `house/` is the source of one block:
[`house/rules.md`](../house/rules.md), the house rules, and
[`house/prs.md`](../house/prs.md), the Pull requests section. Every repo's
AGENTS.md carries a copy of each, **verbatim, markers included**, from
`<!-- house:NAME v1 begin ... -->` to `<!-- house:NAME end -->`. The house
sync (`burrow.house`) finds each block by its markers and rewrites it when its
file here changes, so:

- Copy it byte for byte. Don't reword, reflow or trim it, and keep both marker
  lines exactly as they are.
- Put the rules block straight after the AGENTS.md's title and opening
  paragraph, before the repo's own sections, and the Pull requests block just
  before the repo's Code Review Rules.
- Anything the repo needs on top goes outside the markers: in its own
  sections, or straight after the block it adds to (a path whose PRs wait for
  the owner goes after the Pull requests block). A repo-specific rule that's
  stricter than a house rule wins; say so where you write it.
- When a repo's AGENTS.md already says the same thing as a house rule, delete
  the repo's version in the same change: one fact, one home.
- A repo that keeps things private (keys, personal data, a ROM and the assets
  extracted from it) lists them under `## Private` in its AGENTS.md. The house
  rules point at that section.
- To change a block, change its file in `house/` here. Edits inside the
  markers in a repo are overwritten.
- A new block is a new file in `house/`, named for the block, with its marker
  lines first and last. A repo without a block yet is left alone, so each
  repo's copy goes in by hand once; the house sync keeps it from then on.

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
  format`, once a repo has been formatted). `flake-gate` runs it on a machine
  exactly as the repo's CI does, the workflow's own gate step with the
  inputs the repo gives it (such as `eval-apart`, for a flake too big to
  evaluate at once).
- CI is `.github/workflows/flake.yml` from the template, which calls burrow's
  shared [`flake-check`](../.github/workflows/flake-check.yml) workflow. A
  public repo runs it on GitHub's hosted runners, which cost it nothing and
  have KVM for NixOS VM tests. A private repo pays for hosted minutes, so it
  runs it on a self-hosted NixOS runner (`runner` and `install-nix: false`).
  On a public repo it also attests the gate's result for Conary's ledger, so
  the caller's job grants it `id-token: write` and `attestations: write`. On
  a PR, its `commits` job fails any commit GitHub can't verify: a signed one
  under a name and email its key isn't for (the house rules' Commits). A
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
