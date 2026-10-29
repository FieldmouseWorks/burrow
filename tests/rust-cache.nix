# rust-cache's wrapper, for real: the example machine's own RUSTC_WRAPPER,
# made for a cache directory in the build's sandbox, builds a crate through
# sccache and a fresh target dir's build of it comes out of the cache. The
# server listens on a socket in the user's own 0700 directory, and leaves
# alone a lock the build held: a build under a held gate slot frees it when it
# ends, server or no server. A cached `cargo check` never stands in for
# clippy, so the lint still fails. A build sccache won't take (incremental by
# name), and a user with no cache directory, compile without it.
#
#   nix build .#checks.x86_64-linux.rust-cache -L
{ self, pkgs }:
let
  # The sandbox's build directory; the check fails at once if it's elsewhere.
  dir = "/build/rust-cache";
  machine = self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.rustCache.dir = dir; }];
  };
  wrapper = machine.config.systemd.globalEnvironment.RUSTC_WRAPPER;
in
pkgs.runCommand "rust-cache-check" {
  nativeBuildInputs = with pkgs; [ cargo rustc clippy sccache util-linux ];
} ''
  set -euo pipefail
  fail() { echo "FAIL: $*" >&2; exit 1; }
  me=$(id -un)
  mkdir -p ${dir}
  mkdir -m 0700 ${dir}/$me
  export HOME=$PWD/home CARGO_INCREMENTAL=0 RUSTC_WRAPPER=${wrapper}

  mkdir -p crate/src
  cat >crate/Cargo.toml <<'EOF'
  [package]
  name = "crate"
  version = "0.1.0"
  edition = "2021"
  EOF
  cat >crate/src/lib.rs <<'EOF'
  pub fn next(x: i32) -> i32 {
      return x + 1;
  }
  EOF
  cd crate

  stats() {
    SCCACHE_SERVER_UDS=$sock sccache --show-stats --stats-format json
  }
  hits() { stats | sed -n 's/.*"cache_hits":{"counts":{"Rust":\([0-9]*\).*/\1/p'; }

  # A build under a gate slot, held the way a shell script holds one.
  (
    exec 9>../slot.lock
    flock 9
    cargo build -q
  )
  sock=$(echo ${dir}/$me/run/*.sock)
  [ -S "$sock" ] || fail "no server socket in ${dir}/$me/run"
  [ "$(stat -c %a ${dir}/$me)" = 700 ] || fail "the user's cache directory isn't 0700"
  [ "$(stat -c %a ${dir}/$me/run)" = 700 ] || fail "the sockets' directory isn't 0700"
  stats >/dev/null || fail "the server isn't answering"
  flock -n ../slot.lock true || fail "the server holds the build's gate slot"

  rm -rf target
  cargo build -q
  [ "$(hits)" -ge 1 ] || fail "a fresh target dir's build missed the cache: $(stats)"

  rm -rf target
  cargo check -q
  rm -rf target
  if cargo clippy -q -- -D warnings 2>clippy.log; then
    fail "clippy passed: the cache stood in for the lint"
  fi
  grep -q needless_return clippy.log || fail "clippy failed without the lint: $(cat clippy.log)"

  rm -rf target
  CARGO_INCREMENTAL=1 cargo build -q || fail "an incremental build failed"

  rm -rf target
  USER=nobody cargo build -q || fail "a user with no cache failed to build"

  SCCACHE_SERVER_UDS=$sock sccache --stop-server >/dev/null
  touch $out
''
