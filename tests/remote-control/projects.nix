# Project folders for the Remote Control tests: git checkouts with an origin,
# which is what claude-rc-scan serves (modules/remote-control.nix), made before
# dev's user manager first scans. `after` runs next, as dev, for what a test
# wants beside them.
{ pkgs, names, after ? "" }:
{
  systemd.services.projects = {
    wantedBy = [ "user@1001.service" ];
    before = [ "user@1001.service" ];
    path = [ pkgs.git ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      User = "dev";
    };
    script = ''
      for p in ${pkgs.lib.escapeShellArgs names}; do
        git init -q "/data/dev/src/$p"
        git -C "/data/dev/src/$p" remote add origin "https://example.invalid/$p.git"
      done
    '' + after;
  };
}
