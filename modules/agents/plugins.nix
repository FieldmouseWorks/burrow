# Running engines' plugins in the sandbox (crates/burrow-agent's plugin.rs):
# for each engine with a plugin, its own image, socket and service, and the
# executable a Conary runner is given. What a plugin is granted is decided
# there, from its describe and the engine's approved list; this gives the
# machine's values and nothing else.
{ inputs, config, lib, pkgs, guest }:
let
  cfg = config.burrow.agents;
  inherit (config.burrow) user;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-agent;
  withPlugin = lib.filterAttrs (_: engine: engine.plugin != null) cfg.engines;

  storePaths = roots: "${pkgs.closureInfo { rootPaths = roots; }}/store-paths";
  bundle = name: paths: rec {
    root = pkgs.buildEnv {
      name = "agent-plugin-${name}";
      inherit paths;
      pathsToLink = [ "/bin" ];
      ignoreCollisions = true;
    };
    closure = storePaths [ root ];
  };
  toolbox = import ../toolbox/packages.nix pkgs;
  socketOf = name: "/run/agent-plugin/${name}.sock";
  callDir = name: "agent-plugin-run/${name}-%i";
in
{
  # An engine's part of the agent's config.
  engine = name: engine:
    if engine.plugin == null then null
    else
      let
        system = guest [ (import ./plugin-guest.nix { inherit package; program = lib.getExe engine.plugin; }) ] engine;
      in {
        runner = system.config.microvm.declaredRunner;
        closure = storePaths [ system.config.system.build.toplevel ];
        socket = socketOf name;
      };

  # The machine's part of it.
  host = {
    system = bundle "system" (toolbox.system ++ [ pkgs.curl pkgs.jq config.programs.git.package ]);
    toolchains = bundle "toolchains" toolbox.toolchains;
    nix = bundle "nix" [ config.nix.package ];
    virtiofsd = {
      root = pkgs.virtiofsd;
      closure = storePaths [ pkgs.virtiofsd ];
    };
    # QEMU starts it by a link named for the service, in the VM's directory,
    # beside that service's socket (one word is all a forward can be given).
    forward = pkgs.writeShellScript "agent-plugin-service" ''
      exec ${pkgs.socat}/bin/socat -t 300 FD:0 UNIX-CONNECT:"''${0##*/}.sock"
    '';
    socat = "/run/current-system/sw/bin/socat";
    heavy = if config.burrow.cpu.enable or false then lib.getExe config.burrow.tools.heavy else null;
    unitsClear =
      if config.burrow.watchdog.units.enable or false
      then config.burrow.watchdog.units.clearSocket
      else null;
    jobSockets = "${config.burrow.scratchDir}/jobs";
  };

  # agent-plugin-NAME: no arguments, the plugin contract on stdio.
  executables = agent: lib.mapAttrs (name: _: pkgs.runCommand "agent-plugin-${name}" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "agent-plugin-${name}";
  } ''
    makeBinaryWrapper ${lib.getExe agent} $out/bin/agent-plugin-${name} --add-flags "plugin ${name}"
  '') withPlugin;

  sockets = lib.mapAttrs' (name: _: lib.nameValuePair "agent-plugin-${name}" {
    description = "The door to ${name}'s plugin, for ${user} alone";
    wantedBy = [ "sockets.target" ];
    listenStreams = [ (socketOf name) ];
    socketConfig = {
      Accept = true;
      SocketUser = user;
      SocketMode = "0600";
      DirectoryMode = "0755";
      MaxConnections = 16;
    };
  }) withPlugin;

  # A call is a connection: when it ends, so does everything the call
  # started (the unit's cgroup), and then its clone's work is saved and the
  # clone deleted.
  services = agent: lib.mapAttrs' (name: _: lib.nameValuePair "agent-plugin-${name}@" {
    description = "One call of ${name}'s plugin, in a sandbox";
    after = [ "agents-dataset.service" ];
    restartIfChanged = false;
    serviceConfig = {
      ExecStart = "${lib.getExe agent} _plugin ${name} /run/${callDir name}";
      ExecStopPost = "${lib.getExe agent} _plugin-end /run/${callDir name}";
      StandardInput = "socket";
      StandardOutput = "socket";
      StandardError = "journal";
      SyslogIdentifier = "agent-plugin-${name}";
      RuntimeDirectory = callDir name;
      RuntimeDirectoryMode = "0711";
      UMask = "0002";
      LimitNOFILE = 1048576;
      LimitMEMLOCK = "infinity";
      # Longer than saving a clone's work may take (src/confine.rs).
      TimeoutStopSec = "11min";
    };
  }) withPlugin;
}
