# Which repo the flake's origin is, and so whether HOST-update goes
# through its train: the same in every spelling of GitHub's URLs
# (scheme, host case, a trailing dot, the default port, either SSH
# form, .git, a trailing slash), whatever the case of autoland's name
# for it; nothing for a repo elsewhere, even one with github.com in its
# path; and a GitHub origin it can't read refused, never read as no
# train.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  # How the example takes an origin: through its train or not, and
  # refused or not by the updates module's own assertion. Only its
  # assertions are read: all 1,485 of the example's took 5 s an
  # origin, 139 s of one leg's gate.
  take = origin: let
    machine = self.nixosConfigurations.example.extendModules {
      modules = [{ burrow.updates.origin = lib.mkForce origin; }];
    };
    updates = lib.filter (d: lib.hasSuffix "/modules/updates.nix" (toString d.file)) machine.options.assertions.definitionsWithLocations;
  in {
    train = machine.config.burrow.updates.throughTrain;
    refused = lib.any (a: !a.assertion && lib.hasInfix "OWNER/NAME" a.message) (lib.concatMap (d: d.value) updates);
  };
  forms = [
    "https://github.com/sam/homelab" "https://github.com/sam/homelab.git" "https://github.com/sam/homelab/"
    "https://github.com/Sam/HomeLab" "https://GitHub.com/sam/homelab.git" "https://github.com./sam/homelab"
    "HTTPS://github.com:443/sam/homelab" "git@github.com:sam/homelab.git" "git@github.com:sam/homelab"
    "git@GITHUB.COM.:sam/homelab/" "ssh://git@github.com/sam/homelab.git" "ssh://git@github.com:22/sam/homelab/"
    "ssh://git@GitHub.com.:22/sam/homelab"
  ];
  wrong = lib.filter (o: let t = take o; in !t.train || t.refused) forms
    ++ lib.filter (o: let t = take o; in t.train || t.refused) [
      "https://git.example.org/sam/homelab" "https://github.com/sam/other" "/srv/mirrors/github.com/sam/homelab"
      "https://git.example.org/github.com/sam/homelab"
    ]
    ++ lib.filter (o: !(take o).refused) [
      "https://github.com/sam" "https://www.github.com/sam/homelab" "git@github.com:sam/homelab/extra"
      "https://github.com:8443/sam/homelab" "http://github.com/sam/homelab" "https://sam@github.com/sam/homelab"
      "ssh://sam@github.com/sam/homelab" "git://GitHub.com/sam/homelab"
    ];
in pkgs.runCommand "update-origin-check" { } ''
  ${lib.concatMapStrings (o: "echo 'wrong for ${o}'
") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
