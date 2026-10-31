# flake-gate (tools/flake-gate): a repo's CI gate step, run as its CI runs it,
# with GitHub stood in for. And burrow's own gate, as flake-gate runs it here and
# CI runs it there: the shared workflow's own script, evaluating apart.
#
#   bash tests/flake-gate.sh BURROW-SOURCE   (flake-gate, yq and git on PATH)
# GitHub expressions are written as they are, unexpanded.
# shellcheck disable=SC2016
set -euo pipefail
burrow=$1
tmp=$(mktemp -d)
export HOME=$tmp
git() { command git -c user.name=gate -c user.email=gate@example.org -c init.defaultBranch=main "$@"; }
fail() { echo "FAILED: $*"; exit 1; }

# A repo with these workflows (a caller's flake.yml and, given, a local
# flake-check.yml), committed.
repo() {
  local d
  d=$(mktemp -d "$tmp/repo.XXXXXX")
  mkdir -p "$d/.github/workflows" "$d/sub"
  printf '%s\n' "$1" > "$d/.github/workflows/flake.yml"
  [ -z "${2-}" ] || printf '%s\n' "$2" > "$d/.github/workflows/flake-check.yml"
  git -C "$d" init -q && git -C "$d" add . && git -C "$d" commit -qm workflows
  echo "$d"
}
shared() {
  cat <<WF
on:
  workflow_call:
    inputs:
      build-skip: { type: string, default: '' }
      eval-apart: { type: boolean, default: false }
      runner: { type: string, default: ubuntu-latest }
      quiet: { type: boolean }
jobs:
  check:
    steps:
      - uses: actions/checkout@0000000000000000000000000000000000000000
      - id: gate
        shell: bash
        env:
          SKIP: \${{ inputs.build-skip }}
          APART: \${{inputs.eval-apart}}
          QUIET: \${{ inputs.quiet }}
${1-}
        run: |
          echo "\$APART|\$SKIP|\$QUIET|\$PWD" > "\$GATE_OUT"
          test -d "\$RUNNER_TEMP"
${2-}
WF
}
caller() {
  printf 'jobs:\n  check:\n    uses: %s\n%s\n' "$1" "${2-}"
}

# Run in a subdirectory: the gate runs at the top, with the caller's inputs
# or the workflow's defaults, as GitHub gives them.
d=$(repo "$(caller ./.github/workflows/flake-check.yml '    with: { eval-apart: true, build-skip: "vm x" }')" "$(shared)")
(cd "$d/sub" && GATE_OUT=$tmp/out flake-gate) || fail "a gate that passes failed"
[ "$(cat "$tmp/out")" = "true|vm x|false|$d" ] || fail "ran with $(cat "$tmp/out")"
d=$(repo "$(caller ./.github/workflows/flake-check.yml)" "$(shared)")
(cd "$d" && GATE_OUT=$tmp/out flake-gate) || fail "a gate that passes failed"
[ "$(cat "$tmp/out")" = "false||false|$d" ] || fail "ran with defaults $(cat "$tmp/out")"

# GitHub's shell: bash is -eo pipefail, so a failed command anywhere in a
# pipeline fails the gate.
d=$(repo "$(caller ./.github/workflows/flake-check.yml)" "$(shared "" '          false | true')")
if (cd "$d" && GATE_OUT=$tmp/out flake-gate); then fail "a pipeline that failed passed"; fi

# The workflow another repo names, at the branch, tag or commit it names,
# through git with no login (a stand-in for GitHub, in place of its URL).
remote=$(mktemp -d "$tmp/remote.XXXXXX")
mkdir -p "$remote/.github/workflows"
shared > "$remote/.github/workflows/flake-check.yml"
git -C "$remote" init -q && git -C "$remote" add . && git -C "$remote" commit -qm v1 && git -C "$remote" tag v1
pinned=$(git -C "$remote" rev-parse HEAD)
# main moves on, with a gate that fails, so only the named ref's passes.
shared "" '          false' > "$remote/.github/workflows/flake-check.yml"
git -C "$remote" commit -qam v2
git -C "$remote" config uploadpack.allowAnySHA1InWant true
git config --global url."file://$remote".insteadOf https://github.com/o/burrow
for ref in "$pinned" v1; do
  d=$(repo "$(caller "o/burrow/.github/workflows/flake-check.yml@$ref" '    with: { build-skip: vm }')")
  (cd "$d" && GATE_OUT=$tmp/out flake-gate) || fail "a workflow named by $ref failed"
  [ "$(cat "$tmp/out")" = "false|vm|false|$d" ] || fail "ran $ref with $(cat "$tmp/out")"
  (cd "$d" && flake-gate --print) | grep -x 'env: SKIP=vm' > /dev/null || fail "printed $ref wrong"
done
d=$(repo "$(caller o/burrow/.github/workflows/flake-check.yml@main)")
if (cd "$d" && GATE_OUT=$tmp/out flake-gate); then fail "ran another ref's gate than main's"; fi

# What it can't run as GitHub would, it refuses, and runs nothing.
refused() {
  rm -f "$tmp/out"
  if (cd "$1" && GATE_OUT=$tmp/out flake-gate 2> "$tmp/err"); then fail "ran $2"; fi
  [ ! -e "$tmp/out" ] || fail "ran some of $2"
  grep -q "$3" "$tmp/err" || fail "$2: $(cat "$tmp/err")"
}
refused "$(repo "$(caller ./.github/workflows/flake-check.yml)" "$(shared '          EVENT: ${{ github.event_name }}')")" \
  "an env that isn't an input" 'env EVENT is'
refused "$(repo "$(caller ./.github/workflows/flake-check.yml)" "$(shared '          NONE: ${{ inputs.none }}')")" \
  "an input the workflow doesn't have" "input none, which the workflow doesn't have"
refused "$(repo "$(caller ./.github/workflows/flake-check.yml)" "$(shared | sed 's/shell: bash/shell: sh/')")" \
  "a step that isn't bash" "isn't shell: bash"
refused "$(repo "$(caller ./.github/workflows/flake-check.yml)
  again:
    uses: ./.github/workflows/flake-check.yml" "$(shared)")" "two gates" "2 jobs calling flake-check.yml"

# burrow's own: CI's inputs, and the shared workflow's gate step as it is.
d=$(mktemp -d "$tmp/burrow.XXXXXX")
cp -r "$burrow/.github" "$d/" && chmod -R u+w "$d"
git -C "$d" init -q && git -C "$d" add . && git -C "$d" commit -qm burrow
(cd "$d" && flake-gate --print) > "$tmp/print"
yq '.jobs.check.steps[] | select(.id == "gate") | .run' "$d/.github/workflows/flake-check.yml" > "$tmp/step"
diff <(tail -n +4 "$tmp/print") "$tmp/step" || fail "flake-gate runs another script than CI's gate step"
# The whole flake at once holds 6-7G until its last build ends, beside every
# other job on remi; one output at a time holds a fraction of that.
grep -qx 'env: APART=true' "$tmp/print" || fail "burrow's gate evaluates the whole flake at once: $(head -3 "$tmp/print")"
grep -qx 'env: SKIP=agents' "$tmp/print" || fail "burrow's gate skips $(grep SKIP "$tmp/print")"
echo "flake-gate: ok"
