# gh as burrow builds it, for every module and user: burrow.tools.gh, unless
# github.nix's wrapper, which runs this one, takes its place.
#
# `gh auth login`, `refresh` and `setup-git` write gh's own path into the
# user's git config as its credential helper, after a blank entry that cuts
# off the system's, and a path in the store breaks every git fetch of the
# user's once the store is collected (burrow#1464). Patched to name the system
# profile's gh instead. The patch stops applying, and the build says so, if gh
# moves the helper under it. Not an overlay: a NixOS test's pkgs take none.
pkgs:
pkgs.gh.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../patches/gh-setup-git-lasting-path.patch ];
})
