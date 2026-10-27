# The signed-in browser, for real: Claude's MCP server drives the one browser
# the owner sees, a login (a cookie) outlives a restart of the browser, only
# burrow.user on the machine reaches its DevTools port and its viewer (under
# iptables and under nftables), the viewer is open on the interface it's given
# and speaks VNC there, the screen listens on no TCP or abstract socket, the
# profile is the browser's alone, and Claude gets the MCP server as soon as it
# has a config, and loses it when the browser is turned off.
#
#   nix build .#checks.x86_64-linux.browser -L
{ self, pkgs }:
let
  # A site with a login: the page sets a cookie that lasts a year.
  site = pkgs.writeTextDir "index.html" ''
    <html><head><title>burrow test page</title></head>
    <body><script>document.cookie = "login=yes; max-age=31536000; path=/"</script></body></html>
  '';

  # An MCP client, as Claude is one: starts burrow-browser-mcp, makes the
  # calls it's given (a JSON list of [tool, arguments]) in one session and
  # prints their text.
  mcp = pkgs.writeText "mcp-call.py" ''
    import json, subprocess, sys

    p = subprocess.Popen(["burrow-browser-mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    n = 0

    def call(method, params, notify=False):
        global n
        msg = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            n += 1
            msg["id"] = n
        p.stdin.write(json.dumps(msg) + "\n")
        p.stdin.flush()
        while not notify:
            r = json.loads(p.stdout.readline())
            if r.get("id") == n:
                return r

    call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}})
    call("notifications/initialized", {}, notify=True)
    for tool, args in json.loads(sys.argv[1]):
        r = call("tools/call", {"name": tool, "arguments": args})
        if "error" in r or r["result"].get("isError"):
            sys.exit(json.dumps(r))
        print("\n".join(c.get("text", "") for c in r["result"]["content"]))
    p.stdin.close()
    p.wait(timeout=30)
  '';

  # The first thing noVNC gets through the viewer's WebSocket: the screen's
  # RFB greeting.
  rfb = pkgs.writeText "rfb-greeting.py" ''
    import base64, os, socket, sys

    s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=10)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall((f"GET /websockify HTTP/1.1\r\nHost: {sys.argv[1]}\r\nUpgrade: websocket\r\n"
               f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
               "Sec-WebSocket-Protocol: binary\r\n\r\n").encode())
    data = b""
    while b"\r\n\r\n" not in data:
        data += s.recv(4096)
    head, rest = data.split(b"\r\n\r\n", 1)
    assert head.startswith(b"HTTP/1.1 101"), head
    while len(rest) < 2 + 12:
        rest += s.recv(4096)
    print(rest[2:14].decode().strip())
  '';

  browserMachine = extra: { lib, ... }: {
    imports = [ self.nixosModules.browser extra ];
    burrow.browser = {
      enable = true;
      viewer.interfaces = [ "eth1" ];
    };
    virtualisation.memorySize = 2048;

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
      # As on a real machine: its runtime dir is where the MCP server writes.
      linger = true;
    };
    users.groups.dev.gid = 1001;
    # Anyone else on the machine: a sandbox's proxy, a reviewer, CI.
    users.users.other = {
      isNormalUser = true;
      group = "users";
    };

    environment.systemPackages = [ pkgs.python3 pkgs.curl pkgs.jq pkgs.iproute2 ];

    systemd.services.site = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server -b 127.0.0.1 -d ${site} 8088";
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "browser";

  nodes = {
    machine = browserMachine { };
    # The same, with the firewall on nftables.
    nft = browserMachine { networking.nftables.enable = true; };
    client = {
      environment.systemPackages = [ pkgs.python3 pkgs.curl ];
    };
  };

  testScript = ''
    import json
    import shlex

    def drive(node, *calls):
        cmd = f"XDG_RUNTIME_DIR=/run/user/1001 python3 ${mcp} {shlex.quote(json.dumps(calls))}"
        return node.succeed(f"su - dev -c {shlex.quote(cmd)}")

    def login_kept(node):
        # Another page of the site, which sets nothing: the cookie is the old one.
        out = drive(node, ["browser_navigate", {"url": "http://127.0.0.1:8088/other"}],
                    ["browser_evaluate", {"function": "() => document.cookie"}])
        assert "login=yes" in out, out

    cdp = "curl -fsS --max-time 5 http://127.0.0.1:9222/json/version"
    viewer = "curl -fsS --max-time 5 -o /dev/null http://127.0.0.1:6080/vnc.html"

    start_all()
    for node in (machine, nft):
        node.wait_for_unit("burrow-browser.service")
        node.wait_for_unit("burrow-browser-viewer.service")
        node.wait_for_unit("user@1001.service")
        node.wait_until_succeeds("su dev -c '" + cdp + "'", timeout=120)

    with subtest("only burrow.user reaches the DevTools port and the viewer"):
        for node in (machine, nft):
            node.succeed("su dev -c '" + viewer + "'")
            for who in ("other", "root"):
                for check in (cdp, viewer):
                    node.fail(f"su {who} -s /bin/sh -c '{check}'")
            # The machine's own outside address goes through lo too.
            ip = node.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()
            node.fail(f"su other -s /bin/sh -c 'curl -fsS --max-time 5 -o /dev/null http://{ip}:6080/vnc.html'")

    with subtest("the viewer is open on its interface, and the screen speaks VNC through it"):
        client.wait_until_succeeds("curl -fsS --max-time 5 -o /dev/null http://machine:6080/vnc.html", timeout=60)
        greeting = client.succeed("python3 ${rfb} machine 6080").strip()
        assert greeting.startswith("RFB 003."), greeting
        # Not the DevTools port, from anywhere else.
        client.fail("curl -fsS --max-time 5 http://machine:9222/json/version")

    with subtest("the screen listens on no TCP port and no abstract socket"):
        out = machine.succeed("ss -Hlntp; ss -Hlxp")
        assert "Xvnc" not in machine.succeed("ss -Hlntp"), out
        assert "@/tmp/.X11-unix" not in machine.succeed("ss -Hlx"), out

    with subtest("the profile is the browser's alone"):
        assert machine.succeed("stat -c '%a %U' /var/lib/burrow-browser").strip() == "700 burrow-browser"
        machine.fail("su dev -c 'ls /var/lib/burrow-browser'")

    with subtest("Claude gets the MCP server once it has a config, and only then"):
        machine.succeed("systemctl show -p ActiveState --value burrow-browser-mcp.service | grep -qx inactive")
        machine.fail("test -e /home/dev/.claude.json")
        machine.succeed("su dev -c 'echo {} >/home/dev/.claude.json'")
        machine.wait_for_unit("burrow-browser-mcp.service")
        entry = machine.succeed("jq -c .mcpServers.browser /home/dev/.claude.json").strip()
        assert entry == '{"type":"stdio","command":"/run/current-system/sw/bin/burrow-browser-mcp","args":[]}', entry
        # Again, as every boot does: nothing changes.
        before = machine.succeed("stat -c %.9Y /home/dev/.claude.json")
        add = machine.succeed("sed -n 's/^ExecStart=//p' /etc/systemd/system/burrow-browser-mcp.service").strip()
        machine.succeed(f"su dev -c {shlex.quote(add)}")
        assert machine.succeed("stat -c %.9Y /home/dev/.claude.json") == before

    with subtest("Claude drives the browser the owner sees"):
        out = drive(machine, ["browser_navigate", {"url": "http://127.0.0.1:8088/"}])
        assert "burrow test page" in out, out
        tabs = machine.succeed("su dev -c 'curl -fsS http://127.0.0.1:9222/json/list'")
        assert "http://127.0.0.1:8088/" in tabs, tabs
        # Snapshots go to the runtime dir, not the folder Claude works in.
        machine.succeed("ls /run/user/1001/burrow-browser/ | grep -q .")

    with subtest("a login outlives the browser"):
        machine.succeed("systemctl restart burrow-browser.service")
        machine.wait_until_succeeds("su dev -c '" + cdp + "'", timeout=120)
        login_kept(machine)

    with subtest("a reboot keeps both"):
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("burrow-browser.service")
        machine.wait_for_unit("burrow-browser-mcp.service")
        assert machine.succeed("jq -c .mcpServers.browser /home/dev/.claude.json").strip() == entry
        machine.wait_until_succeeds("su dev -c '" + cdp + "'", timeout=120)
        machine.wait_for_unit("user@1001.service")
        login_kept(machine)

    with subtest("turned off, Claude loses the MCP server"):
        machine.succeed("systemctl stop burrow-browser-mcp.service")
        assert machine.succeed("jq -c .mcpServers.browser /home/dev/.claude.json").strip() == "null"
  '';
}
