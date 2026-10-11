#!/usr/bin/env python3
"""Eval helper: forward OpenAI-style requests to apfel, rewriting what it doesn't accept.

Butterfish hard-codes OpenAI model names (gpt-5.5) and sends fields apfel rejects. This proxy
replaces "model" with apfel's model and drops those fields, so Butterfish can be compared with
bro on the same on-device model.

    python3 evals/model-proxy.py [listen_port] [upstream]   # default 11435 → http://127.0.0.1:11434
    evals/run.sh --tool butterfish --endpoint http://127.0.0.1:11435/v1
"""
import json
import sys
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 11435
UPSTREAM = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:11434"
MODEL = "apple-foundationmodel"
DROP = ["reasoning", "service_tier", "store", "previous_response_id", "parallel_tool_calls", "include", "prompt_cache_key", "safety_identifier", "text", "truncation"]


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        try:
            data = json.loads(body)
            data["model"] = MODEL
            for key in DROP:
                data.pop(key, None)
            if "max_output_tokens" in data:
                data["max_output_tokens"] = min(data["max_output_tokens"], 512)
            else:
                data["max_output_tokens"] = 512
            body = json.dumps(data).encode()
        except ValueError:
            pass
        req = urllib.request.Request(UPSTREAM + self.path, data=body, method="POST",
                                     headers={"content-type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                self.relay(resp.status, resp.headers.get("content-type", "application/json"), resp)
        except urllib.error.HTTPError as e:
            self.relay(e.code, e.headers.get("content-type", "application/json"), e)

    def relay(self, status, content_type, stream):
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.end_headers()
        while True:
            chunk = stream.read(4096)
            if not chunk:
                break
            self.wfile.write(chunk)
            self.wfile.flush()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
