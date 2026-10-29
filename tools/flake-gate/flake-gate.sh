# flake-gate: a repo's CI gate, run here. It runs the gate step of the shared
# flake-check workflow the repo's .github/workflows/flake.yml calls, that
# step's own script, with the inputs the repo gives it (eval-apart,
# build-skip), as GitHub runs it. The workflow stays the gate's one home.
#
#   flake-gate           run it in the repo this is in
#   flake-gate --print   print the inputs and the script, and run nothing
print=
case ${1-} in
  --print) print=1 ;;
  "") ;;
  *) echo "usage: flake-gate [--print]" >&2; exit 2 ;;
esac

root=$(git rev-parse --show-toplevel)
cd "$root" || exit
caller=.github/workflows/flake.yml
[ -f "$caller" ] || { echo "flake-gate: no $caller in $root" >&2; exit 1; }

# The one job that calls flake-check.yml, and where that workflow is.
jobs=$(yq -o=json '[.jobs | to_entries[] | select(.value.uses // "" | test("(^|/)[.]github/workflows/flake-check[.]yml(@|$)"))]' "$caller")
[ "$(jq length <<<"$jobs")" = 1 ] || { echo "flake-gate: $caller has $(jq length <<<"$jobs") jobs calling flake-check.yml, not one" >&2; exit 1; }
uses=$(jq -r '.[0].value.uses' <<<"$jobs")
with=$(jq -c '.[0].value.with // {}' <<<"$jobs")

workflow=$(mktemp)
tmp=$(mktemp -d)
trap 'rm -rf -- "$workflow" "$tmp"' EXIT
case $uses in
  ./*) cp -- "${uses#./}" "$workflow" ;;
  */*/*@*)
    # OWNER/REPO/PATH@REF, at the branch, tag or commit the caller names,
    # fetched as git reads any GitHub repo: no token needed for a public one
    # (an agent sandbox has none), and git's own login for a private one.
    repo=$(cut -d/ -f1-2 <<<"${uses%@*}")
    path=$(cut -d/ -f3- <<<"${uses%@*}")
    git init -q "$tmp/workflow-repo"
    git -C "$tmp/workflow-repo" fetch -q --depth=1 "https://github.com/$repo" "${uses##*@}"
    git -C "$tmp/workflow-repo" show "FETCH_HEAD:$path" > "$workflow" ;;
  *) echo "flake-gate: can't fetch $uses" >&2; exit 1 ;;
esac

step=$(yq -o=json '[.jobs.check.steps[] | select(.id == "gate")]' "$workflow")
[ "$(jq length <<<"$step")" = 1 ] || { echo "flake-gate: $uses has no one step with id gate" >&2; exit 1; }
[ "$(jq -r '.[0].shell' <<<"$step")" = bash ] || { echo "flake-gate: $uses's gate step isn't shell: bash" >&2; exit 1; }
inputs=$(yq -o=json '.on.workflow_call.inputs // {}' "$workflow")

# The step's env, each an input as GitHub gives it: the caller's value, or
# the input's default, or its type's (false, 0, empty), each as a string. Anything else this can't
# mirror, so it stops rather than run a different gate.
env=$(jq -c --argjson with "$with" --argjson inputs "$inputs" '
  .[0].env // {} | to_entries | map(
    [.value | capture("^[$][{][{] *inputs[.](?<i>[A-Za-z0-9_-]+) *[}][}]$")?.i][0] as $i
    | if $i == null then error("env \(.key) is \(.value | tojson), not one input")
      elif ($inputs | has($i) | not) then error("env \(.key) is input \($i), which the workflow doesn'"'"'t have")
      else {key, value: (if $with | has($i) then $with[$i]
        elif $inputs[$i] | has("default") then $inputs[$i].default
        else {boolean: "false", number: "0"}[$inputs[$i].type] // "" end | tostring)}
      end) | from_entries' <<<"$step") || { echo "flake-gate: can't run $uses's gate step as GitHub would" >&2; exit 1; }
jq -r '.[0].run' <<<"$step" > "$tmp/gate.sh"

if [ -n "$print" ]; then
  echo "workflow: $uses"
  jq -r 'to_entries[] | "env: \(.key)=\(.value)"' <<<"$env"
  cat "$tmp/gate.sh"
  exit 0
fi

mkdir "$tmp/runner"
args=()
while IFS= read -r -d '' kv; do args+=("$kv"); done < <(jq -j 'to_entries[] | "\(.key)=\(.value)\u0000"' <<<"$env")
# GitHub runs shell: bash as bash --noprofile --norc -eo pipefail.
env "${args[@]}" RUNNER_TEMP="$tmp/runner" bash --noprofile --norc -eo pipefail "$tmp/gate.sh"
