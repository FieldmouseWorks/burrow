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
# the repo git names. Neither can fall back to the user's own login: git's
# other helpers are cleared, git doesn't reach GitHub over SSH (its SSH
# remotes go over https), and gh's own login, in a file or the keyring, is
# out of its reach. Without that env, both are the user's, as before.
#
# Only github.com: the env's settings for git and gh are its.
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
            # it's reached on), not in GitHub's other variable.
            GH_CONFIG_DIR = "/run/gh-token/no-login";
            DBUS_SESSION_BUS_ADDRESS = "disabled:";
            GITHUB_TOKEN = "";
            # Set in the environment, so they come after every config file's:
            # the empty helper clears the ones before it.
            GIT_CONFIG_COUNT = "5";
            GIT_CONFIG_KEY_0 = "credential.https://github.com.helper";
            GIT_CONFIG_VALUE_0 = "";
            GIT_CONFIG_KEY_1 = "credential.https://github.com.helper";
            GIT_CONFIG_VALUE_1 = "!${package}/bin/gh-token git-credential";
            GIT_CONFIG_KEY_2 = "credential.https://github.com.useHttpPath";
            GIT_CONFIG_VALUE_2 = "true";
            # GitHub's SSH remotes go over https instead, and anything still
            # over SSH (an alias, a rewrite of the user's own) stops.
            GIT_CONFIG_KEY_3 = "url.https://github.com/.insteadOf";
            GIT_CONFIG_VALUE_3 = "git@github.com:";
            GIT_CONFIG_KEY_4 = "url.https://github.com/.insteadOf";
            GIT_CONFIG_VALUE_4 = "ssh://git@github.com/";
            GIT_SSH_COMMAND = "${noSsh}";
            GIT_SSH_VARIANT = "ssh";
          };
          defaultText = lib.literalMD "GH_TOKEN_SOCKET, and git's and gh's settings for it";
          description = "The environment a process acts as the App in.";
        };
      };
    }));
  };

  config = lib.mkIf (cfg.apps != { }) {
    burrow.tools.gh = gh;
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
