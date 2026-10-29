# cargo's RUSTC_WRAPPER (rust-cache.nix): cargo runs `WRAPPER RUSTC ARGS...`,
# and this runs them through sccache with the user's own cache. The compile
# runs here, in the build's own process and unit (sccache's client-side mode):
# the user's daemon, rust-cache-USER, only reads and writes the cache.
# Whatever keeps the cache from being used compiles without it instead, so a
# build never fails for the cache's sake. ROOT, CONF and SCCACHE are set above.

# sccache refuses a build that turns incremental compilation on by name.
if [ "${CARGO_INCREMENTAL:-}" = 1 ] || [ "${CARGO_BUILD_INCREMENTAL:-}" = 1 ]; then
  exec "$@"
fi

# The user's own daemon, or none. ROOT/USER is root's, so the socket's link in
# it is the module's, and it points into the daemon's runtime directory, which
# exists only while the daemon runs and only the user can reach: nobody else
# can stand in for the daemon. A user whose cache isn't theirs (no cache, or
# USER naming someone else) builds without, as does anyone while the daemon is
# down.
base=$ROOT/${USER:-$(id -un)}
if [ ! -O "$base/cache" ] || [ ! -S "$base/sccache.sock" ]; then
  exec "$@"
fi

# Only the module's settings: the user's own SCCACHE_ variables could turn
# client-side mode off (SCCACHE_LOG does) and send the compile to the daemon,
# whose sandbox can't build it.
for var in "${!SCCACHE_@}"; do
  unset "$var"
done

status=0
SCCACHE_CONF=$CONF SCCACHE_SERVER_UDS=$base/sccache.sock SCCACHE_CLIENT_SIDE=1 \
  "$SCCACHE" "$@" || status=$?
# sccache's own failures exit 2 (a socket with no daemon behind it, a compiler
# it can't drive), as does a compile it saw killed by a signal; rustc's never
# do. Compile without it.
if [ "$status" = 2 ]; then
  exec "$@"
fi
exit "$status"
