# rust-cache: every Rust build on the machine shares a compile cache. cargo
# runs rustc through sccache (rust-cache/rustc-wrapper.sh), so a crate a user
# compiled once comes out of that user's cache the next time, in a fresh
# worktree or a CI checkout as much as in the one it was built in. Only
# library crates come out of it: binaries, tests, proc macros, build scripts
# and incrementally built crates still compile every time, so a target dir
# kept between builds, where one can be, saves more still.
#
# One cache per user, never shared: a user is the trust boundary here (each CI
# runner's pull requests run as a user of its own), and whatever a user's
# builds put in the cache, that user's next build takes as compiled. Each
# user's lives in DIR/USER, mode 0700, made for burrow.user and the users of
# `services`; anyone else's builds go uncached.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.rustCache;
  inherit (lib) mkOption types;

  vars = {
    ROOT = cfg.dir;
    SIZE = cfg.size;
    SCCACHE = lib.getExe pkgs.sccache;
  };

  wrapper = pkgs.writeShellApplication {
    name = "rustc-wrapper";
    runtimeInputs = [ pkgs.coreutils pkgs.util-linux ];
    text = lib.concatStrings (lib.mapAttrsToList (n: v: "${n}=${lib.escapeShellArg v}\n") vars)
      + builtins.readFile ./rust-cache/rustc-wrapper.sh;
  };
  exe = lib.getExe wrapper;

  users = lib.unique ([ config.burrow.user ]
    ++ lib.concatMap (s: lib.toList (config.systemd.services.${s}.serviceConfig.User or [ ])) cfg.services);
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.rustCache = {
    enable = lib.mkEnableOption "a shared compile cache (sccache) for every Rust build, one per user";

    dir = mkOption {
      type = types.str;
      default = "/var/cache/sccache";
      description = ''
        Where each user's cache (DIR/USER/cache) and its servers' sockets
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

    services = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "github-runner-ci" ];
      description = ''
        System services that build Rust as a user of their own, CI runners
        say: each one's user gets a cache, and the service may write it
        however read-only its sandbox makes the rest of the system.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = map (s: {
      assertion = config.systemd.services.${s}.serviceConfig ? User;
      message = "burrow.rustCache.services: ${s} runs as no User of its own, so it has no cache to write.";
    }) cfg.services;

    # Every way a process here gets its environment: logins and their shells,
    # each user's systemd manager (and so proj's jobs and the Remote Control
    # sessions), and the system's own units (the CI runners).
    environment.sessionVariables.RUSTC_WRAPPER = exe;
    environment.etc."environment.d/60-rust-cache.conf".text = "RUSTC_WRAPPER=${exe}\n";
    systemd.globalEnvironment.RUSTC_WRAPPER = exe;

    systemd.tmpfiles.rules = [ "d ${cfg.dir} 0755 root root -" ]
      ++ map (u: "d ${cfg.dir}/${u} 0700 ${u} ${config.users.users.${u}.group} -") users
      # A server's socket and lock outlive it; those idle for 10 days go.
      ++ [ "e ${cfg.dir}/*/run - - - 10d" ];

    burrow.claude.machineMd = lib.mkOrder 1410 ''
      ## Rust builds

      cargo runs rustc through sccache here (`RUSTC_WRAPPER`), with a cache of
      each user's own in ${cfg.dir}/USER: a fresh worktree's build takes every
      crate that user compiled before from it. Whatever keeps the cache from
      working compiles without it, so a failed build failed in rustc; to rule
      the cache out anyway, build with `RUSTC_WRAPPER=` once. Never point
      sccache at a TCP port (its default): any local user could compile, and
      so run code, through it.
    '';

    systemd.services = lib.genAttrs cfg.services (_: {
      # "-": a service that starts before the directory exists still starts.
      serviceConfig.ReadWritePaths = [ "-${cfg.dir}" ];
    });
  };
}
