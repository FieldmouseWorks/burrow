# A blobless clone over smart HTTP with a token-only URL
# (http://TOKEN@host/repo.git), as Renovate clones, from a server that wants
# the token. curl 8.23 strips the credentials from CURLINFO_EFFECTIVE_URL; a
# git that takes that for a redirect forgets the token, and the clone fails
# (patches/git-curl-effective-url.patch).
#
#   python3 git-token-url.py GIT WORKDIR
import base64, http.server, os, subprocess, sys, threading

git, work = sys.argv[1], sys.argv[2]
TOKEN = "gho_burrowtesttoken"


def run(*args, **kw):
    return subprocess.run([git, *args], check=True, **kw)


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.backend()

    def do_POST(self):
        self.backend()

    def backend(self):
        auth = self.headers.get("Authorization", "")
        want = "Basic " + base64.b64encode(f"{TOKEN}:".encode()).decode()
        if auth != want:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="test"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        path, _, query = self.path.partition("?")
        body = b""
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            while True:
                size = int(self.rfile.readline().strip(), 16)
                if size == 0:
                    self.rfile.readline()
                    break
                body += self.rfile.read(size)
                self.rfile.readline()
        else:
            body = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        env = {
            "GIT_PROJECT_ROOT": os.path.join(work, "srv"),
            "GIT_HTTP_EXPORT_ALL": "1",
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "HTTP_CONTENT_ENCODING": self.headers.get("Content-Encoding", ""),
            "GIT_PROTOCOL": self.headers.get("Git-Protocol", ""),
            "REMOTE_USER": TOKEN,
        }
        out = subprocess.run([git, "http-backend"], input=body, env=env,
                             stdout=subprocess.PIPE, check=True).stdout
        head, _, payload = out.partition(b"\r\n\r\n")
        status = 200
        headers = []
        for line in head.decode().split("\r\n"):
            k, _, v = line.partition(":")
            if k.lower() == "status":
                status = int(v.split()[0])
            else:
                headers.append((k, v.strip()))
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


src = os.path.join(work, "src")
run("init", "-q", "-b", "main", src)
for i in range(3):
    with open(os.path.join(src, f"f{i}"), "w") as f:
        f.write(f"blob {i}\n")
run("-C", src, "add", ".")
run("-C", src, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "files")
run("clone", "-q", "--bare", src, os.path.join(work, "srv", "repo.git"))
run("-C", os.path.join(work, "srv", "repo.git"), "config", "uploadpack.allowFilter", "true")

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f"http://{TOKEN}@127.0.0.1:{server.server_port}/repo.git"

env = {"HOME": work, "PATH": os.environ["PATH"], "GIT_TERMINAL_PROMPT": "0"}
clone = subprocess.run([git, "clone", "--filter=blob:none", "-b", "main", url,
                        os.path.join(work, "clone")], env=env)
if clone.returncode != 0:
    sys.exit("the clone with a token-only URL failed")
