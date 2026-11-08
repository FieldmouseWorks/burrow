# burrow.lib.project: a project's dev shell and its gate, so each repo's
# flake.nix is a few lines and gets better when burrow does (Renovate bumps
# the burrow input, pinned by commit as templates/project/flake.nix has it).
#
#   inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=COMMIT";
#   outputs = { self, burrow }: burrow.lib.project {
#     inherit self;
#     rust = { };                      # or { dir = "client"; }
#     python = { };
#   };
#
# `nix develop` gets the toolchains; `nix flake check` is the gate: what it
# runs is the same on a laptop, in an agent's sandbox and in CI. The checks
# are hermetic (the Nix sandbox, no network). A repo's heavier gates (VM
# replays, browsers, databases) stay its own and run in `nix develop`. A Rust
# workspace's binaries are its packages.default, for machines to install.
# Every project's gate also looks for secrets in its tracked files.
{ nixpkgs, rust-overlay, crane }:
{
  self,
  systems ? [ "x86_64-linux" "aarch64-linux" ],
  # Rust: fmt, clippy (all targets, warnings are errors) and the tests of the
  # Cargo workspace in DIR, built with the toolchain DIR/rust-toolchain.toml
  # (or the repo root's) pins; latest stable without one. `test = false` skips
  # the tests (say, when they need a database); `testInputs = pkgs: [ pkgs.git ]`
  # puts tools the tests run on their PATH. `include = [ "docs/spec.md" ]`
  # names files outside DIR, from the repo's root, that the tests read: the
  # checks then build from the root with DIR and only those, so a test finds
  # each where a checkout has it. A workspace that links native
  # libraries names them for every build, check and the dev shell:
  # `nativeBuildInputs = pkgs: [ pkgs.pkg-config ]` for the tools its build
  # scripts run, `buildInputs = pkgs: [ pkgs.alsa-lib ]` for the libraries.
  # `members = true` (a workspace with no root package) makes each member
  # with a binary a package of its own, named as its crate and built from
  # only the members it reaches by path, with the lockfile cut down to what
  # they use: what runs one program rebuilds only for a change that reaches
  # it. packages.default then joins them.
  rust ? null,
  # Python: ruff's error rules (syntax errors, undefined names) over DIRS.
  # `format = true` also holds them to `ruff format`.
  python ? null,
  # Node: the current LTS in the dev shell. Its checks stay in the repo's own
  # CI, which needs node_modules.
  node ? null,
  # More dev shell tools: pkgs: [ pkgs.godot ].
  packages ? (_: [ ]),
  # More hermetic checks: pkgs: { name = derivation; }. A check that reads
  # test files without running them all says which it runs, as git glob
  # pathspecs from the repo's root: `passthru.testFiles = [ "server/**" ]`,
  # or `[ ]` for one that runs none (a formatter, a lint). The negative run
  # (flake-check.yml) counts a new test as passing on the old code only
  # where a check that runs it passed; one that doesn't say runs every test
  # it reads.
  checks ? (_: { }),
}:
let
  lib = nixpkgs.lib;
  forAll = f: lib.genAttrs systems (system: f (import nixpkgs {
    inherit system;
    overlays = [ rust-overlay.overlays.default ];
  }));

  rustCfg = {
    dir = ".";
    test = true;
    testInputs = _: [ ];
    include = [ ];
    members = false;
    nativeBuildInputs = _: [ ];
    buildInputs = _: [ ];
  } // (if rust == null then { } else rust);
  pyCfg = { dirs = [ "." ]; format = false; } // (if python == null then { } else python);
  rustDir = if rustCfg.dir == "." then self else self + "/${rustCfg.dir}";

  # What the repo's rust-toolchain(.toml) asks for, as rust-overlay's
  # fromRustupToolchain takes it; without one, latest stable with rustfmt and
  # clippy and nothing else (rustup's default profile would add 700 MiB of
  # offline docs to every build that needs the toolchain).
  toolchainSpec =
    let
      file = lib.findFirst builtins.pathExists null [
        (rustDir + "/rust-toolchain.toml")
        (rustDir + "/rust-toolchain")
        (self + "/rust-toolchain.toml")
        (self + "/rust-toolchain")
      ];
      text = builtins.readFile file;
      # The legacy rust-toolchain file is either TOML or just the channel.
      spec =
        if lib.hasInfix "[toolchain]" text then (builtins.fromTOML text).toolchain
        else { channel = lib.trim text; };
    in
    { components = [ ]; } // (if file != null then spec else { channel = "stable"; profile = "minimal"; components = [ "rustfmt" "clippy" ]; });

  # extra: components on top of the repo's own (the shell's rust-analyzer).
  toolchainFor = pkgs: extra:
    pkgs.rust-bin.fromRustupToolchain (toolchainSpec // {
      components = toolchainSpec.components ++ extra;
    });

  craneFor = pkgs: (crane.mkLib pkgs).overrideToolchain (toolchainFor pkgs [ ]);

  # The workspace's binaries, built for release: packages.default, what a
  # machine installs.
  rustPackage = pkgs: (craneFor pkgs).buildPackage {
    src = packageSource pkgs;
    strictDeps = true;
    nativeBuildInputs = rustCfg.nativeBuildInputs pkgs;
    buildInputs = rustCfg.buildInputs pkgs;
    cargoExtraArgs = "--locked --workspace";
    # The gate runs the tests.
    doCheck = false;
  };

  # rust.members: each member with a binary, built alone. Its workspace is
  # the member and those it reaches by path (dev and build dependencies
  # too, since cargo loads them), the root manifest kept but for the other
  # members and the shared dependencies none of these use; its lockfile is
  # what those members reach through Cargo.lock's own dependency lists. Both
  # are written from what evaluation reads, never paths into the repo, so
  # only the member's sources and what it uses are its inputs.
  readToml = path: builtins.fromTOML (builtins.readFile path);
  rootToml = readToml (rustDir + "/Cargo.toml");
  lockToml = readToml (rustDir + "/Cargo.lock");
  memberDirs =
    let dirs = rootToml.workspace.members or [ ]; in
    if rootToml ? package then throw "rust.members: the workspace in ${rustCfg.dir} has a root package; members wants one without"
    else if lib.any (d: builtins.match ".*[*?[].*" d != null) dirs then throw "rust.members: name each member in ${rustCfg.dir}/Cargo.toml, not by a glob"
    else dirs;
  memberToml = dir: readToml (rustDir + "/${dir}/Cargo.toml");
  # A manifest's dependency tables, by kind and by target.
  depTables = toml:
    lib.concatMap (t: map (kind: t.${kind} or { }) [ "dependencies" "dev-dependencies" "build-dependencies" ])
      ([ toml ] ++ lib.attrValues (toml.target or { }));
  # PATH, as DIR's manifest gives it, from the workspace's root.
  fromRoot = dir: path: lib.concatStringsSep "/" (lib.foldl'
    (parts: part:
      if part == "." || part == "" then parts
      else if part != ".." then parts ++ [ part ]
      else if parts == [ ] then throw "rust.members: ${dir} depends on ${path}, outside the workspace"
      else lib.init parts)
    [ ] (lib.splitString "/" "${dir}/${path}"));
  # The members DIR depends on: by a path of its own, or by the workspace's
  # for a dependency it inherits.
  memberDeps = dir: lib.concatMap (t: lib.concatLists (lib.mapAttrsToList (name: d:
    let inherited = rootToml.workspace.dependencies.${name} or { }; in
    if !builtins.isAttrs d then [ ]
    else if d ? path then [ (fromRoot dir d.path) ]
    else if (d.workspace or false) && builtins.isAttrs inherited && inherited ? path then [ (fromRoot "." inherited.path) ]
    else [ ]) t)) (depTables (memberToml dir));
  closureOf = dir: map (x: x.key) (builtins.genericClosure {
    startSet = [ { key = dir; } ];
    operator = x: map (key: { inherit key; }) (memberDeps x.key);
  });
  manifestFor = dirs:
    let
      shared = lib.concatMap (d: lib.concatMap
        (t: lib.attrNames (lib.filterAttrs (_: v: builtins.isAttrs v && (v.workspace or false)) t))
        (depTables (memberToml d))) dirs;
      ws = rootToml.workspace;
    in
    rootToml // {
      workspace = builtins.removeAttrs ws [ "default-members" "exclude" ] // { members = dirs; }
        // lib.optionalAttrs (ws ? dependencies) { dependencies = lib.getAttrs (lib.unique shared) ws.dependencies; };
    };
  # A lock entry's dependency ("NAME", "NAME VERSION" or "NAME VERSION
  # (SOURCE)", whatever tells it apart) as its entry.
  lockByName = lib.groupBy (p: p.name) lockToml.package;
  lockKey = p: "${p.name} ${p.version} ${p.source or ""}";
  lockDep = spec:
    let
      parts = lib.splitString " " spec;
      at = n: builtins.elemAt parts n;
      found = builtins.filter
        (p: (builtins.length parts < 2 || p.version == at 1) && (builtins.length parts < 3 || "(${p.source or ""})" == at 2))
        (lockByName.${at 0} or [ ]);
    in
    if builtins.length found == 1 then builtins.head found
    else throw "rust.members: Cargo.lock has ${toString (builtins.length found)} entries for ${spec}";
  lockFor = dirs:
    let
      names = map (d: (memberToml d).package.name) dirs;
      reached = builtins.genericClosure {
        startSet = map (p: { key = lockKey p; inherit p; })
          (builtins.filter (p: !(p ? source) && builtins.elem p.name names) lockToml.package);
        operator = x: map (p: { key = lockKey p; inherit p; }) (map lockDep (x.p.dependencies or [ ]));
      };
      kept = lib.genAttrs (map (x: x.key) reached) (_: true);
    in
    lockToml // { package = builtins.filter (p: kept ? ${lockKey p}) lockToml.package; };
  # What the packages are built from: the workspace's Cargo sources, less
  # its tests (cargoTests). A member's package takes its own part of it.
  packageSource = pkgs: lib.cleanSourceWith {
    src = (craneFor pkgs).cleanCargoSource rustDir;
    name = "source";
    filter = path: type: !(cargoTests (toString path) type);
  };
  # Whether PATH (a source file's absolute path) of TYPE is tests that
  # Cargo builds a package's library and programs without, which a package
  # leaves out of its source: it runs no test (doCheck = false), so a change
  # to a test alone leaves the package, and every check built from it, as it
  # was. The checks that run tests (rustChecks) take the whole directory,
  # and the negative run (flake-check.yml) builds only the checks a PR's
  # tests change, so it builds those, not every machine test that runs the
  # programs. Only what is plainly tests goes, and whatever a build could
  # need stays:
  # - a package's integration tests: the `tests` directory beside its
  #   manifest;
  # - a module whatever its name (its file, its mod.rs, its directory of
  #   modules, or the file a #[path] names) that every declaration reading
  #   it marks #[cfg(test)] (`#[cfg(test)] mod tests;`,
  #   `#[cfg(test)] #[path = "sweep_tests.rs"] mod tests;`), which Cargo
  #   drops before reading its file, and nothing a declaration without it
  #   reads (a program in src/bin named tests has no module declaring it);
  # and neither when it is, or holds, a target's file (a test target's
  # included: Cargo checks a declared target's file is there), anything in
  # a directory Cargo finds targets in by itself (src/bin, examples,
  # benches), or a crate, nor anywhere in a package with a build script or
  # whose Rust, outside the test modules themselves, has any include!,
  # include_str! or include_bytes! at all (their argument can be computed),
  # a path attribute naming tests, or one inside a cfg_attr (whose reads
  # can't be told from here). What no reading
  # of the text can see (a macro from another crate that includes a file
  # named at the call, a proc macro reading one) the rust-package-sources
  # check sees: the build itself, from the whole directory, says which
  # files it read.
  cargoTests = path: type:
    let
      name = baseNameOf path;
      dir = dirOf path;
      at = pathOf;
      # The package it's in: the nearest directory, its own included, whose
      # manifest has a [package].
      owner = d:
        if cargoPackages ? ${d} then cargoPackages.${d}
        else if d == rustRoot || !(lib.hasPrefix "${rustRoot}/" d) then null
        else owner (dirOf d);
      pkg = owner (builtins.unsafeDiscardStringContext dir);
      integration = name == "tests" && type == "directory" && dir == pkg.dir;
      module = (type == "directory" || lib.hasSuffix ".rs" name) && pkg.modules.${path} or false;
    in
    pkg != null && (integration || module) && pkg.prunable
    && !(lib.any (t: t == path || lib.hasPrefix "${path}/" t) (pkg.targets ++ pkg.needed))
    && !(lib.any (d: lib.hasPrefix "${pkg.dir}/${d}/" path) [ "src/bin" "examples" "benches" ])
    && !(type == "directory" && lib.any (f: baseNameOf f == "Cargo.toml") (lib.filesystem.listFilesRecursive (at path)));
  # Every package Cargo loads, by its directory's absolute path, with its
  # targets' files and build script (as absolute paths, `..` resolved) and
  # whether cargoTests may leave anything of it out. Cargo loads the root
  # manifest, the workspace's members (globs matched as Cargo does, less its
  # exclude), and whatever any of those depends on by path, its own or the
  # workspace's; any other Cargo.toml (a fixture) is just a file.
  rustRoot = builtins.unsafeDiscardStringContext (toString rustDir);
  # An absolute path under rustRoot, as a path into rustDir: read through
  # it as Nix read the source, whatever the file's name.
  pathOf = p: if p == rustRoot then rustDir else rustDir + "/${lib.removePrefix "${rustRoot}/" p}";
  # PATH, relative to DIR, as an absolute path, with `.` and `..` resolved.
  resolvePath = dir: rel: "/" + lib.concatStringsSep "/" (lib.foldl'
    (acc: part: if part == "" || part == "." then acc else if part == ".." then lib.init acc else acc ++ [ part ])
    [ ] (lib.splitString "/" "${dir}/${rel}"));
  cargoDirs =
    let
      ws = rootToml.workspace or { };
      # Cargo's member globs as regexes over a directory's path from the
      # root, with a trailing slash: `*`, `?`, `[...]` and `[!...]` within
      # a part, `**` for any number of parts, everything else literal, and
      # `.` and `..` parts resolved first. Matched against every directory
      # with a Cargo.toml, so a branch the glob doesn't reach is no error.
      globRegex = pattern:
        let
          parts = lib.foldl' (acc: p: if p == "" || p == "." then acc else if p == ".." then lib.init acc else acc ++ [ p ])
            [ ] (lib.splitString "/" pattern);
          # One character on: st is what's made so far, whether it's in a
          # class, and whether the class has nothing in it yet (where `!`
          # negates and `]` is itself, as a POSIX bracket takes it first).
          char = st: c:
            let add = x: st // { out = st.out + x; fresh = false; }; in
            if st.inClass then
              (if c == "]" && !st.fresh then add "]" // { inClass = false; }
              else if c == "!" && st.fresh && lib.hasSuffix "[" st.out then st // { out = st.out + "^"; }
              # A class's characters are its own: a POSIX bracket has no escapes.
              else add c)
            else if c == "[" then st // { out = st.out + "["; inClass = true; fresh = true; }
            else if c == "*" then add "[^/]*"
            else if c == "?" then add "[^/]"
            else if builtins.elem c [ "." "+" "(" ")" "{" "}" "|" "^" "$" "\\" "]" ] then add ("\\" + c)
            else add c;
          part = p: if p == "**" then "([^/]+/)*"
            else (lib.foldl' char { inClass = false; fresh = false; out = ""; } (lib.stringToCharacters p)).out + "/";
        in
        lib.concatStrings (map part parts);
      manifestDirs = map (f: builtins.unsafeDiscardStringContext (toString (dirOf f)))
        (builtins.filter (f: baseNameOf f == "Cargo.toml") (lib.filesystem.listFilesRecursive rustDir));
      relOf = d: lib.removePrefix "${rustRoot}/" d + "/";
      excluded = map (e: relOf (resolvePath rustRoot e)) (ws.exclude or [ ]);
      members = builtins.filter
        (d: d != rustRoot && lib.any (g: builtins.match (globRegex g) (relOf d) != null) (ws.members or [ ])
          && !(lib.any (e: lib.hasPrefix e (relOf d)) excluded))
        manifestDirs;
      # The directories DIR's manifest depends on by path.
      pathDeps = dir: lib.concatMap (t: lib.concatLists (lib.mapAttrsToList (name: d:
        let inherited = ws.dependencies.${name} or { }; in
        if !builtins.isAttrs d then [ ]
        else if d ? path then [ (resolvePath dir d.path) ]
        else if (d.workspace or false) && builtins.isAttrs inherited && inherited ? path then [ (resolvePath rustRoot inherited.path) ]
        else [ ]) t)) (depTables (readToml (pathOf "${dir}/Cargo.toml")));
    in
    map (x: x.key) (builtins.genericClosure {
      startSet = map (key: { inherit key; }) ([ rustRoot ] ++ members);
      # One outside the directory isn't built from it.
      operator = x: map (key: { inherit key; }) (builtins.filter (d: lib.hasPrefix "${rustRoot}/" d) (pathDeps x.key));
    });
  cargoPackages =
    let
      info = dir:
        let
          toml = readToml (pathOf "${dir}/Cargo.toml");
          resolve = resolvePath dir;
          kinds = { bin = "src/bin"; example = "examples"; bench = "benches"; test = "tests"; };
          targets = lib.concatLists (lib.mapAttrsToList (k: d: lib.concatMap
            (t: if t ? path then [ (resolve t.path) ] else [ (resolve "${d}/${t.name}.rs") (resolve "${d}/${t.name}/main.rs") ])
            (toml.${k} or [ ])) kinds)
            ++ lib.optional (toml.lib.path or null != null) (resolve toml.lib.path)
            ++ lib.optional (builtins.isString (toml.package.build or null)) (resolve toml.package.build);
          build = builtins.pathExists (pathOf "${dir}/build.rs") && (toml.package.build or true) != false
            || builtins.isString (toml.package.build or null);
          names = builtins.filter (f: lib.hasSuffix ".rs" (toString f)) (lib.filesystem.listFilesRecursive (pathOf dir));
          # A test module's own reads don't count: Cargo builds the package
          # without reading it.
          includes = lib.any (r: !(modules.${builtins.unsafeDiscardStringContext (toString r)} or false) && builtins.length (builtins.split
            "include(_str|_bytes)?[[:space:]]*!|[^[:alnum:]_]path[[:space:]]*=[[:space:]]*\"([^\"]*/)?tests(/|[.]rs\")|cfg_attr[(][^]]*[^[:alnum:]_]path[[:space:]]*="
            (builtins.readFile r)) > 1) names;
          # Every `mod NAME;` in its Rust, with the attributes just before it,
          # as what it reads (the file, its mod.rs, and the directory of its
          # own modules; a #[path]'s file, relative to the declaring file's
          # directory) and whether one of those attributes is #[cfg(test)].
          # Beside a crate root or mod.rs a module's files are its siblings;
          # beside FILE.rs, in FILE/.
          declarations = lib.concatMap (r:
            let
              file = builtins.unsafeDiscardStringContext (toString r);
              here = dirOf file;
              base = baseNameOf file;
              root = builtins.elem base [ "mod.rs" "lib.rs" "main.rs" ] || builtins.elem file targets;
              children = if root then here else "${here}/${lib.removeSuffix ".rs" base}";
            in
            map (m:
              let
                attrs = builtins.elemAt m 0;
                name = builtins.elemAt m 4;
                pathed = builtins.match ".*#[[]path[[:space:]]*=[[:space:]]*\"([^\"]*)\"[]].*" attrs;
              in
              {
                test = builtins.match ".*#[[]cfg[(]test[)][]].*" attrs != null;
                reads = if pathed != null then [ (resolvePath here (builtins.head pathed)) ]
                  else [ "${children}/${name}.rs" "${children}/${name}/mod.rs" "${children}/${name}" ];
              })
              (builtins.filter builtins.isList (builtins.split
                "((#[[][^]]*[]][[:space:]]*)*)(pub([(][^)]*[)])?[[:space:]]+)?mod[[:space:]]+([[:alnum:]_]+)[[:space:]]*;"
                (builtins.readFile r))))
            names;
          # Each path some declaration reads, as whether every one that reads
          # it is #[cfg(test)].
          modules = lib.mapAttrs (_: ds: lib.all (d: d.test) ds)
            (lib.groupBy (x: x.path) (lib.concatMap (d: map (path: { inherit path; inherit (d) test; }) d.reads) declarations));
          # What a declaration that isn't #[cfg(test)] reads: never left out,
          # nor any directory holding it.
          needed = lib.concatMap (d: lib.optionals (!d.test) d.reads) declarations;
        in
        lib.nameValuePair dir { inherit dir targets modules needed; prunable = !build && !includes; };
    in
    lib.listToAttrs (map info (builtins.filter (d: (readToml (pathOf "${d}/Cargo.toml")) ? package) cargoDirs));
  hasBinary = dir:
    (memberToml dir).bin or [ ] != [ ]
    || builtins.pathExists (rustDir + "/${dir}/src/main.rs")
    || builtins.pathExists (rustDir + "/${dir}/src/bin");
  memberPackage = pkgs: dir:
    let
      craneLib = craneFor pkgs;
      toml = memberToml dir;
      dirs = closureOf dir;
      version = toml.package.version or "0.0.0";
      cargoToml = craneLib.writeTOML "Cargo.toml" (manifestFor dirs);
    in
    craneLib.buildPackage {
      pname = toml.package.name;
      version = if builtins.isAttrs version then rootToml.workspace.package.version else version;
      src = lib.cleanSourceWith {
        src = craneLib.cleanCargoSource rustDir;
        name = "${toml.package.name}-source";
        filter = path: type:
          let rel = lib.removePrefix "${toString rustDir}/" (toString path); in
          lib.any (d: rel == d || lib.hasPrefix "${d}/" rel || (type == "directory" && lib.hasPrefix "${rel}/" d)) dirs
          && !(cargoTests (toString path) type);
      };
      cargoLockParsed = lockFor dirs;
      postPatch = "cp ${cargoToml} Cargo.toml";
      # What it's built from, for the checks to read.
      passthru = { inherit cargoToml; };
      strictDeps = true;
      nativeBuildInputs = rustCfg.nativeBuildInputs pkgs;
      buildInputs = rustCfg.buildInputs pkgs;
      # One build: its dependencies built apart would be a target directory
      # kept for each member, where a member's build is seconds.
      cargoArtifacts = null;
      # Not --locked: the lockfile is the workspace's, cut down, and cargo
      # may cut it further (a dependency's optional one that only another
      # member's features turn on). The versions are still the lockfile's,
      # the only ones vendored, and the checks hold the whole workspace's
      # lockfile to its manifests with --locked.
      cargoExtraArgs = "--offline -p ${toml.package.name}";
      doCheck = false;
      meta = lib.optionalAttrs ((toml.bin or [ ]) == [ ] && builtins.pathExists (rustDir + "/${dir}/src/main.rs")) {
        mainProgram = toml.package.name;
      };
    };
  memberPackages = pkgs:
    let members = lib.listToAttrs (map (dir: lib.nameValuePair (memberToml dir).package.name (memberPackage pkgs dir))
      (builtins.filter hasBinary memberDirs));
    in
    members // {
      default = pkgs.symlinkJoin {
        name = "${rootToml.workspace.metadata.crane.name or "workspace"}-${rootToml.workspace.package.version or "0.0.0"}";
        paths = lib.attrValues members;
      };
    };

  # What the checks build from: the whole directory, not only the Cargo
  # sources, since tests read fixtures. With files outside it to include, the
  # repo's root with only DIR and those, and the build in DIR (crane's way
  # for a workspace below its source's root).
  rustSrc =
    if rustCfg.include == [ ] then { src = lib.cleanSource rustDir; }
    else {
      src = lib.cleanSourceWith {
        src = lib.cleanSource self;
        filter = path: type:
          let rel = lib.removePrefix "${toString self}/" (toString path); in
          lib.any (p: rel == p || lib.hasPrefix "${p}/" rel || (type == "directory" && lib.hasPrefix "${rel}/" p))
            ([ rustCfg.dir ] ++ rustCfg.include);
      };
      # Each file alone: a flake's self is a store path, and a path into it
      # would make every check depend on the whole repo.
      cargoLock = builtins.path { path = rustDir + "/Cargo.lock"; name = "Cargo.lock"; };
      cargoToml = builtins.path { path = rustDir + "/Cargo.toml"; name = "Cargo.toml"; };
      postUnpack = ''
        cd "$sourceRoot"/${lib.escapeShellArg rustCfg.dir}
        sourceRoot=.
      '';
    };

  rustChecks = pkgs:
    let
      craneLib = craneFor pkgs;
      # Every member of the workspace, not only a root package or its
      # default-members, built as `cargo test` builds them (crane's default is
      # release, which drops debug assertions and overflow checks).
      common = rustSrc // {
        strictDeps = true;
        cargoExtraArgs = "--locked --workspace";
        CARGO_PROFILE = "dev";
        nativeBuildInputs = rustCfg.nativeBuildInputs pkgs;
        buildInputs = rustCfg.buildInputs pkgs;
      };
      cargoArtifacts = craneLib.buildDepsOnly common;
    in
    {
      # They read the tests, and clippy compiles them, but neither runs one.
      rust-fmt = craneLib.cargoFmt (rustSrc // { cargoExtraArgs = "--all"; passthru.testFiles = [ ]; });
      rust-clippy = craneLib.cargoClippy (common // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--all-targets -- -D warnings";
        passthru.testFiles = [ ];
      });
      # The packages leave out the tests cargoTests finds by reading the
      # source, and what reading can't see, the build can: the workspace's
      # libraries and programs, built as a package builds them but from the
      # whole directory, read none of the files the packages leave out.
      # rustc's dep-info names every file a crate read: its modules, any
      # file include!d however its path was made, through whichever crate's
      # macro, and what a proc macro tracks. Built for release, as the
      # packages are, so code only release compiles is read too.
      rust-package-sources = craneLib.mkCargoDerivation (common // rec {
        CARGO_PROFILE = "release";
        cargoArtifacts = craneLib.buildDepsOnly (common // { inherit CARGO_PROFILE; });
        pnameSuffix = "-package-sources";
        buildPhaseCargoCommand = "cargoWithProfile build --locked --workspace";
        doInstallCargoArtifacts = false;
        nativeBuildInputs = rustCfg.nativeBuildInputs pkgs ++ [ pkgs.diffutils ];
        installPhaseCommand = ''
          export LC_ALL=C
          (cd ${craneLib.cleanCargoSource rustDir} && find . -type f | sort) >"$TMPDIR/all"
          (cd ${packageSource pkgs} && find . -type f | sort) >"$TMPDIR/kept"
          comm -23 "$TMPDIR/all" "$TMPDIR/kept" | sed 's|^[.]/||' >"$TMPDIR/left"
          # Each file a dep-info names on a line of its own (a space in its
          # name escaped, as make reads it), from the workspace's root as
          # rustc and cargo ran there: as named, and with its links
          # followed to the file itself. Either way it's a file the
          # packages must have.
          find target -name '*.d' -exec grep -h '^[^ ].*:$' {} + | sed 's/:$//; s/\\\(.\)/\1/g' | sort -u >"$TMPDIR/named"
          { xargs -r -d '\n' realpath -m -s --relative-to="$PWD" -- <"$TMPDIR/named"
            xargs -r -d '\n' realpath -m --relative-to="$PWD" -- <"$TMPDIR/named"; } | sort -u >"$TMPDIR/read"
          if read=$(comm -12 "$TMPDIR/left" "$TMPDIR/read") && [ -n "$read" ]; then
            echo "The packages leave these out as tests, but building them reads them:"
            echo "$read"
            echo "Give each a name that isn't tests (lib/project.nix's cargoTests says what it leaves out)."
            exit 1
          fi
          mkdir -p $out
        '';
        passthru.testFiles = [ ];
      });
    } // lib.optionalAttrs rustCfg.test {
      rust-test = craneLib.cargoTest (common // {
        inherit cargoArtifacts;
        nativeBuildInputs = rustCfg.nativeBuildInputs pkgs ++ rustCfg.testInputs pkgs;
      });
    };

  # The house rules' "never commit secrets": gitleaks over every tracked
  # file (a flake's source is the repo's tracked tree), with its own rules for
  # credentials that have a known shape (GitHub, cloud and API tokens,
  # private keys), but not its generic "API key" guess, which takes every
  # invented test password for one. Its config also skips files by path
  # (lockfiles, SVGs, vendored and minified code), and a token in any tracked
  # text file is a leak, so here no allowlist names a path: one that does
  # nothing else goes, and one that ANDs a path with the rest goes whole
  # rather than widening to every file. A false alarm is answered in the
  # repo: `gitleaks:allow` in a comment on the line, or the finding's
  # fingerprint in .gitleaksignore. Files over 5 MB (recordings, databases)
  # and binaries aren't read.
  gitleaksConfig = pkgs: pkgs.runCommand "gitleaks.toml" { nativeBuildInputs = [ pkgs.remarshal pkgs.jq ]; } ''
    toml2json ${pkgs.gitleaks.src}/config/gitleaks.toml | jq '
      def unpathed: [ .[]
        | if has("paths") and ((.condition // "or") | ascii_downcase) == "and" then empty else del(.paths) end
        | select(has("regexes") or has("stopwords") or has("commits")) ];
      def allowlists: .allowlists = ((.allowlists // []) + [.allowlist // empty] | unpathed) | del(.allowlist);
      allowlists
      | .rules |= map(select(.id != "generic-api-key") | allowlists | if .allowlists == [] then del(.allowlists) end)
    ' > config.json
    # Fail closed on a config whose shape this no longer matches.
    jq -e '([.. | objects | select(has("paths") or has("allowlist"))] == []) and (.rules | length > 100)' config.json >/dev/null
    json2toml config.json $out
  '';
  # A finding is reported by file, line, rule and fingerprint, never by what
  # the file says: a public repo's CI log is public too. gitleaks' own output
  # stays out of the log, since it can quote a file: its verbose findings
  # show the line around a secret with only that secret redacted, and its
  # warning for a .gitleaksignore line that isn't a fingerprint quotes the
  # line, a token pasted there included. Leaks exit 2, so any other failure
  # (a scan that stopped, a bad config) still fails, with a pointer to
  # gitleaks' log in a build kept with --keep-failed.
  secretsCheck = pkgs: pkgs.runCommand "secrets" {
    nativeBuildInputs = [ pkgs.gitleaks pkgs.jq ];
    # It reads every file and runs none.
    passthru.testFiles = [ ];
  } ''
    cd ${self}
    status=0
    gitleaks dir --no-banner --redact --config ${gitleaksConfig pkgs} --max-target-megabytes 5 \
      --exit-code 2 --report-format json --report-path "$TMPDIR/leaks.json" . >"$TMPDIR/gitleaks.log" 2>&1 || status=$?
    if grep -q 'Invalid .gitleaksignore entry' "$TMPDIR/gitleaks.log"; then
      echo ".gitleaksignore has lines that aren't a fingerprint (FILE:RULE:LINE), and they ignore nothing"
    fi
    if [ -e "$TMPDIR/leaks.json" ]; then
      jq -r '.[] | "\(.File):\(.StartLine): \(.RuleID) (fingerprint \(.Fingerprint))"' "$TMPDIR/leaks.json"
    fi
    case $status in
      0) jq -e 'length == 0' "$TMPDIR/leaks.json" >/dev/null ;;
      2) exit 1 ;;
      *)
        echo "gitleaks stopped (exit $status) before it finished. Its log isn't shown here; nix build this check with --keep-failed and read gitleaks.log in the build directory it keeps."
        exit 1
        ;;
    esac
    touch $out
  '';

  pyChecks = pkgs:
    let dirs = lib.escapeShellArgs pyCfg.dirs; in
    {
      python-ruff = pkgs.runCommand "python-ruff" { nativeBuildInputs = [ pkgs.ruff ]; passthru.testFiles = [ ]; } ''
        cd ${self}
        ruff check --no-cache --select E9,F63,F7,F82 ${dirs}
        ${lib.optionalString pyCfg.format "ruff format --no-cache --check ${dirs}"}
        touch $out
      '';
    };
in
{
  devShells = forAll (pkgs: {
    default = pkgs.mkShell {
      packages =
        lib.optionals (rust != null) (
          [ (toolchainFor pkgs [ "rust-src" "rust-analyzer" ]) ]
          ++ rustCfg.nativeBuildInputs pkgs
        )
        ++ lib.optionals (python != null) [ pkgs.python3 pkgs.ruff pkgs.uv ]
        ++ lib.optional (node != null) pkgs.nodejs
        ++ packages pkgs;
      buildInputs = lib.optionals (rust != null) (rustCfg.buildInputs pkgs);
    };
  });

  checks = forAll (pkgs:
    { secrets = secretsCheck pkgs; }
    // lib.optionalAttrs (rust != null) (rustChecks pkgs)
    // lib.optionalAttrs (python != null) (pyChecks pkgs)
    // checks pkgs);

  formatter = forAll (pkgs: pkgs.nixfmt-rfc-style);
} // lib.optionalAttrs (rust != null) {
  packages = forAll (pkgs: if rustCfg.members then memberPackages pkgs else { default = rustPackage pkgs; });
}
