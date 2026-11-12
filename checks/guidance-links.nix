# Offline links, including fragments, from every path in burrow's docs lane.
# Check the whole set so a deletion/rename also checks incoming links.
{ self, pkgs, ... }:
pkgs.runCommand "guidance-links-check" {
  nativeBuildInputs = [ pkgs.lychee ];
  # Lychee initializes its client even offline; the sandbox has no system CA.
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
} ''
  cd ${self}
  lychee --offline --include-fragments README.md AGENTS.md CLAUDE.md 'docs/**/*.md' 'house/*.md'
  touch $out
''
