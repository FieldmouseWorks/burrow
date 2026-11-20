# burrow.projectsDir is made once, by common.nix, whatever else is on: with
# proj on or off, and with the folder, the user and their group moved from
# the example's. Each machine has exactly one tmpfiles rule for the folder
# (rules lines and settings entries alike), 0755 and the user's; one whose
# burrow.user it doesn't declare has none, and still evaluates.
{ self, pkgs, nixpkgs, ... }:
let
  inherit (nixpkgs) lib;
  machine = proj: custom: extra: lib.nixosSystem {
    modules = [
      self.nixosModules.proj
      ../examples/machine.nix
      { burrow.proj.enable = proj; }
      (lib.optionalAttrs custom {
        burrow = { projectsDir = "/srv/work"; user = "sam"; };
        users.users.sam = { isNormalUser = true; group = "crew"; };
        users.groups.crew = { };
      })
      extra
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
    { name = "proj on"; proj = true; custom = false; dir = "/data/dev/src"; want = [ "d 0755 dev dev" ]; }
    { name = "proj off"; proj = false; custom = false; dir = "/data/dev/src"; want = [ "d 0755 dev dev" ]; }
    { name = "proj on, own folder, user and group"; proj = true; custom = true; dir = "/srv/work"; want = [ "d 0755 sam crew" ]; }
    { name = "proj off, own folder, user and group"; proj = false; custom = true; dir = "/srv/work"; want = [ "d 0755 sam crew" ]; }
    { name = "no such user"; proj = false; custom = false; dir = "/data/dev/src"; want = [ ]; extra = { burrow.user = "ghost"; }; }
  ];
  failures = lib.concatMap (c:
    let
      got = rules (machine c.proj c.custom (c.extra or { })).config c.dir;
      show = rs: "[${lib.concatMapStringsSep ", " (r: "'${r}'") rs}]";
    in lib.optional (got != c.want) "${c.name}: rules for ${c.dir}: ${show got}, want ${show c.want}") cases;
in pkgs.runCommand "projects-dir-check" { } ''
  ${lib.concatMapStrings (f: "echo ${lib.escapeShellArg f}\n") failures}
  ${lib.optionalString (failures != [ ]) "exit 1"}
  touch $out
''
