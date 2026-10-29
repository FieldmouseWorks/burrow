# The tools projects and the agents working on them expect. The same set goes
# into the agent sandboxes (agents/guest.nix), so a job behaves the same in a
# VM as on the host.
inputs:
{ config, lib, options, pkgs, ... }:
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.toolbox.enable = lib.mkEnableOption "the shared toolbox (on the host; sandboxes always get it)";

  config = lib.mkIf config.burrow.toolbox.enable {
    environment.systemPackages = with pkgs; [
      # VCS. gh and git-lfs are github.nix's, which know an App's env, where
      # there is one.
      (config.burrow.tools.gh or gh)
      (config.burrow.tools.git-lfs or git-lfs)
      jujutsu
      mergiraf
      # search and files
      ripgrep
      fd
      tree
      file
      unzip
      zip
      zstd
      # languages and builds
      gcc
      gnumake
      pkg-config
      python3
      uv
      nodejs_22
      pnpm
      rustup
      go
      sqlite
      # VMs and backups
      qemu_kvm
      rclone
      # secrets tooling for sops-nix
      age
      sops
      ssh-to-age
    ];

    # git over https uses gh's login, so `gh auth login` is the only GitHub
    # login a fresh box needs.
    programs.git = {
      enable = true;
      config.credential."https://github.com".helper = "!${pkgs.gh}/bin/gh auth git-credential";
    };

    programs.direnv = {
      enable = true;
      nix-direnv.enable = true;
    };

    # Prebuilt binaries (GitHub Actions runners, npm postinstalls, Playwright)
    # expect a normal Linux loader. The runners are .NET, which aborts at start
    # without ICU.
    programs.nix-ld.enable = true;
    programs.nix-ld.libraries = options.programs.nix-ld.libraries.default ++ [ pkgs.icu ];

    # ...and #!/bin/bash: the runners' own scripts (config.sh, run.sh, and the
    # run-helper.sh they rewrite on every self-update) start with it.
    systemd.tmpfiles.settings.bin-bash."/bin/bash"."L+".argument = "${pkgs.bashInteractive}/bin/bash";
  };
}
