# The stand-ins ../proj.nix runs proj against: Claude, and GitHub as gh
# answers it.
{ pkgs }:
{
  # Claude, as far as proj can tell: reads the prompt, answers in stream-json.
  # "sleep N" sleeps; "eat" runs something that holds far more memory than
  # the test's job cap, then fails; "outgrow" does, then answers; "bloat"
  # grows past the cap itself, as Claude can; told any of those was killed,
  # it answers, or with /tmp/again grows past it again;
  # "twice" answers twice in one turn, "answerfail" answers and then fails;
  # "linger N" answers, then stays up N seconds, as Claude does while
  # something it left in the background (a CI watch, a long build) is still
  # going; "detach N" does too, having left work in its own session that
  # ignores TERM and writes to the stream. Told so, it answers again, or as
  # /tmp/cut-mode says: "linger" answers and stays up, "hang" says nothing,
  # "crash" and "signal" die without a word, "error" fails, "resumed" puts
  # out the empty result a resumed Claude Code can before it starts, then
  # works past proj's ten seconds and answers;
  # "chatter N" says something every second for N seconds, then answers;
  # "named N" names its session ("named") as Claude's first line does, then
  # sleeps N seconds without answering, as an issue's "inbox-hang" does
  # ("hang") for good, resumed putting out an empty result first; told it
  # was cut off, it answers, or with /tmp/again hangs again. "eat" and
  # "bloat" name theirs too.
  # "scratch" makes a worktree of web, with a file in it, in its scratch and
  # says where that is in /tmp/scratch; "scratch ask" then asks.
  # As a job, it says what comes next before it answers (proj done; "ask"
  # asks, "await" awaits), but "silent" doesn't, and with /tmp/silent nothing
  # does, even told to; what it's told is kept in /tmp/told.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    args=$*
    echo "$args" >/tmp/args
    init() { echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$1\"}"; }
    bloat() { init bloat; x=$(head -c 512M /dev/zero | tr '\0' x); }
    hang() {
      init hang
      # Resumed, it puts out the empty result Claude Code can first.
      case " $args " in *" --resume "*) echo '{"type":"result","session_id":"hang","num_turns":0,"result":"","is_error":false}' ;; esac
      sleep 600
    }
    if [ -n "''${PROJ_JOB:-}" ] && [ ! -e /tmp/silent ]; then
      case $prompt in silent*) ;; *) proj done >/dev/null ;; esac
    fi
    case $prompt in
      sleep*) sleep "''${prompt#sleep }" ;;
      named*) init named; sleep "''${prompt#named }" ;;
      chatter*) for _ in $(seq "''${prompt#chatter }"); do echo '{"type":"system"}'; sleep 1; done ;;
      linger*)
        echo '{"type":"result","session_id":"s","result":"waiting on CI","is_error":false}'
        sleep "''${prompt#linger }"
        exit 0 ;;
      detach*)
        # shellcheck disable=SC2016
        setsid bash -c 'trap "" TERM; exec -a proj-stray bash -c "while :; do echo stray; sleep 1; done"' &
        echo '{"type":"result","session_id":"s","result":"waiting on CI","is_error":false}'
        sleep "''${prompt#detach }"
        exit 0 ;;
      "Your answer ended the turn while"*)
        echo "$*" >/tmp/cut-args
        printf '%s' "$prompt" >/tmp/told
        { pgrep -cf '^proj-stray' || true; } >/tmp/strays
        echo '{"type":"system","subtype":"told"}'
        case $(cat /tmp/cut-mode 2>/dev/null) in
          linger)
            echo '{"type":"result","session_id":"s","result":"still waiting","is_error":false}'
            sleep 600 ;;
          hang) sleep 600 ;;
          crash) exit 3 ;;
          signal) kill -KILL $$ ;;
          error)
            echo '{"type":"result","session_id":"s","is_error":true,"errors":["boom"]}'
            exit 1 ;;
          resumed)
            echo '{"type":"result","session_id":"s","num_turns":0,"result":"","is_error":false}'
            sleep 15
            echo '{"type":"result","session_id":"s","num_turns":4,"result":"the build passed","is_error":false}' ;;
          *) echo '{"type":"result","session_id":"s","result":"the build passed","is_error":false}' ;;
        esac
        exit 0 ;;
      "Your answer didn't say what comes next"*)
        printf '%s' "$prompt" >/tmp/told
        echo '{"type":"result","session_id":"s","result":"told","is_error":false}'
        exit 0 ;;
      ask) proj ask >/dev/null ;;
      scratch*)
        echo "''${PROJ_SCRATCH-}" >/tmp/scratch
        git -C /data/dev/src/web worktree add -q --detach "$PROJ_SCRATCH/web" && touch "$PROJ_SCRATCH/web/built"
        case $prompt in *ask) proj ask >/dev/null ;; esac ;;
      await*) proj await "''${prompt#await }" >/dev/null ;;
      # Awaits, then takes a while to answer: what changes meanwhile is news.
      slowawait*) proj await "''${prompt#slowawait }" >/dev/null; sleep 10 ;;
      eat) init eat; head -c 512M /dev/zero | tail >/dev/null; exit 1 ;;
      bloat) bloat ;;
      "Your last run was killed"*) [ ! -e /tmp/again ] || bloat ;;
      *inbox-hang*) hang ;;
      "Your last turn was cut off"*) [ ! -e /tmp/again ] || hang ;;
      # Runs something that outgrows the job's cap, as a whole-flake
      # evaluation does, and answers with how that went.
      outgrow)
        head -c 512M /dev/zero | tail >/dev/null
        echo "{\"type\":\"result\",\"session_id\":\"s\",\"result\":\"it exited $?\",\"is_error\":false}" ;;
      # Answers, then answers again once a background task reports back.
      twice) echo '{"type":"result","session_id":"s","result":"the answer","is_error":false}' ;;
      # Answers, then fails as "fail" does.
      answerfail)
        echo '{"type":"result","session_id":"s","result":"the answer","is_error":false}'
        echo '{"type":"result","session_id":"s","is_error":true,"errors":["refused"]}'
        exit 1 ;;
      # Fails as Claude does when the API refuses it: a result with no text.
      fail) echo '{"type":"result","session_id":"s","is_error":true,"errors":["refused"]}'; exit 1 ;;
      # An issue's task (proj-inbox) that takes a while to answer.
      *inbox-slow*) sleep 8 ;;
      # What it would act on GitHub as: its App's socket, git's settings and
      # the token gh would be given.
      *showenv*) prompt="socket=''${GH_TOKEN_SOCKET-none} count=''${GIT_CONFIG_COUNT-none} token=''${GH_TOKEN-none}" ;;
    esac
    ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'
  '';
  # GitHub, as far as proj-watch and proj-inbox can tell: each endpoint's
  # answer is a file in /tmp/gh that the test writes. A file holding one JSON
  # value per line is one page per line, which --slurp gathers into an array
  # as gh does. What's posted (-F body=@-) is added to /tmp/gh/posted.
  gh = pkgs.writeShellScriptBin "gh" ''
    [ "$1" = api ] || exit 2
    shift
    slurp="" post=""
    while [ $# -gt 0 ]; do
      case $1 in
        --slurp) slurp=1 ;;
        -F | -f) post=1; shift ;;
        -*) ;;
        *) ep=''${1%%\?*} ;;
      esac
      shift
    done
    if [ -n "$post" ]; then cat >>/tmp/gh/posted; echo '{}'; exit 0; fi
    # /tmp/gh/slow: GitHub takes its time, as it does.
    [ -e /tmp/gh/slow ] && sleep 2
    f="/tmp/gh/''${ep//\//_}"
    [ -e "$f" ] || exit 1
    if [ -n "$slurp" ]; then ${pkgs.jq}/bin/jq -cs . "$f"; else cat "$f"; fi
  '';
}
