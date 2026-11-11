# dashboard: the machine's own web app, for its owner's devices, on the
# tailnet alone (crates/burrow-dashboard). The service listens on localhost;
# `tailscale serve` puts it on the machine's ts.net name over HTTPS, with
# the tailnet's own certificate. No public name, no open port, no ACME
# token. The ts.net name is the WebAuthn RP ID, so it's picked once: every
# passkey is bound to it.
#
# One owner signs in, with a passkey. A device is added with a one-time code
# printed on the machine (`sudo burrow-dashboard enroll`), as many devices as
# the owner likes. The app installs from the browser (Safari on iOS
# included), and its service worker keeps the shell for when it's offline.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.dashboard;
  inherit (lib) mkOption types;
  account = "burrow-dashboard";
  stateDir = "${config.burrow.stateDir}/dashboard";
  # Where browsers reach it: what they send as Origin, and WebAuthn's
  # expected origin. The RP ID is the host alone (crates' Config::rp_id).
  origin = "https://${cfg.host}" + lib.optionalString (cfg.serve.httpsPort != 443) ":${toString cfg.serve.httpsPort}";

  # What crates/burrow-dashboard reads (its Config).
  dashboardConfig = pkgs.writeText "burrow-dashboard.json" (builtins.toJSON {
    inherit (cfg) title port;
    inherit origin;
    state = stateDir;
    # The crate's assets/ alone, so the config changes only with them.
    shell = builtins.path { path = ../crates/burrow-dashboard/assets; name = "burrow-dashboard-shell"; };
    fonts = import ./dashboard/fonts.nix { inherit pkgs; };
    owner = { inherit (cfg.owner) id login; };
    idle_timeout = cfg.idleTimeout;
    max_age = cfg.maxAge;
  });

  # `burrow-dashboard serve` and `burrow-dashboard enroll`, the config built in.
  dashboard = pkgs.runCommand account {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = account;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-dashboard}/bin/burrow-dashboard \
      $out/bin/${account} --add-flags ${dashboardConfig}
  '';

  tailscale = lib.getExe config.services.tailscale.package;
  serveArgs = [ "--https=${toString cfg.serve.httpsPort}" ];
in
{
  imports = [
    (import ./common.nix inputs)
    ./updates/health-options.nix
  ];

  options.burrow.dashboard = {
    enable = lib.mkEnableOption "the machine's dashboard, on the tailnet, for its owner's passkeys";

    host = mkOption {
      type = types.str;
      example = "burrow.tail1234.ts.net";
      description = ''
        The machine's ts.net name, where the dashboard is reached and the
        WebAuthn RP ID its passkeys are bound to. Change it and every passkey
        has to be enrolled again, so pick it once.
      '';
    };

    owner = {
      id = mkOption {
        type = types.ints.positive;
        example = 583231;
        description = ''
          The owner's GitHub account id (`gh api user --jq .id`), which never
          changes: the passkeys' user handle.
        '';
      };
      login = mkOption {
        type = types.str;
        example = "sam";
        description = "The owner's GitHub login, as passkey managers and the dashboard name them.";
      };
    };

    title = mkOption {
      type = types.str;
      default = config.burrow.prefix;
      defaultText = lib.literalExpression "config.burrow.prefix";
      description = "What the app is called, on its pages and the home screen.";
    };

    port = mkOption {
      type = types.port;
      default = 8473;
      description = "The localhost port the service listens on, for `tailscale serve` to forward to.";
    };

    idleTimeout = mkOption {
      type = types.ints.positive;
      default = 12 * 60 * 60;
      defaultText = lib.literalExpression "12 * 60 * 60";
      description = "Seconds a session lasts unused.";
    };

    maxAge = mkOption {
      type = types.ints.positive;
      default = 7 * 24 * 60 * 60;
      defaultText = lib.literalExpression "7 * 24 * 60 * 60";
      description = "Seconds a session lasts at most, however much it's used.";
    };

    serve = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Put the dashboard on the tailnet with `tailscale serve`. It needs
          services.tailscale and HTTPS certificates on for the tailnet (its
          admin console, DNS). Off, the dashboard is on localhost alone.
        '';
      };
      httpsPort = mkOption {
        type = types.port;
        default = 443;
        description = ''
          The port on the ts.net name it's served at: the dashboard is
          `https://HOST` on 443, `https://HOST:PORT` on any other.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = !cfg.serve.enable || config.services.tailscale.enable;
      message = "burrow.dashboard.serve puts the dashboard on the tailnet: it needs services.tailscale.enable.";
    }];

    users.users.${account} = {
      isSystemUser = true;
      group = account;
    };
    users.groups.${account} = { };

    # The passkeys, sessions and the pending code: the service's alone.
    systemd.tmpfiles.rules = [ "d ${stateDir} 0700 ${account} ${account} -" ];

    environment.systemPackages = [ dashboard ];

    systemd.services.${account} = {
      description = "The machine's dashboard (burrow.dashboard)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe dashboard} serve";
        User = account;
        Group = account;
        Restart = "always";
        RestartSec = 2;
        UMask = "0077";
        # It talks to localhost and nothing else, at the socket.
        IPAddressAllow = "localhost";
        IPAddressDeny = "any";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        ProtectSystem = "strict";
        ReadWritePaths = [ stateDir ];
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged @resources" ];
        MemoryMax = "256M";
        MemorySwapMax = 0;
      };
    };

    # The tailnet's way in: HTTPS on the ts.net name, to localhost. tailscaled
    # keeps it, so this sets it at each start and takes it away at stop.
    systemd.services."${account}-serve" = lib.mkIf cfg.serve.enable {
      description = "The dashboard on the tailnet (tailscale serve)";
      wantedBy = [ "multi-user.target" ];
      wants = [ "tailscaled.service" "${account}.service" ];
      after = [ "tailscaled.service" "${account}.service" ];
      # tailscaled may not be logged in yet at boot.
      startLimitIntervalSec = 600;
      startLimitBurst = 10;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        # It waits for HTTPS to be turned on for the tailnet rather than fail.
        TimeoutStartSec = 60;
        Restart = "on-failure";
        RestartSec = 15;
        ExecStart = lib.escapeShellArgs ([ tailscale "serve" "--bg" "--yes" ] ++ serveArgs ++ [ "http://127.0.0.1:${toString cfg.port}" ]);
        ExecStop = lib.escapeShellArgs ([ tailscale "serve" ] ++ serveArgs ++ [ "off" ]);
      };
    };

    burrow.updates.health.dashboard = ''
      ${lib.getExe pkgs.curl} -fsS -o /dev/null http://127.0.0.1:${toString cfg.port}/login
    '';
  };
}
