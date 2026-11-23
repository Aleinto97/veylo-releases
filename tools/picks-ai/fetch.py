#!/usr/bin/env python3
"""Prepare pinned, offline Picks inference assets for a supported release target."""
import argparse
import hashlib
import io
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

REVISION = 'ea104dacec62c0de699686887e3f920caeb4f3e3'
MODEL_BASE = f'https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/{REVISION}/'
# Microsoft's NuGet package contains the same native binaries as the desktop release archives.
ORT_DESKTOP = (
    'https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.22.0/microsoft.ml.onnxruntime.1.22.0.nupkg',
    'd571e63a2329baacb713f441e65ad75284de354db6e1ac435fe4bebbb417986a',
)
ORT_ANDROID = (
    'https://repo.maven.apache.org/maven2/com/microsoft/onnxruntime/onnxruntime-android/1.22.0/onnxruntime-android-1.22.0.aar',
    '04a4617a9c797cf49225595e45b5546081cb34c86ac817581141577d3b7dbfe2',
)
RUNTIMES = {
    'linux-aarch64': 'runtimes/linux-arm64/native/libonnxruntime.so',
    'linux-x64': 'runtimes/linux-x64/native/libonnxruntime.so',
    'windows-x64': 'runtimes/win-x64/native/onnxruntime.dll',
    'macos-arm64': 'runtimes/osx-arm64/native/libonnxruntime.dylib',
    'android-arm64': 'jni/arm64-v8a/libonnxruntime.so',
    'android-armv7': 'jni/armeabi-v7a/libonnxruntime.so',
}
ASSETS = {
    'model.onnx': ('onnx/model_quantized.onnx', '6c9c6101a956d62dfb5e7190c538226c0c5bb9cb27b651234b6df063ee7dbfe4'),
    'tokenizer.json': ('tokenizer.json', 'd241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66'),
}
ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'apps/veylo-dx/picks-ai'

def download(url, digest):
    cache = DEST / 'downloads' / digest
    cache.parent.mkdir(parents=True, exist_ok=True)
    if not cache.exists() or hashlib.sha256(cache.read_bytes()).hexdigest() != digest:
        print('Fetching', url, flush=True)
        # Direct writes avoid this host's broken atomic-new-file links; retry transient 5xx/network errors.
        last_err = None
        for attempt in range(4):
            try:
                with urllib.request.urlopen(url, timeout=120) as response, open(cache, 'wb') as out:
                    shutil.copyfileobj(response, out)
                last_err = None
                break
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                last_err = exc
                if attempt < 3:
                    wait = 2 ** (attempt + 1)
                    print(f'Fetch attempt {attempt + 1} failed ({exc}); retrying in {wait}s...', flush=True)
                    time.sleep(wait)
        if last_err is not None:
            raise last_err
    if hashlib.sha256(cache.read_bytes()).hexdigest() != digest:
        raise RuntimeError('Checksum mismatch: ' + url)
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
    url = 'https://download.visualstudio.microsoft.com/download/pr/e9604e21-4b79-4096-a4b3-1b63d4b55b4d/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe'
    data = download(url, 'cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b').read_bytes()
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
