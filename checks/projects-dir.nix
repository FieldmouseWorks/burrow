# burrow.projectsDir is made once, by common.nix, whatever else is on: with
# Remote Control on or off, and with the folder, the user and their group
# moved from the example's. Each machine has exactly one tmpfiles rule for
# the folder (rules lines and settings entries alike), 0755 and the user's.
{ self, pkgs, nixpkgs, ... }:
let
  inherit (nixpkgs) lib;
  machine = rc: custom: lib.nixosSystem {
    modules = [
      self.nixosModules.remote-control
      ../examples/machine.nix
      { burrow.remoteControl.enable = rc; }
      (lib.optionalAttrs custom {
        burrow = { projectsDir = "/srv/work"; user = "sam"; };
        users.users.sam = { isNormalUser = true; group = "crew"; };
        users.groups.crew = { };
      })
    ];
  };
  # Every rule the machine has for DIR: "TYPE MODE USER GROUP" each.
  rules = config: dir:
    let
      t = config.systemd.tmpfiles;
      lines = builtins.filter (l: builtins.elemAt l 1 == dir)
        (map (r: builtins.filter (f: f != "") (lib.splitString " " r))
          (builtins.filter (r: !(lib.hasPrefix "#" r) && r != "") t.rules));
      settings = lib.concatLists (lib.mapAttrsToList (_: paths:
        lib.mapAttrsToList (type: e: "${type} ${e.mode} ${e.user} ${e.group}") (paths.${dir} or { })) t.settings);
    in map (l: "${builtins.elemAt l 0} ${builtins.elemAt l 2} ${builtins.elemAt l 3} ${builtins.elemAt l 4}") lines ++ settings;
  cases = [
    { name = "Remote Control on"; rc = true; custom = false; dir = "/data/dev/src"; want = "d 0755 dev dev"; }
    { name = "Remote Control off"; rc = false; custom = false; dir = "/data/dev/src"; want = "d 0755 dev dev"; }
    { name = "Remote Control on, own folder, user and group"; rc = true; custom = true; dir = "/srv/work"; want = "d 0755 sam crew"; }
    { name = "Remote Control off, own folder, user and group"; rc = false; custom = true; dir = "/srv/work"; want = "d 0755 sam crew"; }
  ];
  failures = lib.concatMap (c:
    let got = rules (machine c.rc c.custom).config c.dir; in
    lib.optional (got != [ c.want ])
      "${c.name}: rules for ${c.dir}: [${lib.concatMapStringsSep ", " (r: "'${r}'") got}], want ['${c.want}']") cases;
in pkgs.runCommand "projects-dir-check" { } ''
  ${lib.concatMapStrings (f: "echo ${lib.escapeShellArg f}\n") failures}
  ${lib.optionalString (failures != [ ]) "exit 1"}
  touch $out
''
