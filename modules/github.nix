# GitHub Apps' installation tokens for the processes that should act as an App
# rather than as the user's own gh login, from a key none of them can read.
#
# Each App gets a socket, /run/gh-token/NAME.sock, that its `users` can
# connect to. A connection is answered by gh-token (./github, one process each,
# Accept=yes) as its own user, with the App's key as a credential only that
# user can read: a token for the account or repo asked for, good for at least
# ten minutes, kept until then so GitHub mints one at most every fifty.
#
# A process acts as the App with the App's `env` in its environment. Then gh
# (burrow.tools.gh, the one on the PATH) gets a token for the account its call
# is about, and git's only credential helper is gh-token, which gets one for
# the repo git names on github.com and stops git for any other host. Neither
# can fall back to a login of the user's: git's other helpers are cleared,
# git doesn't use SSH (GitHub's SSH remotes go over https), no host hears the
# logins git's config could send without asking the helper (a URL's, a
# header's, a cookie jar's, a client certificate, a Kerberos ticket, the
# netrc's: ./github/src/remote.rs), git-lfs, which would, isn't run, and gh's
# own login, in a file or the keyring, is out of its reach.
# Without that env, all of them are the user's, as before.
#
# The env closes what git and gh do by themselves and from config files.
# What a caller puts on git's own command line (`-c`, GIT_CONFIG_PARAMETERS)
# comes after it, so a call that names another program for git to run (a
# git-lfs, a credential helper) runs it: a process acting as the App runs as
# the user, and can always reach the user's own files.
#
# Only github.com, over https: the App's tokens are for it, and in its env git
# has no login for any other host.
inputs:
{ config, lib, pkgs, ... }:
let
  inherit (lib) mkOption types;
  cfg = config.burrow.github;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.gh-token;

  # git over SSH, in an App's env: the user's own keys, so never.
  noSsh = pkgs.writeShellScript "gh-token-no-ssh" ''
    echo "gh-token: acting as the App, git reaches GitHub over https, never over SSH with your keys" >&2
    exit 1
  '';

  # git-lfs as everyone runs it: never in an App's env, where it would send
  # the user's own login (remote.rs), git-lfs otherwise.
  gitLfs = pkgs.writeShellScriptBin "git-lfs" ''
    exec ${package}/bin/gh-token git-lfs ${pkgs.git-lfs}/bin/git-lfs "$@"
  '';

  # git's own helpers (GIT_EXEC_PATH), but for curl's: gh-token's
  # git-remote, which runs git's with only the credential helper's login.
  # curl's are git-remote-http and every name git gives it (https, ftp,
  # ftps), found as the files that are it, so a name a later git adds is
  # one too. And git-lfs: git puts these first on the PATH of what it runs
  # (`git lfs`, hooks, filters). The machine's git's, so the git a process
  # in the env runs is it.
  git = config.programs.git.package;
  gitCore = pkgs.runCommand "gh-token-git-core" { } ''
    mkdir $out
    core=${git}/libexec/git-core
    for f in $core/*; do ln -s "$f" $out/; done
    ln -s ${gitLfs}/bin/git-lfs $out/git-lfs
    for f in $core/*; do
      [ "$f" -ef $core/git-remote-http ] || continue
      helper=$(basename "$f")
      rm $out/$helper
      printf '#!%s\nexec %s git-remote %s "$@"\n' ${pkgs.runtimeShell} ${package}/bin/gh-token \
        $core/$helper >$out/$helper
      chmod +x $out/$helper
    done
    for helper in git-remote-http git-remote-https; do
      grep -q 'gh-token git-remote' $out/$helper || { echo "$helper isn't git-remote-http" >&2; exit 1; }
    done
  '';

  # gh as everyone runs it: the App's token when GH_TOKEN_SOCKET asks for
  # one, the user's own login otherwise (client.rs).
  gh = pkgs.writeShellScriptBin "gh" ''
    exec ${package}/bin/gh-token gh ${pkgs.gh}/bin/gh "$@"
  '';
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.github.apps = mkOption {
    default = { };
    description = ''
      GitHub Apps whose tokens processes on this machine ask for, by name.
      Make each under the account that owns the repos, install it on every
      account whose repos it acts on, and put its private key in `keyFile`.
    '';
    type = types.attrsOf (types.submodule ({ name, ... }: let socket = "/run/gh-token/${name}.sock"; in {
      options = {
        id = mkOption {
          type = types.ints.positive;
          description = "The App's ID, from its settings page.";
        };
        slug = mkOption {
          type = types.str;
          description = "The App's slug: GitHub shows its bot as SLUG[bot].";
        };
        keyFile = mkOption {
          type = types.str;
          description = "The App's private key (PEM), readable by root: a secret, never in the store.";
        };
        api = mkOption {
          type = types.str;
          default = "https://api.github.com";
          description = "GitHub's REST API: another is a stand-in for github.com's, as the tests have.";
        };
        users = mkOption {
          type = types.listOf types.str;
          default = [ config.burrow.user ];
          defaultText = lib.literalExpression "[ config.burrow.user ]";
          description = "Who may ask for its tokens.";
        };
        socket = mkOption {
          type = types.str;
          readOnly = true;
          default = socket;
          description = "Where its tokens are asked for.";
        };
        env = mkOption {
          type = types.attrsOf types.str;
          readOnly = true;
          default = {
            GH_TOKEN_SOCKET = socket;
            # A gh that isn't the wrapper finds no login, rather than the
            # user's: not in its config, not in the keyring (the session bus
            # it's reached on), not in gh's other variables, github.com's or
            # another host's (an empty one is none).
            GH_CONFIG_DIR = "/run/gh-token/no-login";
            DBUS_SESSION_BUS_ADDRESS = "disabled:";
            GITHUB_TOKEN = "";
            GH_ENTERPRISE_TOKEN = "";
            GITHUB_ENTERPRISE_TOKEN = "";
            # Set in the environment, so they come after every config file's.
            GIT_CONFIG_COUNT = "9";
            # gh-token is git's only credential helper, for every URL: the
            # empty one clears whatever helpers the config files name, for any
            # URL. It answers github.com over https, however git spells it,
            # with the App's token, and stops git for every other host.
            GIT_CONFIG_KEY_0 = "credential.helper";
            GIT_CONFIG_VALUE_0 = "";
            GIT_CONFIG_KEY_1 = "credential.helper";
            GIT_CONFIG_VALUE_1 = "!${package}/bin/gh-token git-credential";
            GIT_CONFIG_KEY_2 = "credential.useHttpPath";
            GIT_CONFIG_VALUE_2 = "true";
            # GitHub's SSH remotes go over https instead, and anything still
            # over SSH (an alias, a rewrite of the user's own) stops.
            GIT_CONFIG_KEY_3 = "url.https://github.com/.insteadOf";
            GIT_CONFIG_VALUE_3 = "git@github.com:";
            GIT_CONFIG_KEY_4 = "url.https://github.com/.insteadOf";
            GIT_CONFIG_VALUE_4 = "ssh://git@github.com/";
            # LFS's filters, whatever git-lfs a config names, are the one that
            # refuses, and a file they'd filter fails rather than going in or
            # out as it is.
            GIT_CONFIG_KEY_5 = "filter.lfs.process";
            GIT_CONFIG_VALUE_5 = "${gitLfs}/bin/git-lfs filter-process";
            GIT_CONFIG_KEY_6 = "filter.lfs.smudge";
            GIT_CONFIG_VALUE_6 = "${gitLfs}/bin/git-lfs smudge -- %f";
            GIT_CONFIG_KEY_7 = "filter.lfs.clean";
            GIT_CONFIG_VALUE_7 = "${gitLfs}/bin/git-lfs clean -- %f";
            GIT_CONFIG_KEY_8 = "filter.lfs.required";
            GIT_CONFIG_VALUE_8 = "true";
            GIT_SSH_COMMAND = "${noSsh}";
            GIT_SSH_VARIANT = "ssh";
            GIT_EXEC_PATH = "${gitCore}";
          };
          defaultText = lib.literalMD "GH_TOKEN_SOCKET, and git's and gh's settings for it";
          description = "The environment a process acts as the App in.";
        };
      };
    }));
  };

  config = lib.mkIf (cfg.apps != { }) {
    burrow.tools.gh = gh;
    burrow.tools.git-lfs = gitLfs;
    burrow.tools.gh-token = package;

    users.users.gh-token = {
      isSystemUser = true;
      group = "gh-token";
    };
    users.groups = { gh-token = { }; } // lib.mapAttrs' (name: app:
      lib.nameValuePair "gh-token-${name}" { members = app.users; }) cfg.apps;

    systemd.sockets = lib.mapAttrs' (name: app: lib.nameValuePair "gh-token-${name}" {
      description = "Tokens of the GitHub App ${app.slug}";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ app.socket ];
      socketConfig = {
        Accept = true;
        SocketGroup = "gh-token-${name}";
        SocketMode = "0660";
        DirectoryMode = "0755";
      };
    }) cfg.apps;

    systemd.services = lib.mapAttrs' (name: app: lib.nameValuePair "gh-token-${name}@" {
      description = "A token of the GitHub App ${app.slug}, for one who asked";
      serviceConfig = {
        ExecStart = "${package}/bin/gh-token answer --api ${lib.escapeShellArg app.api} --app-id ${toString app.id} --key %d/key --cache %t/gh-token-cache/${name}";
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        User = "gh-token";
        Group = "gh-token";
        LoadCredential = [ "key:${app.keyFile}" ];
        # Tokens kept between answers, in memory only, for this user only.
        RuntimeDirectory = "gh-token-cache/${name}";
        RuntimeDirectoryMode = "0700";
        RuntimeDirectoryPreserve = true;
        # An asker that never finishes its request.
        RuntimeMaxSec = "2min";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
      };
    }) cfg.apps;
  };
}
