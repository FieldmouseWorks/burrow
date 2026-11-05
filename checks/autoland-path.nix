# The steward's tools reach a pass however it's started: by hand or
# a Claude job (`autoland-steward kick OWNER/REPO`), with no gh or
# proj of its own, gets them from the wrapper, ahead of its own PATH. The reviewers' service gets none of them: only what its unit
# gives it. Its gh is the machine's: in an App's env (a job's),
# `status` and a project's clone go through the socket, and a pass's
# own calls with the steward's token as given.
{ self, pkgs, ... }:
let
  example = self.nixosConfigurations.example.config;
  steward = builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward")
    example.environment.systemPackages);
  # gh-token's side of the socket: the App's token for whatever's asked.
  socket = pkgs.writeText "fake-socket.py" ''
    import os, socket, sys
    s = socket.socket(socket.AF_UNIX)
    s.bind(sys.argv[1])
    s.listen()
    open(sys.argv[1] + ".up", "w").close()
    while True:
        c, _ = s.accept()
        c.recv(4096)
        c.sendall(b"token from-the-socket\n")
        c.close()
  '';
in pkgs.runCommand "autoland-path-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  w=${steward}/bin/autoland-steward
  # The wrapper, but showing the PATH it would run the steward with.
  grep -q '^exec .*steward.py' "$w" || { echo "autoland-steward doesn't exec the steward"; exit 1; }
  sed 's|^exec .*steward.py.*|printf "%s\\n" "$PATH"; exit|' "$w" >path-of
  for sub in kick status pass; do
    p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash path-of $sub)
    case $p in *:/caller) ;; *) echo "$sub: the caller's PATH isn't kept after the steward's: $p"; exit 1 ;; esac
    [ "$p" != /caller ] || { echo "autoland-steward sets no PATH of its own"; exit 1; }
    for t in gh git tar proj; do
      env -i PATH="$p" ${pkgs.bash}/bin/bash -c "command -v $t" >/dev/null || { echo "$sub: no $t on the steward's PATH"; exit 1; }
    done
  done
  r=${builtins.head (pkgs.lib.splitString " " example.systemd.services.autoland-review.serviceConfig.ExecStart)}
  grep -q '^exec .*/bin/burrow-review "$@"' "$r" || { echo "the reviewers' service isn't burrow-review"; exit 1; }
  sed 's|^exec .*|printf "%s\\n" "$PATH"; exit|' "$r" >review-path-of
  p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash review-path-of work)
  [ "$p" = /caller ] || { echo "review gets more than its units give it: $p"; exit 1; }
  p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash path-of status)
  gh=$(env -i PATH="$p" ${pkgs.bash}/bin/bash -c 'command -v gh')
  git=$(env -i PATH="$p" ${pkgs.bash}/bin/bash -c 'command -v git')
  [ "$gh" = ${example.burrow.tools.gh}/bin/gh ] || { echo "the steward's gh isn't the machine's: $gh"; exit 1; }
  [ "$git" = ${example.programs.git.package}/bin/git ] || { echo "the steward's git isn't the machine's: $git"; exit 1; }
  # As a job with an App runs it: no token of its own, the socket's.
  python3 -I ${socket} $PWD/app.sock & while [ ! -e app.sock.up ]; do sleep 0.1; done
  mkdir no-login
  ask() { env -i PATH="$p" HOME=$PWD GH_CONFIG_DIR=$PWD/no-login GH_TOKEN_SOCKET=$PWD/app.sock GH_TOKEN_OWNER=sam "$@" gh auth token 2>&1; }
  got=$(ask GH_TOKEN=) || true
  [ "$got" = from-the-socket ] || { echo "status in an App's env doesn't go through its socket: $got"; exit 1; }
  got=$(ask GH_TOKEN=the-stewards) || true
  [ "$got" = the-stewards ] || { echo "a pass's own call doesn't use the steward's token: $got"; exit 1; }
  touch $out
''
