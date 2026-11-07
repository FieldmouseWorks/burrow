# A Responses API stand-in for the pinned Codex CLI. Each turn replays the
# tool calls in the JSON file named by argv[2] ([[NAME, ARGS], ...]), one per
# request, then answers. A "SID" session_id stands for the session the last
# call left running. Why not Rust: a fixture the VM test runs, never shipped.
import http.server
import json
import re
import sys

PORT = int(sys.argv[1])
CALLS = sys.argv[2]


class Model(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        outputs = [str(i.get("output", "")) for i in request.get("input", []) if i.get("type", "").endswith("_output")]
        with open(CALLS) as f:
            calls = json.load(f)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()

        def event(data):
            self.wfile.write(f"event: {data['type']}\ndata: {json.dumps(data)}\n\n".encode())

        event({"type": "response.created", "response": {"id": "r"}})
        if len(outputs) < len(calls):
            name, args = calls[len(outputs)]
            session = re.search(r"session ID (\d+)", " ".join(outputs))
            if args.get("session_id") == "SID" and session:
                args = dict(args, session_id=int(session.group(1)))
            item = {"type": "function_call", "name": name, "arguments": json.dumps(args), "call_id": f"c{len(outputs)}"}
        else:
            item = {"type": "message", "role": "assistant", "id": "m", "content": [{"type": "output_text", "text": "done"}]}
        event({"type": "response.output_item.done", "item": item})
        usage = {"input_tokens": 1, "input_tokens_details": None, "output_tokens": 1, "output_tokens_details": None, "total_tokens": 2}
        event({"type": "response.completed", "response": {"id": "r", "usage": usage}})

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Model).serve_forever()
