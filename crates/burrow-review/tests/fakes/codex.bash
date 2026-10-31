# A stand-in for the codex CLI, as burrow-review runs it (the tests put a
# bash shebang on top). It records how it was run in $HOME/.fake/codex.log,
# one JSON line a run, then does what $HOME/.fake/codex.json says (a
# value can be {"text in the prompt": value}, for the look whose prompt has it):
#   wait     seconds, before it refreshes
#   refresh  a refreshed login, written in place, as codex writes one
#   sleep    seconds
#   http     [{"env": VAR, "send": TEXT}]: TEXT sent to the proxy VAR names,
#            what came back recorded in $HOME/.fake/http.log
#   events   JSON events printed ahead of an error
#   rc, err  exit with rc, err on stderr
#   say      its last message (else "Looked." and `answer`)
#   gone     its PR closes meanwhile: the sweep marks its session gone
#   relogin  a refreshed login, saved as a new file over its copy
set -euo pipefail
fake=$HOME/.fake
todo=$(cat "$fake/codex.json" 2>/dev/null || echo '{}')
args=("$@")
prompt=$(cat; printf x)
prompt=${prompt%x}

# Its session: the one it's told to resume (an unknown one is an error, as
# codex has it), or a new one, kept in its home.
sid=
resumed=false
if [ "${args[0]:-}" = exec ] && [ "${args[1]:-}" = resume ]; then
  sid=${args[-2]}
  resumed=true
  if [ ! -e "$CODEX_HOME/sessions/$sid" ]; then
    echo "no session $sid" >&2
    exit 1
  fi
fi
[ -n "$sid" ] || sid=session-$(date +%s%N)
mkdir -p "$CODEX_HOME/sessions"
printf '%s' "$prompt" >>"$CODEX_HOME/sessions/$sid"
jq -cn --arg cwd "$PWD" --arg prompt "$prompt" --arg sid "$sid" --argjson resumed "$resumed" \
  --arg began "$(date +%s.%N)" \
  '{args: $ARGS.positional, cwd: $cwd, env: env, prompt: $prompt, session: $sid,
    resumed: $resumed, began: ($began | tonumber)}' --args -- "${args[@]}" >>"$fake/codex.log"

# What codex.json says to do, for this look's prompt: $2 if it says nothing.
todo() {
  jq -r --arg p "$prompt" --arg k "$1" --arg d "$2" '.[$k] // $d | if type == "object" then
    ([to_entries[] | select(.key as $k | $p | contains($k)) | .value] | first // $d) else . end' <<<"$todo"
}

sleep "$(todo wait 0)"
login=$(todo refresh "")
[ -z "$login" ] || printf '%s' "$login" >"$CODEX_HOME/auth.json"
sleep "$(todo sleep 0)"

n=$(jq '.http // [] | length' <<<"$todo")
for ((i = 0; i < n; i++)); do
  var=$(jq -r ".http[$i].env" <<<"$todo")
  at=${!var#http://}
  at=${at%%/*}
  exec 3<>"/dev/tcp/${at%:*}/${at##*:}"
  jq -j ".http[$i].send" <<<"$todo" >&3
  got=$(cat <&3; printf x)
  exec 3<&-
  jq -cn --arg got "${got%x}" '$got' >>"$fake/http.log"
done

jq -c '.events // [] | .[]' <<<"$todo"
rc=$(jq '.rc // 0' <<<"$todo")
if [ "$rc" != 0 ]; then
  jq -r '.err' <<<"$todo" >&2
  exit "$rc"
fi
out=$(jq -r '.say // ("Looked.\n" + (.answer // {findings: [], summary: "fine"} | tojson))' <<<"$todo"; printf x)
out=${out%x}
home=$CODEX_HOME
if [ "$(jq '.gone // false' <<<"$todo")" = true ]; then
  session=$(dirname "$home")
  gone=$(dirname "$session")/.gone-0-$(basename "$session")
  mv "$session" "$gone"
  home=$gone/home # where the sandbox's bind still writes
fi
if [ "$(jq 'has("relogin")' <<<"$todo")" = true ]; then
  rm "$home/auth.json"
  jq -j '.relogin' <<<"$todo" >"$home/auth.json"
fi
for ((i = 0; i < ${#args[@]}; i++)); do
  if [ "${args[i]}" = -o ]; then
    printf '%s' "$out" >"${args[i + 1]}" # codex writes its last message there
  fi
done
# --json: events, one a line, the session's id among them.
jq -cn --arg sid "$sid" '{type: "thread.started", thread_id: $sid}'
echo '{"type":"turn.completed"}'
