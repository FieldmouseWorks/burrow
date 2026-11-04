# The gate's plan, which gate.sh splices into each evaluation. With
# eval-apart, what `nix flake check` checks of the outputs it knows,
# given the flake's outputs as `nix eval` evaluates the checkout's
# snapshot (purely, with the same source and self as nix flake
# check, a checkout with uncommitted changes included). With no
# entry, the plan: the outputs it can't check or doesn't know, and
# each derivation it evaluates, whether it's a check and whether
# it's a NixOS system. With an entry's index, that one: for a NixOS
# system nothing, once its toplevel is a derivation; for the rest
# its derivation's path, once it's a derivation as nix flake check
# reads one (a name that's a string without context) with a drvPath
# that's a store derivation's path. With builds, every check to
# build but those in skip, as a list: in an attrset nix-instantiate
# passes over any name that isn't an identifier, a.b or "sp ace",
# without a word.

{ system, entry ? null, builds ? false, skip ? "" }: outputs:
let
  known = [ "checks" "packages" "devShells" "formatter" "nixosConfigurations" "legacyPackages" "nixosModules" "templates" ];
  # What nix flake check checks that this doesn't.
  refused = [ "apps" "defaultPackage" "defaultApp" "devShell" "overlay" "overlays" "nixosModule" "hydraJobs" "defaultTemplate" "bundlers" "defaultBundler" ];
  # What nix flake check only forces, and doesn't warn of.
  community = [ "lib" "darwinConfigurations" "darwinModules" "flakeModule" "flakeModules" "herculesCI" "homeConfigurations" "homeModule" "homeModules" "nixopsConfigurations" ];
  names = builtins.attrNames outputs;
  set = what: v: if builtins.isAttrs v then v else throw "flake output '${what}' is not an attribute set";
  # Every output is forced, and a NixOS module is only forced.
  forced = builtins.foldl' (a: n: builtins.seq outputs.${n} a) true names;
  modules = builtins.mapAttrs (_: m: builtins.seq m true) (set "nixosModules" (outputs.nixosModules or { }));
  template = n: t:
    let
      what = "templates.${n}";
      extra = builtins.filter (a: !builtins.elem a [ "path" "description" "welcomeText" ]) (builtins.attrNames t);
    in
    if !(t ? path) then throw "template '${what}' lacks attribute 'path'"
    else if !builtins.pathExists t.path then throw "template '${what}' refers to a non-existent path '${toString t.path}'"
    else if !(t ? description) then throw "template '${what}' lacks attribute 'description'"
    else if !builtins.isString t.description || builtins.hasContext t.description then throw "the description of template '${what}' is not a string without context"
    else if extra != [ ] then throw "template '${what}' has unsupported attribute '${builtins.head extra}'"
    else true;
  templates = builtins.mapAttrs (n: t: template n (set "templates.${n}" t)) (set "templates" (outputs.templates or { }));
  # An output by system: every system's name has a dash, and only
  # this system's outputs are checked further.
  here = name: f:
    let
      systems = map
        (s: if builtins.length (builtins.split "-" s) > 1 then s
          else throw "'${s}' in flake output '${name}' is not a valid system type")
        (builtins.attrNames (set name outputs.${name}));
    in
    if !(outputs ? ${name}) then [ ]
    else builtins.deepSeq systems (if builtins.elem system systems then f outputs.${name}.${system} else [ ]);
  each = name: here name (s: map
    (n: { what = "${name}.${system}.${n}"; path = [ name system n ]; check = name == "checks"; inherit n; })
    (builtins.attrNames (set "${name}.${system}" s)));
  entries =
    each "checks" ++ each "packages" ++ each "devShells"
    ++ here "formatter" (_: [ { what = "formatter.${system}"; path = [ "formatter" system ]; } ])
    ++ map
      (n: { what = "nixosConfigurations.${n}"; path = [ "nixosConfigurations" n "config" "system" "build" "toplevel" ]; nixos = true; })
      (builtins.attrNames (set "nixosConfigurations" (outputs.nixosConfigurations or { })));
  skipped = builtins.filter (s: builtins.isString s && s != "") (builtins.split " " skip);
  build = e: e.check or false && !builtins.elem e.n skipped;
  isDerivation = v: (v.type or null) == "derivation";
  store = builtins.storeDir + "/";
  # A store derivation's hash and name, as nix reads a store path's
  # (a name of at most 211 characters; ending in .drv, it's never
  # "." or "..").
  drvName = "[0-9a-df-np-sv-z]{32}-[A-Za-z0-9+._?=-]+[.]drv";
  drvPath = e: v:
    if !isDerivation v then throw "flake attribute '${e.what}' is not a derivation"
    else if !(v ? name) then throw "derivation name missing in ${e.what}"
    else if !builtins.isString v.name || builtins.hasContext v.name then throw "the name of ${e.what} is not a string without context"
    else if !(v ? drvPath) then throw "${e.what} has no drvPath"
    else
      let
        # As nix coerces a drvPath, copying nothing: a string as it
        # is, a path in the store as it is (one outside it would be
        # copied in, never as a .drv), an attribute set by what its
        # __toString gives, or else by its outPath, coerced the same
        # way.
        coerce = x:
          if builtins.isPath x then toString x
          else if builtins.isAttrs x && x ? __toString then coerce (x.__toString x)
          else if builtins.isAttrs x && x ? outPath then coerce x.outPath
          else "${x}";
        p = coerce v.drvPath;
        name = builtins.substring (builtins.stringLength store) (builtins.stringLength p) p;
      in
      if builtins.substring 0 (builtins.stringLength store) p != store
        || builtins.match drvName name == null || builtins.stringLength name > 33 + 211
      then throw "the drvPath of ${e.what} is not a store derivation's path"
      else p;
in
if builds then
  map (e: builtins.foldl' (v: n: v.${n}) outputs e.path) (builtins.filter build entries)
else if entry == null then
  # legacyPackages has only its system names checked.
  builtins.deepSeq [ (here "legacyPackages" (_: [ ])) forced modules templates ] {
    other = builtins.filter (n: builtins.elem n refused) names;
    unknown = builtins.filter (n: !builtins.elem n (known ++ refused ++ community)) names;
    entries = map (e: { inherit (e) what; check = e.check or false; name = e.n or null; nixos = e.nixos or false; }) entries;
  }
else
  let
    e = builtins.elemAt entries entry;
    v = builtins.foldl' (v: n: v.${n}) outputs e.path;
  in
  if e.nixos or false then
    (if isDerivation v then { } else throw "'config.system.build.toplevel' of ${e.what} is not a derivation")
  else { drv = drvPath e v; }
