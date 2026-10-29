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
# limits (sccache's client-side mode); the daemon only stores. burrow.user has
# one, and so do `users`; anyone else's builds go uncached.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.rustCache;
  inherit (lib) mkOption types;

  # Client-side mode is new in sccache 0.19; burrow's nixpkgs has 0.15.
  sccache = inputs.nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system}.sccache;
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
  exe = lib.getExe wrapper;

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
        Where each user's cache (DIR/USER/cache) and its daemon's socket
        (DIR/USER/run) live. Keep it out of snapshots and backups: it's only
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

    # Every way a process here gets its environment: logins and their shells,
    # each user's systemd manager (and so proj's jobs and the Remote Control
    # sessions), and the system manager, for its units (the CI runners) and
    # the ones systemd-run starts alike.
    environment.sessionVariables.RUSTC_WRAPPER = exe;
    environment.etc."environment.d/60-rust-cache.conf".text = "RUSTC_WRAPPER=${exe}\n";
    systemd.settings.Manager.DefaultEnvironment = [ "RUSTC_WRAPPER=${exe}" ];

    # DIR/USER is root's, so only root decides where its socket link points;
    # what it points at, and the cache, are the user's alone.
    systemd.tmpfiles.rules = [ "d ${cfg.dir} 0755 root root -" ]
      ++ lib.concatMap (u: let g = config.users.users.${u}.group; in [
        "d ${cfg.dir}/${u} 0711 root root -"
        "d ${cfg.dir}/${u}/cache 0700 ${u} ${g} -"
        "d ${cfg.dir}/${u}/run 0700 ${u} ${g} -"
        "L+ ${cfg.dir}/${u}/sccache.sock - - - - run/sccache.sock"
      ]) users;

    systemd.services = lib.listToAttrs (map (u: lib.nameValuePair "rust-cache-${u}" {
      description = "The compile cache of ${u}'s Rust builds";
      wantedBy = [ "multi-user.target" ];
      environment = {
        SCCACHE_CONF = "${conf}";
        SCCACHE_DIR = "${cfg.dir}/${u}/cache";
        SCCACHE_CACHE_SIZE = cfg.size;
        SCCACHE_SERVER_UDS = "${cfg.dir}/${u}/run/sccache.sock";
        SCCACHE_START_SERVER = "1";
        SCCACHE_NO_DAEMON = "1";
        SCCACHE_IDLE_TIMEOUT = "0";
      };
      serviceConfig = {
        User = u;
        ExecStart = lib.getExe sccache;
        # A build that finds a socket with nobody behind it tries to start a
        # daemon of its own, fails (the link isn't the build's to replace) and
        # waits 5 s for one before compiling without; with no socket, it
        # compiles without at once.
        ExecStopPost = "${pkgs.coreutils}/bin/rm -f ${cfg.dir}/${u}/run/sccache.sock";
        Restart = "always";
        RestartSec = 1;
        # It only reads and writes the cache: it never compiles, and listens
        # on nothing but its socket.
        ReadWritePaths = [ "${cfg.dir}/${u}/cache" "${cfg.dir}/${u}/run" ];
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
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
