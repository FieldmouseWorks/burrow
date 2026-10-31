# rust-cache: every Rust build on the machine shares a compile cache. cargo
# runs rustc through sccache (rust-cache/rustc-wrapper.sh), so a dependency a
# user compiled once comes out of that user's cache the next time, in a fresh
# worktree or a CI checkout as much as in the one it was built in. Only
# library crates built without incremental compilation come out of it, which
# in practice means dependencies: a workspace's own crates, binaries, tests,
# proc macros and build scripts still compile every time, so a target dir
# kept between builds, where one can be, saves more still.
#
# One cache per user, never shared: a user is the trust boundary here (each CI
# runner's pull requests run as a user of its own), and whatever a user's
# builds put in the cache, that user's next build takes as compiled. Each
# user's cache has one daemon, rust-cache-USER, the only process that reads
# its index or writes it: sccache's disk cache keeps the index in memory, so
# two daemons on one cache would miss each other's entries and overrun its
# size. The compiles themselves run in the builds, under their own units'
# limits (sccache's client-side mode); the daemon only stores, and couldn't do
# more if a build asked it to: it refuses to compile, and sees nothing but
# sccache itself and the cache. burrow.user has one, and so do `users`;
# anyone else's builds go uncached.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.rustCache;
  inherit (lib) mkOption types;

  # Client-side mode is new in sccache 0.19; burrow's nixpkgs has 0.15. Its
  # daemon still runs whatever compiler a client outside client-side mode
  # names, so this one is patched to refuse, and the client compiles itself.
  # sccache also keys a compile on every CARGO_ variable, so a target dir
  # named by CARGO_TARGET_DIR or CARGO_BUILD_TARGET_DIR made each new target
  # dir miss everything; the second patch leaves those two out, as sccache
  # already leaves out --out-dir.
  # Only the local disk cache is used, so none of the cloud backends either.
  sccache = inputs.nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system}.sccache.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [
      ./rust-cache/sccache-storage-only.patch
      ./rust-cache/sccache-target-dir-key.patch
    ];
    cargoBuildNoDefaultFeatures = true;
  });
  # No config file: the environment below is all of it, for the daemon and
  # the builds alike, whatever a user keeps in ~/.config/sccache.
  conf = pkgs.writeText "sccache.conf" "";

  vars = {
    ROOT = cfg.dir;
    CONF = "${conf}";
    SCCACHE = lib.getExe sccache;
  };

  wrapper = pkgs.writeShellApplication {
    name = "rustc-wrapper";
    runtimeInputs = [ pkgs.coreutils ];
    text = lib.concatStrings (lib.mapAttrsToList (n: v: "${n}=${lib.escapeShellArg v}\n") vars)
      + builtins.readFile ./rust-cache/rustc-wrapper.sh;
  };
  # What RUSTC_WRAPPER names. A process keeps the environment it started
  # with, and a CI runner or a Remote Control session outlives many switches.
  # Naming the wrapper itself would leave it on the one it started with, and
  # naming /etc/rust-cache/rustc-wrapper would leave it naming nothing after a
  # switch that turns the cache off, or back to a system from before it. So
  # it names this launcher, which runs whichever wrapper the running system
  # has there, or rustc itself when there's none. It runs the wrapper by its
  # store path, so a switch that removes the /etc entry mid-launch can't pull
  # it away between the lookup and the exec. A running process's environment
  # keeps the launcher from being collected.
  exe = toString (pkgs.writeShellScript "rustc-launcher" ''
    wrapper=$(${pkgs.coreutils}/bin/readlink -e /etc/rust-cache/rustc-wrapper)
    if [ -x "$wrapper" ]; then
      exec "$wrapper" "$@"
    fi
    exec "$@"
  '');

  users = lib.unique ([ config.burrow.user ] ++ cfg.users);
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.rustCache = {
    enable = lib.mkEnableOption "a shared compile cache (sccache) for every Rust build, one per user";

    dir = mkOption {
      type = types.str;
      default = "/var/cache/sccache";
      description = ''
        Where each user's cache (DIR/USER/cache) and the link to its daemon's
        socket (DIR/USER/sccache.sock) live; the socket itself is in
        /run/rust-cache-USER. Keep it out of snapshots and backups: it's only
        a cache, and it churns with every toolchain and dependency bump. An
        ephemeral / is fine; a reboot only costs one cold build each.
      '';
    };

    size = mkOption {
      type = types.str;
      default = "10G";
      description = "The most each user's cache holds (sccache's SCCACHE_CACHE_SIZE); the least recently used goes first.";
    };

    users = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "ci" ];
      description = ''
        Users besides burrow.user whose builds get a cache of their own: the
        users CI runners run as, say. A runner may read its cache however
        read-only its sandbox makes the rest of the system; only the daemon
        writes it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = map (u: {
      assertion = config.users.users ? ${u};
      message = "burrow.rustCache.users: there is no user ${u}.";
    }) users;

    # What the launcher runs, replaced by each switch.
    environment.etc."rust-cache/rustc-wrapper".source = lib.getExe wrapper;

    # Every way a process here gets its environment: logins and their shells,
    # each user's systemd manager (and so proj's jobs and the Remote Control
    # sessions), and the system manager, for its units (the CI runners) and
    # the ones systemd-run starts alike.
    environment.sessionVariables.RUSTC_WRAPPER = exe;
    environment.etc."environment.d/60-rust-cache.conf".text = "RUSTC_WRAPPER=${exe}\n";
    systemd.settings.Manager.DefaultEnvironment = [ "RUSTC_WRAPPER=${exe}" ];

    # DIR/USER is root's, so only root decides where its socket link points:
    # into the daemon's runtime directory, which exists only while the daemon
    # runs. The cache, and what the link points at, are the user's alone.
    systemd.tmpfiles.rules = [ "d ${cfg.dir} 0755 root root -" ]
      ++ lib.concatMap (u: let g = config.users.users.${u}.group; in [
        "d ${cfg.dir}/${u} 0711 root root -"
        "d ${cfg.dir}/${u}/cache 0700 ${u} ${g} -"
        "L+ ${cfg.dir}/${u}/sccache.sock - - - - /run/rust-cache-${u}/sccache.sock"
      ]) users;

    systemd.services = lib.listToAttrs (map (u: let
      cache = "${cfg.dir}/${u}/cache";
      run = "/run/rust-cache-${u}";
    in lib.nameValuePair "rust-cache-${u}" {
      description = "The compile cache of ${u}'s Rust builds";
      wantedBy = [ "multi-user.target" ];
      environment = {
        SCCACHE_CONF = "${conf}";
        SCCACHE_DIR = cache;
        SCCACHE_CACHE_SIZE = cfg.size;
        SCCACHE_SERVER_UDS = "${run}/sccache.sock";
        SCCACHE_START_SERVER = "1";
        SCCACHE_NO_DAEMON = "1";
        SCCACHE_IDLE_TIMEOUT = "0";
        # A build hands the daemon each compiled crate in one message, and
        # sccache drops anything over 8 MiB (its default) without a word, so a
        # big dependency would never come out of the cache.
        SCCACHE_MAX_FRAME_LENGTH = toString (1024 * 1024 * 1024);
      };
      # A client can ask the daemon for any key, and a key can name a path
      # outside the cache, so the daemon reaches nothing else: a chroot
      # holding only sccache, its config and the cache.
      confinement = {
        enable = true;
        binSh = null;
        packages = [ conf ];
      };
      serviceConfig = {
        User = u;
        ExecStart = lib.getExe sccache;
        Restart = "always";
        RestartSec = 1;
        # systemd removes it, and the socket with it, when the daemon stops,
        # so a build compiles without the cache at once rather than trying a
        # socket nobody is behind.
        RuntimeDirectory = [ "rust-cache-${u}" ];
        RuntimeDirectoryMode = "0700";
        BindPaths = [ cache ];
        PrivateNetwork = true;
        RestrictAddressFamilies = [ "AF_UNIX" ];
        NoNewPrivileges = true;
      };
    }) users);

    burrow.claude.machineMd = lib.mkOrder 1410 ''
      ## Rust builds

      cargo runs rustc through sccache here (`RUSTC_WRAPPER`), with a cache of
      each user's own in ${cfg.dir}/USER that the user's daemon,
      rust-cache-USER, keeps: a fresh worktree's build takes every dependency
      that user compiled before from it. The compile itself still runs in the
      build. Whatever keeps the cache from working compiles without it, so a
      failed build failed in rustc; to rule the cache out anyway, build with
      `RUSTC_WRAPPER=` once. Never point sccache at a TCP port (its default):
      any local user could compile, and so run code, through it.
    '';
  };
}
