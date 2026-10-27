# browser: a Chromium that stays up on the machine, signed in to the owner's
# accounts by the owner, that every Claude on the machine can drive. The owner
# watches it and signs in through noVNC in their own browser; Claude drives it
# with Playwright's MCP server over the Chrome DevTools Protocol (CDP).
#
# Its profile holds the owner's sessions, so it's kept like a secret: its own
# user, mode 0700, outside the data dataset the sandboxes clone. Its X display
# lives in a private /tmp and its VNC on a socket only that user opens. CDP
# and the viewer are TCP on this machine, so the firewall refuses both to
# every local user but burrow.user: not the sandboxes' egress proxy, the
# reviewers or CI. The viewer is open to the network only on
# burrow.browser.viewer.interfaces.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.browser;
  inherit (config.burrow) user;
  inherit (config.networking) hostName;
  account = "burrow-browser";
  stateDir = "/var/lib/${account}";
  runDir = "/run/${account}";
  # Its own X server in a private /tmp: no other display to clash with.
  display = ":0";
  ports = [ cfg.cdpPort cfg.viewer.port ];
  home = config.users.users.${user}.home;
  uid = toString config.users.users.${user}.uid;
  # What keeps everyone else off its ports: nothing listens without it.
  firewall = if config.networking.nftables.enable then "nftables.service" else "firewall.service";

  # What Claude starts for each session: Playwright's MCP server on the running
  # browser. Snapshots and logs go to the user's runtime dir, not the project.
  mcp = pkgs.writeShellApplication {
    name = "burrow-browser-mcp";
    text = ''
      exec ${lib.getExe cfg.mcpPackage} --cdp-endpoint http://127.0.0.1:${toString cfg.cdpPort} \
        --output-dir "''${XDG_RUNTIME_DIR:?}/burrow-browser" "$@"
    '';
  };
  # A stable path, so Claude's config never changes when the package does.
  server = builtins.toJSON { type = "stdio"; command = "/run/current-system/sw/bin/burrow-browser-mcp"; args = [ ]; };
  register = pkgs.writeShellApplication {
    name = "burrow-browser-register";
    runtimeInputs = [ config.burrow.claude.package pkgs.jq pkgs.systemd ];
    text = ''
      want=${lib.escapeShellArg server}
      case ''${1:?add or remove} in
        add)
          jq -e --argjson w "$want" '.mcpServers.browser == $w' ~/.claude.json >/dev/null && exit 0
          claude mcp remove -s user browser >/dev/null 2>&1 || true
          claude mcp add-json -s user browser "$want" ;;
        remove)
          # Shutting down isn't turning it off.
          [ "$(systemctl is-system-running || true)" = stopping ] && exit 0
          claude mcp remove -s user browser ;;
      esac
    '';
  };

  # Each piece runs as the browser's user, sees no /home, and writes only its
  # state and runtime dirs. Chromium's own sandbox needs user namespaces, so
  # those stay allowed.
  confined = {
    User = account;
    Group = account;
    Slice = "${account}.slice";
    PrivateTmp = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    NoNewPrivileges = true;
    Restart = "always";
    RestartSec = 1;
  };
  # The pieces that draw on the display share its /tmp, where its socket is.
  onDisplay = {
    requires = [ "${account}-display.service" ];
    after = [ "${account}-display.service" ];
    partOf = [ "${account}-display.service" ];
    wantedBy = [ "multi-user.target" ];
    unitConfig.JoinsNamespaceOf = "${account}-display.service";
    environment.DISPLAY = display;
  };
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.browser = {
    enable = lib.mkEnableOption "a signed-in browser on the machine that Claude drives and the owner watches";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.chromium;
      defaultText = lib.literalExpression "pkgs.chromium";
      description = "The browser. It has to speak CDP: Chromium or Chrome.";
    };

    mcpPackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.playwright-mcp;
      defaultText = lib.literalExpression "pkgs.playwright-mcp";
      description = "Playwright's MCP server, which Claude drives the browser with.";
    };

    cdpPort = lib.mkOption {
      type = lib.types.port;
      default = 9222;
      description = "The browser's DevTools port, which Chromium opens on 127.0.0.1, for burrow.user only.";
    };

    viewer = {
      port = lib.mkOption {
        type = lib.types.port;
        default = 6080;
        description = "noVNC, the owner's view of the browser, at /vnc.html.";
      };
      interfaces = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "tailscale0" ];
        description = ''
          Interfaces the viewer is open on, such as a tailnet's. It asks for no
          password: whoever reaches it can use every account the browser is
          signed in to, so only name interfaces that reach only the owner.
          Empty: only burrow.user on the machine, or an SSH tunnel
          (`ssh -L PORT:localhost:PORT`).
        '';
      };
    };

    viewer.origins = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = map (h: "http://${h}:${toString cfg.viewer.port}") [ "localhost" "127.0.0.1" hostName ];
      defaultText = lib.literalExpression ''map (h: "http://''${h}:''${toString cfg.viewer.port}") [ "localhost" "127.0.0.1" config.networking.hostName ]'';
      description = ''
        The addresses the owner opens the viewer at, as origins: its WebSocket
        refuses a page from anywhere else, so another site open in their browser
        can't watch or drive it.
      '';
    };

    memory = lib.mkOption {
      type = lib.types.str;
      default = "4G";
      description = "The most the browser and its display may hold (MemoryMax).";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      # CDP and the viewer answer anyone who can connect.
      assertion = config.networking.firewall.enable;
      message = "burrow.browser needs networking.firewall: it keeps every user but burrow.user off the browser's ports.";
    } {
      assertion = config.users.users.${user}.uid != null;
      message = "burrow.browser needs burrow.user to have a fixed uid: the firewall lets only that uid reach the browser.";
    }];

    users.users.${account} = {
      isSystemUser = true;
      group = account;
      home = stateDir;
    };
    users.groups.${account} = { };

    # Without fonts every page is boxes.
    fonts.enableDefaultPackages = lib.mkDefault true;

    environment.systemPackages = [ mcp ];

    systemd.slices.${account} = {
      description = "The signed-in browser";
      sliceConfig.MemoryMax = cfg.memory;
    };

    systemd.services = {
      # The screen: an X server whose VNC is a socket only its user opens. No
      # TCP, no abstract socket; its X socket is in its private /tmp.
      "${account}-display" = {
        description = "The browser's screen";
        wantedBy = [ "multi-user.target" ];
        # Everything else runs on the screen, so it all stops with the firewall.
        bindsTo = [ firewall ];
        after = [ firewall ];
        serviceConfig = confined // {
          RuntimeDirectory = account;
          RuntimeDirectoryMode = "0700";
          ExecStart = lib.escapeShellArgs [
            "${pkgs.tigervnc}/bin/Xvnc" display
            "-rfbunixpath" "${runDir}/vnc.sock" "-rfbport" "-1"
            "-SecurityTypes" "None" "-AlwaysShared"
            "-nolisten" "tcp" "-nolisten" "local"
            "-geometry" "1280x800" "-desktop" "${hostName} browser"
          ];
          ExecStartPost = "${pkgs.bash}/bin/bash -c 'until [ -S /tmp/.X11-unix/X0 ]; do sleep 0.1; done'";
        };
      };

      # Every window full screen, and still so when the viewer resizes it.
      "${account}-wm" = onDisplay // {
        description = "The browser's window manager";
        serviceConfig = confined // {
          ExecStart = "${pkgs.matchbox}/bin/matchbox-window-manager -use_titlebar no";
        };
      };

      "${account}" = onDisplay // {
        description = "The signed-in browser";
        environment = onDisplay.environment // { HOME = stateDir; };
        serviceConfig = confined // {
          StateDirectory = account;
          StateDirectoryMode = "0700";
          # Closed as its window would be, so it writes its cookies first. As
          # burrow.user: the only one its DevTools port answers.
          ExecStop = "+${pkgs.util-linux}/bin/runuser -u ${user} -- ${pkgs.python3}/bin/python3 -I ${./browser/close.py} ${toString cfg.cdpPort}";
          ExecStart = lib.escapeShellArgs [
            (lib.getExe cfg.package)
            "--user-data-dir=${stateDir}/profile"
            "--remote-debugging-port=${toString cfg.cdpPort}"
            # No keyring here: Chromium would wait for one.
            "--password-store=basic"
            "--no-first-run"
            "--no-default-browser-check"
            "--start-maximized"
          ];
        };
      };

      # noVNC and the WebSocket it talks to the screen through.
      "${account}-viewer" = {
        description = "The owner's view of the browser (noVNC)";
        # It asks for no password, so a page of any other site in the owner's
        # browser mustn't get to open its WebSocket.
        requires = [ "${account}-display.service" ];
        after = [ "${account}-display.service" ];
        partOf = [ "${account}-display.service" ];
        wantedBy = [ "multi-user.target" ];
        serviceConfig = confined // {
          ExecStart = lib.escapeShellArgs [
            (lib.getExe' pkgs.python3Packages.websockify "websockify")
            "--web=${pkgs.novnc}/share/webapps/novnc" "--file-only"
            "--unix-target=${runDir}/vnc.sock"
            "--auth-plugin=websockify.auth_plugins.ExpectOrigin"
            "--auth-source=${lib.concatStringsSep " " cfg.viewer.origins}"
            (toString cfg.viewer.port)
          ];
        };
      };

      # Claude's tools for it, in burrow.user's Claude config (user scope, so
      # every session has them). Waits for Claude to have a config: making one
      # would read as logged in.
      "${account}-mcp" = {
        description = "Give Claude Code the browser's MCP server";
        wantedBy = [ "multi-user.target" ];
        unitConfig.ConditionPathExists = "${home}/.claude.json";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          User = user;
          ExecStart = "${lib.getExe register} add";
          ExecStop = "${lib.getExe register} remove";
        };
      };
    };
    systemd.paths."${account}-mcp" = {
      wantedBy = [ "paths.target" ];
      pathConfig.PathExists = "${home}/.claude.json";
    };

    # Only burrow.user may connect to CDP or the viewer from this machine
    # (connections to its own addresses leave through lo, whatever the
    # address), and from elsewhere only the viewer, on viewer.interfaces, even
    # where the firewall trusts an interface or opens the port more widely.
    networking.firewall = lib.mkMerge [
      { interfaces = lib.genAttrs cfg.viewer.interfaces (_: { allowedTCPPorts = [ cfg.viewer.port ]; }); }
      (lib.mkIf (!config.networking.nftables.enable) {
        # Built beside the live chains, hooked in, and only then is the old
        # one unhooked: a reload never leaves the ports open. Nothing in
        # extraStopCommands: a stopped firewall keeps these, and the browser
        # stops with it anyway.
        extraCommands = ''
          for c in ${account}-out ${account}-in; do
            ip46tables -F "$c-new" 2>/dev/null || ip46tables -N "$c-new"
          done
          ip46tables -A ${account}-out-new -p tcp -m multiport --dports ${lib.concatMapStringsSep "," toString ports} \
            -m owner ! --uid-owner ${uid} -j REJECT --reject-with tcp-reset
          ip46tables -A ${account}-in-new -p tcp ! --dport ${toString cfg.viewer.port} -j RETURN
          for i in lo ${lib.escapeShellArgs cfg.viewer.interfaces}; do
            ip46tables -A ${account}-in-new -i "$i" -j RETURN
          done
          ip46tables -A ${account}-in-new -p tcp -j REJECT --reject-with tcp-reset
          ip46tables -I OUTPUT -o lo -p tcp -j ${account}-out-new
          ip46tables -I INPUT -p tcp -j ${account}-in-new
          while ip46tables -D OUTPUT -o lo -p tcp -j ${account}-out 2>/dev/null; do :; done
          while ip46tables -D INPUT -p tcp -j ${account}-in 2>/dev/null; do :; done
          for c in ${account}-out ${account}-in; do
            ip46tables -F "$c" 2>/dev/null || true
            ip46tables -X "$c" 2>/dev/null || true
            ip46tables -E "$c-new" "$c"
          done
        '';
      })
    ];
    # The ruleset loads in one transaction, so a reload never opens them. A drop
    # in any base chain is final, whatever the firewall's own input accepts.
    networking.nftables.tables.${account} = lib.mkIf config.networking.nftables.enable {
      family = "inet";
      content = ''
        chain output {
          type filter hook output priority filter; policy accept;
          oifname "lo" tcp dport { ${lib.concatMapStringsSep ", " toString ports} } meta skuid != ${uid} reject with tcp reset
        }
        chain input {
          type filter hook input priority filter; policy accept;
          tcp dport ${toString cfg.viewer.port} iifname != { ${lib.concatMapStringsSep ", " (i: ''"${i}"'') ([ "lo" ] ++ cfg.viewer.interfaces)} } reject with tcp reset
        }
      '';
    };

    burrow.claude.machineMd = lib.mkOrder 850 ''
      ## The browser

      A Chromium runs here for ${config.burrow.owner}, signed in to their accounts by them,
      and the `browser` MCP server (Playwright) drives it. ${config.burrow.owner} watches it and
      signs in at ${if cfg.viewer.interfaces != [ ]
        then "http://${hostName}:${toString cfg.viewer.port}/vnc.html?autoconnect=true&resize=remote"
        else "localhost:${toString cfg.viewer.port}/vnc.html through `ssh -L ${toString cfg.viewer.port}:localhost:${toString cfg.viewer.port} ${hostName}`"}.

      - It's their browser, as them. Work in a tab of your own and close it after;
        leave theirs alone.
      - A site wants a login or a code: ask ${config.burrow.owner} to sign in at that address.
        Never ask for, type or keep a password or 2FA code.
      - Go at a person's pace on their own account: no scraping, bulk actions or
        loops. Sites like LinkedIn forbid automation and ban accounts for it.
      - Anything that publishes, sends, pays, accepts or deletes as them (a post,
        a message, a connection request, a purchase, saving a profile edit) needs
        their yes for that exact action first, unless they already approved the
        exact content. Filling in a form up to its button doesn't.
      - Its cookies and storage are their logins: never export, copy or print them.
    '';
  };
}
