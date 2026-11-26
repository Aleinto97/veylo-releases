#!/usr/bin/env python3
"""Offline unit tests for tools/picks-ai/fetch.py retry, mirror failover, and checksum verification."""
import email.message
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import urllib.error

import fetch


class FetchTests(unittest.TestCase):
    def test_pins_json_is_valid_and_has_sha256_digests(self):
        pins = json.loads((Path(fetch.__file__).parent / 'pins.json').read_text(encoding='utf-8'))
        for name, spec in pins['assets'].items():
            self.assertEqual(len(spec['sha256']), 64, name)
            int(spec['sha256'], 16)
        for key in ('ort_desktop', 'ort_android', 'vc_redist_x64'):
            self.assertTrue(pins[key]['urls'], key)
            self.assertEqual(len(pins[key]['sha256']), 64, key)
            int(pins[key]['sha256'], 16)
        self.assertGreaterEqual(len(pins['ort_android']['urls']), 2, 'android runtime needs a mirror')

    def test_mirror_failover_on_429_and_retry_after_cap(self):
        payload = b'valid-onnx-archive-bytes'
        digest = hashlib.sha256(payload).hexdigest()
        seen_urls = []
        sleeps = []

        def fake_urlopen(req, timeout=120):
            url = req.full_url if hasattr(req, 'full_url') else str(req)
            seen_urls.append(url)
            if len(seen_urls) == 1:
                hdrs = email.message.Message()
                hdrs['Retry-After'] = '9999'
                raise urllib.error.HTTPError(url, 429, 'Too Many Requests', hdrs, None)
            return io.BytesIO(payload)

        with tempfile.TemporaryDirectory() as tmp:
            old_dest = fetch.DEST
            fetch.DEST = Path(tmp)
            try:
                with mock.patch.object(fetch.urllib.request, 'urlopen', side_effect=fake_urlopen), \
                     mock.patch.object(fetch.time, 'sleep', side_effect=sleeps.append):
                    path = fetch.download(['https://primary.invalid/a.aar', 'https://mirror.invalid/a.aar'], digest)
                    self.assertEqual(path.read_bytes(), payload)
                    self.assertEqual(seen_urls, ['https://primary.invalid/a.aar', 'https://mirror.invalid/a.aar'])
                    # Switching to a fresh mirror uses a short wait rather than sleeping Retry-After=9999.
                    self.assertEqual(sleeps, [2])
                    # Second call hits local cache with zero network requests.
                    path2 = fetch.download(['https://primary.invalid/a.aar'], digest)
                    self.assertEqual(path2, path)
                    self.assertEqual(len(seen_urls), 2)
            finally:
                fetch.DEST = old_dest

    def test_retry_after_capped_when_no_mirror_available(self):
        payload = b'ok-after-retry'
        digest = hashlib.sha256(payload).hexdigest()
        calls = 0
        sleeps = []

        def fake_urlopen(req, timeout=120):
            nonlocal calls
            calls += 1
            if calls == 1:
                hdrs = email.message.Message()
                hdrs['Retry-After'] = '9999'
                raise urllib.error.HTTPError('https://single.invalid/f', 429, 'Too Many Requests', hdrs, None)
            return io.BytesIO(payload)

        with tempfile.TemporaryDirectory() as tmp:
            old_dest = fetch.DEST
            fetch.DEST = Path(tmp)
            try:
                with mock.patch.object(fetch.urllib.request, 'urlopen', side_effect=fake_urlopen), \
                     mock.patch.object(fetch.time, 'sleep', side_effect=sleeps.append):
                    fetch.download(['https://single.invalid/f'], digest)
                    self.assertEqual(sleeps, [fetch.MAX_RETRY_WAIT])
            finally:
                fetch.DEST = old_dest

    def test_checksum_mismatch_removes_corrupt_file(self):
        digest = '0' * 64
        with tempfile.TemporaryDirectory() as tmp:
            old_dest = fetch.DEST
            fetch.DEST = Path(tmp)
            try:
                with mock.patch.object(fetch.urllib.request, 'urlopen', side_effect=lambda *a, **kw: io.BytesIO(b'bad')), \
                     mock.patch.object(fetch.time, 'sleep'):
                    with self.assertRaises(RuntimeError):
                        fetch.download(['https://primary.invalid/bad'], digest)
                self.assertFalse((Path(tmp) / 'downloads' / digest).exists())
            finally:
                fetch.DEST = old_dest

    def test_checksum_mismatch_fails_over_to_mirror(self):
        good = b'verified payload'
        digest = hashlib.sha256(good).hexdigest()
        seen = []

        def fake_urlopen(req, timeout=120):
            seen.append(req.full_url)
            return io.BytesIO(b'corrupt' if 'primary' in req.full_url else good)

        with tempfile.TemporaryDirectory() as tmp:
            old_dest = fetch.DEST
            fetch.DEST = Path(tmp)
            try:
                with mock.patch.object(fetch.urllib.request, 'urlopen', side_effect=fake_urlopen), \
                     mock.patch.object(fetch.time, 'sleep'):
                    out = fetch.download(['https://primary.invalid/f', 'https://mirror.invalid/f'], digest)
                self.assertEqual(seen, ['https://primary.invalid/f', 'https://mirror.invalid/f'])
                self.assertEqual(out.read_bytes(), good)
            finally:
                fetch.DEST = old_dest


if __name__ == '__main__':
    unittest.main()
