#!/usr/bin/env python3
"""Prepare pinned, offline Picks inference assets for a supported release target."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

HERE = Path(__file__).resolve().parent
PINS = json.loads((HERE / 'pins.json').read_text(encoding='utf-8'))
MODEL_BASE = PINS['model_base']
ORT_DESKTOP = (PINS['ort_desktop']['urls'], PINS['ort_desktop']['sha256'])
ORT_ANDROID = (PINS['ort_android']['urls'], PINS['ort_android']['sha256'])
VC_REDIST = (PINS['vc_redist_x64']['urls'], PINS['vc_redist_x64']['sha256'])
RUNTIMES = PINS['runtimes']
ASSETS = {name: (spec['remote'], spec['sha256']) for name, spec in PINS['assets'].items()}
ROOT = HERE.parents[1]
DEST = ROOT / 'apps/veylo-dx/picks-ai'
MAX_RETRY_WAIT = 120
USER_AGENT = 'veylo-fetch/1.0'


def _sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def download(urls, digest):
    if isinstance(urls, str):
        urls = [urls]
    cache = DEST / 'downloads' / digest
    cache.parent.mkdir(parents=True, exist_ok=True)
    if not cache.exists() or _sha256_file(cache) != digest:
        print('Fetching', urls[0], flush=True)
        # Direct writes avoid this host's broken atomic-new-file links; retry transient errors and
        # rotate across configured mirrors (e.g. Google's Maven Central mirror on HTTP 429).
        last_err = None
        attempts = 7
        for attempt in range(attempts):
            source = urls[attempt % len(urls)]
            next_source = urls[(attempt + 1) % len(urls)]
            try:
                req = urllib.request.Request(source, headers={'User-Agent': USER_AGENT})
                with urllib.request.urlopen(req, timeout=120) as response, open(cache, 'wb') as out:
                    shutil.copyfileobj(response, out)
                last_err = None
                break
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                last_err = exc
                if cache.exists():
                    cache.unlink()
                if attempt < attempts - 1:
                    # Switching to a distinct mirror needs only a brief pause; retrying the same host
                    # honours a capped Retry-After or backs off exponentially up to 60 s.
                    if next_source != source:
                        wait = 2
                    else:
                        retry_after = getattr(exc, 'headers', None) and exc.headers.get('Retry-After')
                        if retry_after and retry_after.isdigit():
                            wait = min(int(retry_after), MAX_RETRY_WAIT)
                        else:
                            wait = min(2 ** (attempt + 1), 60)
                    print(f'Fetch attempt {attempt + 1} ({source}) failed ({exc}); retrying in {wait}s...', flush=True)
                    time.sleep(wait)
        if last_err is not None:
            raise last_err
    if _sha256_file(cache) != digest:
        cache.unlink(missing_ok=True)
        raise RuntimeError('Checksum mismatch: ' + urls[0])
    return cache

def prepare(target):
    shared = DEST / 'model'
    shared.mkdir(parents=True, exist_ok=True)
    for name, (remote, digest) in ASSETS.items():
        path = shared / name
        if not path.exists() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            shutil.copyfile(download(MODEL_BASE + remote, digest), path)
    for name in ['LICENSE', 'NOTICE']:
        shutil.copyfile(Path(__file__).parent / name, shared / name)
    dest = DEST / target
    dest.mkdir(parents=True, exist_ok=True)
    native_path = RUNTIMES[target]
    desktop_archive = download(*ORT_DESKTOP)
    runtime_archive = download(*ORT_ANDROID) if target.startswith('android-') else desktop_archive
    with zipfile.ZipFile(runtime_archive) as z:
        with z.open(native_path) as src, open(dest / Path(native_path).name, 'wb') as out:
            shutil.copyfileobj(src, out)
    # The Android AAR omits notices; include them from the matching desktop package.
    with zipfile.ZipFile(desktop_archive) as z:
        for name in ['LICENSE', 'ThirdPartyNotices.txt']:
            with z.open(name) as src, open(dest / name, 'wb') as out:
                shutil.copyfileobj(src, out)
    if target == 'windows-x64':
        prepare_windows_crt(dest)
    print('Prepared', target)

def find_bsdtar():
    # libarchive's tar reads the cabinets: bsdtar on Linux, the tar.exe that ships with Windows and macOS.
    system_tar = os.path.join(os.environ.get('SystemRoot', 'C:\\Windows'), 'System32', 'tar.exe')
    for path in [shutil.which('bsdtar'), shutil.which('tar'), system_tar]:
        if path and os.path.isfile(path) and 'bsdtar' in subprocess.run([path, '--version'], capture_output=True, text=True).stdout:
            return path
    raise RuntimeError('Windows asset preparation requires bsdtar (libarchive-tools).')

def prepare_windows_crt(dest):
    # App-local dependencies from Microsoft's pinned VC++ 2022 redistributable.
    # Extract only the four runtime DLLs ONNX Runtime imports, and retain its EULA.
    data = download(*VC_REDIST).read_bytes()
    bsdtar = find_bsdtar()
    # This pinned WiX Burn executable has a UX cabinet and a payload cabinet.
    with tempfile.TemporaryDirectory(dir=DEST) as work:
        work = Path(work)
        for offset in [479744, 686152]:
            if data[offset:offset+4] != b'MSCF':
                raise RuntimeError('Unexpected VC++ redistributable layout')
            size = struct.unpack_from('<I', data, offset + 8)[0]
            cabinet = work / f'{offset}.cab'
            cabinet.write_bytes(data[offset:offset+size])
            subprocess.run([bsdtar, '-xf', str(cabinet), '-C', str(work)], check=True)
        runtime_cabinet = work / 'a12'
        for name in ['msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll']:
            with open(dest / name, 'wb') as out:
                subprocess.run([bsdtar, '-xOf', str(runtime_cabinet), name + '_amd64'], stdout=out, check=True)
        shutil.copyfile(work / 'u4', dest / 'Microsoft-VC-Redist-License.rtf')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('targets', choices=RUNTIMES, nargs='+')
    for target in parser.parse_args().targets:
        prepare(target)
