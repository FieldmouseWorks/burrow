# The update job's dispatch (modules/updates/dispatch.sh), with a stand-in for
# proj: its fix jobs are pipeline work, and one problem gets one job while it's
# recent. $1 is dispatch.sh.
set -euo pipefail

tmp=$(mktemp -d)
state=$tmp/state
# shellcheck disable=SC2034 # dispatch's
PROJECT=homelab
mkdir -p "$state/jobs"
as_user() { "$@"; }
proj() { printf '%s\n' "$*" >>"$tmp/proj.log"; echo "homelab-$(wc -l <"$tmp/proj.log")"; }
# shellcheck source=/dev/null
. "$1"

fail() { echo "FAIL: $*" >&2; exit 1; }

# A fix to what deploys everything goes ahead of everyone's jobs, in the slot
# kept for it, never behind a project's long ones.
dispatch build-main "main doesn't build" >/dev/null
[ "$(cat "$tmp/proj.log")" = "run --pipeline homelab main doesn't build" ] || fail "proj got: $(cat "$tmp/proj.log")"
[ "$(cat "$state/jobs/build-main")" = homelab-1 ] || fail "no job recorded for build-main"

# The same problem again while its job is recent: no second job.
dispatch build-main "main doesn't build" >/dev/null
[ "$(wc -l <"$tmp/proj.log")" = 1 ] || fail "a second job for build-main"

# Once it's 6 hours old, the problem gets a job again.
touch -d '7 hours ago' "$state/jobs/build-main"
dispatch build-main "main doesn't build" >/dev/null
[ "$(wc -l <"$tmp/proj.log")" = 2 ] || fail "no new job once the last was 6 hours old"
[ "$(cat "$state/jobs/build-main")" = homelab-2 ] || fail "build-main's job wasn't replaced"

# With a machine App, the job acts as it.
# shellcheck disable=SC2034 # dispatch's
APP=workers
dispatch deploy "the switch failed" >/dev/null
[ "$(tail -1 "$tmp/proj.log")" = "run --pipeline --app workers homelab the switch failed" ] || fail "proj got: $(tail -1 "$tmp/proj.log")"

echo "update-dispatch: ok"
