# The house action's callers, burrow's and the template's: each runs
# it on every push and whenever the title or body is edited, with a
# token that can post its statuses, and the action hands the check
# the event's PR and head. The check itself is crates/burrow-house,
# gated as crates-*.
{ pkgs, ... }:
pkgs.runCommand "house-callers-check" { nativeBuildInputs = [ pkgs.yq-go ]; } ''
  for caller in ${../.github/workflows/house.yml} ${../templates/project/.github/workflows/house.yml}; do
    yq -e '.on.pull_request.types | sort | join(",") == "edited,opened,reopened,synchronize"' "$caller"
    yq -e '.jobs.house.permissions | to_entries | map(.key + "=" + .value) | join(",") == "contents=read,pull-requests=read,statuses=write"' "$caller"
    yq -e '.jobs.house.steps[-1].uses | test("^(\./|FieldmouseWorks/burrow/)\.github/actions/house(@|$)")' "$caller"
  done
  action=${../.github/actions/house/action.yml}
  # Built where rustup finds the crates' pinned toolchain.
  yq -e '.runs.steps[0]."working-directory" == "''${{ github.action_path }}/../../../crates"' "$action"
  yq -e '.runs.steps[-1].env.NUMBER == "''${{ github.event.pull_request.number }}"' "$action"
  yq -e '.runs.steps[-1].env.HEAD_SHA == "''${{ github.event.pull_request.head.sha }}"' "$action"
  touch $out
''
