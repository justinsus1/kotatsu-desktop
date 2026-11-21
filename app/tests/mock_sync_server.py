"""Minimal stand-in for kotatsu-syncserver, same upsert semantics, for testing."""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DB = {"cats": {}, "favs": {}, "hist": {}}
TOKEN = "test-token"


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        sys.stderr.write("%s %s\n" % (self.command, self.path))

    def send(self, code, obj=None):
        body = b"" if obj is None else json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authed(self):
        return self.headers.get("Authorization") == "Bearer " + TOKEN

    def body(self):
        n = int(self.headers.get("Content-Length", 0))
        return json.loads(self.rfile.read(n) or b"{}")

    def favpkg(self):
        return {"categories": list(DB["cats"].values()), "favourites": list(DB["favs"].values()), "timestamp": 0}

    def histpkg(self):
        return {"history": list(DB["hist"].values()), "timestamp": 0}

    def do_GET(self):
        if self.path == "/dump":
            return self.send(200, DB if False else {"cats": list(DB["cats"].values()), "favs": list(DB["favs"].values()), "hist": list(DB["hist"].values())})
        if not self.authed():
            return self.send(401)
        if self.path == "/resource/favourites":
            return self.send(200, self.favpkg())
        if self.path == "/resource/history":
            return self.send(200, self.histpkg())
        self.send(404)

    def do_POST(self):
        b = self.body()
        if self.path == "/auth":
            if b.get("password") == "pw":
                return self.send(200, {"token": TOKEN})
            return self.send(400, "Wrong password")
        if self.path == "/seed":
            DB["cats"] = {c["category_id"]: c for c in b.get("cats", [])}
            DB["favs"] = {(f["manga_id"], f["category_id"]): f for f in b.get("favs", [])}
            DB["hist"] = {h["manga_id"]: h for h in b.get("hist", [])}
            return self.send(200, {})
        if not self.authed():
            return self.send(401)
        if self.path == "/resource/favourites":
            for c in b.get("categories") or []:
                DB["cats"][c["category_id"]] = c
            for f in b.get("favourites") or []:
                DB["favs"][(f["manga_id"], f["category_id"])] = f
            return self.send(200, self.favpkg())
        if self.path == "/resource/history":
            for h in b.get("history") or []:
                old = DB["hist"].get(h["manga_id"])
                if old and old["updated_at"] > h["updated_at"]:
                    continue
                DB["hist"][h["manga_id"]] = h
            return self.send(200, self.histpkg())
        self.send(404)


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
