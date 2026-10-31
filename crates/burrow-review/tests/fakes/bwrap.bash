# A stand-in for bubblewrap, as burrow-review runs it (the tests put a bash
# shebang on top). It records how the sandbox was set up in
# $HOME/.fake/box.log, one JSON line a look: its arguments before `--`, its
# binds as [flag, source, destination], and the git config bound over
# HOME/.gitconfig. Then it runs the command where the sandbox would start
# it: in the directory bound at --chdir.
set -euo pipefail
a=("$@")
cut=0
while [ "${a[cut]}" != -- ]; do
  cut=$((cut + 1))
done
binds='[]'
git='[]'
for ((i = 0; i < cut; i++)); do
  case ${a[i]} in
  --bind | --ro-bind | --ro-bind-try)
    binds=$(jq -c --arg k "${a[i]}" --arg s "${a[i + 1]}" --arg d "${a[i + 2]}" '. + [[$k, $s, $d]]' <<<"$binds")
    case ${a[i + 2]} in
    */.gitconfig) git=$(jq -c --rawfile t "${a[i + 1]}" '. + [$t]' <<<"$git") ;;
    esac
    ;;
  esac
done
jq -cn --argjson binds "$binds" --argjson git "$git" '{args: $ARGS.positional, binds: $binds, gitconfig: $git}' \
  --args -- "${a[@]:0:cut}" >>"$HOME/.fake/box.log"
where=
for ((i = 0; i < cut; i++)); do
  [ "${a[i]}" != --chdir ] || where=${a[i + 1]}
done
start=$where
for ((i = 0; i < cut; i++)); do
  case ${a[i]} in
  --bind | --ro-bind) [ "${a[i + 2]}" != "$where" ] || start=${a[i + 1]} ;;
  esac
done
cd "$start"
exec "${a[@]:cut+1}"
