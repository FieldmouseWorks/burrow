# Guidance checks in burrow

The callers in [flake.yml](../.github/workflows/flake.yml) and
[flake-retarget.yml](../.github/workflows/flake-retarget.yml) declare the same
`docs-paths` and `docs-checks`. Guidance means the root README, AGENTS and
CLAUDE files, Markdown under `docs/`, and the top-level Markdown house blocks.
A PR changing only these takes the shared workflow's existing docs lane.
Mixed changes, non-Markdown files, templates, workflows, Nix and lockfiles
keep the full gate. Deletions and both ends of a rename count too.

## Consumers and checks

These inputs are packaged by builds; they aren't all inert README files.
This inventory is why burrow declares checks rather than an empty docs job:

| Consumer | Input and behavior | Check kept for the docs lane |
|---|---|---|
| [house module](../modules/house.nix) | Packages `house/` in the sync config and `/etc/burrow/house`; changed blocks trigger sync after a switch | `house-sync-config` reads the example's generated config; `crates-rust-test` runs the sync command, rewrite, version, source-repo and API fixtures |
| [house-sync](../crates/house-sync/src/blocks.rs) | Reads each top-level `house/*.md`, hashes its bytes and rewrites copies | `guidance` validates each source's complete markers and every verbatim AGENTS copy; `crates-rust-test` retains block/model-name and sync fixtures |
| [baseline bundle](../modules/autoland.nix) (`baselineDocs`) | Carries `docs/`, `house/`, templates and lib together for baseline jobs to read; Markdown changes change the bundle's store path, not executable logic | `guidance-links` checks relative links and fragments offline, against the whole tree; `guidance` checks text |
| [house action](../.github/actions/house/action.yml) | Reads the PR diff, its guide sizes, title and body | Continues to run independently on every PR and body edit, including `house/guide-size`; `crates-rust-test` runs its guide-growth, exemption and other house fixtures |
| Agents | Read AGENTS and CLAUDE, and follow links into docs and house blocks | `guidance` checks the CLAUDE import, valid UTF-8, LF lines, final newlines and conflict markers; `guidance-links` checks local destinations |
| [secrets scanner](../checks/secrets.nix) | Reads every tracked text file, guidance included | `secrets`, unchanged |

The `tests/house/` package fixtures consume their own sources and
`lib/project.nix`, not `house/*.md` or `docs/*.md`. They remain in the full
gate; the Rust house and sync fixtures that do read guidance remain in
`crates-rust-test`. That declaration deliberately keeps the whole Rust test
check, so another Rust guidance consumer cannot silently lose its tests.
Formatting and lint checks compile Rust or inspect code, not Markdown
contents. VM checks of unrelated services need not rebuild solely because a
baseline bundle's store path changed. A change to any consumer's code or
Nix configuration still gets the full gate.

`guidance` requires this repo's AGENTS copy to change with its source block.
It uses sync's existing marker and rewrite code, not a second interpretation
of house blocks. Guide size and the permitted PR-body reason stay owned by
the house action; the docs lane does not replace that status with a stricter
absolute limit. External links are not fetched by the offline link check.

## Regression coverage

The [lane harness](../crates/burrow-workflows/src/lane.rs) runs the shared
workflow's own classifier on real Git repos, including the original caller
as a negative control for a `house/rules.md` edit. It covers guidance edits,
deletions, renames and mixed changes, and executes the docs job's build
script with a recorded Nix to assert exactly the declared checks run.
[Guidance fixtures](../crates/burrow-house/tests/guidance.rs) reject malformed
source markers even when their local copy matches, missing, stale and
duplicate copies, broken imports and invalid text. The link checker scans
the whole guidance set, so a removed or renamed target fails incoming links
unless those links change with it.
