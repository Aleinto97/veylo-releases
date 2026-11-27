#!/usr/bin/env python3
"""Serves the runtime test clips with HTTP Range support, like real stream hosts do
(python -m http.server has none, so mpv can't seek MP4s through it).
   tool/serve-clips.py <dir> [port]"""
import http.server, os, re, sys

ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8765


class RangeHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=ROOT, **kw)

    def log_message(self, *a):
        pass

    def send_head(self):
        path = self.translate_path(self.path)
        if not os.path.isfile(path):
            return super().send_head()
        size = os.path.getsize(path)
        m = re.match(r"bytes=(\d*)-(\d*)$", self.headers.get("Range", ""))
        f = open(path, "rb")
        if not m or (m.group(1) == "" and m.group(2) == ""):
            self.send_response(200)
            start, end = 0, size - 1
        else:
            if m.group(1) == "":
                start, end = max(0, size - int(m.group(2))), size - 1
            else:
                start = int(m.group(1))
                end = min(int(m.group(2)), size - 1) if m.group(2) else size - 1
            if start >= size:
                f.close()
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.end_headers()
                return None
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        f.seek(start)
        self._remaining = end - start + 1
        return f

    def copyfile(self, source, outputfile):
        left = getattr(self, "_remaining", None)
        while left is None or left > 0:
            chunk = source.read(min(1 << 16, left) if left is not None else 1 << 16)
            if not chunk:
                break
            try:
                outputfile.write(chunk)
            except (BrokenPipeError, ConnectionResetError):
                break
            if left is not None:
                left -= len(chunk)


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), RangeHandler).serve_forever()
