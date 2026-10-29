# cargo's RUSTC_WRAPPER (rust-cache.nix): cargo runs `WRAPPER RUSTC ARGS...`,
# and this runs them through sccache with the user's own cache, ROOT/USER.
# Whatever keeps the cache from being used compiles without it instead, so a
# build never fails for the cache's sake. ROOT, SIZE and SCCACHE are set above.

# sccache refuses a build that turns incremental compilation on by name.
if [ "${CARGO_INCREMENTAL:-}" = 1 ]; then
  exec "$@"
fi

# Only a directory the user owns: one another user made could be theirs to
# fill. The module makes each user's, mode 0700.
base=$ROOT/${USER:-$(id -un)}
if [ ! -d "$base" ] || [ ! -O "$base" ]; then
  exec "$@"
fi
for sub in run cache; do
  if [ ! -d "$base/$sub" ]; then
    mkdir -m 0700 "$base/$sub" 2>/dev/null || [ -d "$base/$sub" ] || exec "$@"
  fi
done

# One server per systemd unit (by cgroup), on a socket in the user's own
# directory. A server runs the compiles sent to it as its own children, so
# each unit's stay under that unit's limits and end with it; and nobody but
# the user can reach it, unlike sccache's default TCP port, which would let any
# local user compile, and so run code, as this one.
cgroup=
read -r cgroup </proc/self/cgroup || true
key=$(printf '%s' "$cgroup" | cksum)
key=${key%% *}
sock=$base/run/$key.sock
lock=$base/run/$key.lock
export SCCACHE_DIR=$base/cache SCCACHE_CACHE_SIZE=$SIZE SCCACHE_SERVER_UDS=$sock

# A unit's server holds its lock as long as it lives. With nothing holding it,
# start one, without the build's open files: sccache's own start would hand it
# all of them, and a lock the build holds (a gate slot, say) would stay held by
# the server, idle, long after the build is done.
server=
if flock -n "$lock" rm -f "$sock"; then
  (
    for fd in /proc/self/fd/*; do
      fd=${fd##*/}
      if [ "$fd" -gt 2 ]; then
        eval "exec $fd>&-"
      fi
    done
    SCCACHE_START_SERVER=1 SCCACHE_NO_DAEMON=1 exec flock -n "$lock" "$SCCACHE"
  ) </dev/null >/dev/null 2>&1 &
  server=$!
fi

# Wait for the unit's server to listen: the one started here, or, if that one
# lost the lock to another build's or died, whichever holds the lock. With
# none holding it, compile without.
tries=0
while [ "$tries" -lt 200 ]; do
  if [ -S "$sock" ]; then
    exec "$SCCACHE" "$@"
  fi
  if [ -n "$server" ] && ! kill -0 "$server" 2>/dev/null; then
    server=
  fi
  if [ -z "$server" ] && flock -n "$lock" true; then
    break
  fi
  sleep 0.05
  tries=$((tries + 1))
done
exec "$@"
