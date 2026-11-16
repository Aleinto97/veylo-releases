#!/usr/bin/env python3
"""Prepare pinned, offline Picks inference assets for a supported release target."""
import argparse
import hashlib
import io
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import tarfile
import urllib.request
import zipfile

REVISION = 'ea104dacec62c0de699686887e3f920caeb4f3e3'
MODEL_BASE = f'https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/{REVISION}/'
ORT_BASE = 'https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/'
RUNTIMES = {
    'linux-aarch64': (ORT_BASE + 'onnxruntime-linux-aarch64-1.22.0.tgz', 'bb76395092d150b52c7092dc6b8f2fe4d80f0f3bf0416d2f269193e347e24702'),
    'linux-x64': (ORT_BASE + 'onnxruntime-linux-x64-1.22.0.tgz', '8344d55f93d5bc5021ce342db50f62079daf39aaafb5d311a451846228be49b3'),
    'windows-x64': (ORT_BASE + 'onnxruntime-win-x64-1.22.0.zip', '174c616efc0271194488642a72f1a514e01487da4dfe84c49296d66e40ebe0da'),
    'android-arm64': ('https://repo.maven.apache.org/maven2/com/microsoft/onnxruntime/onnxruntime-android/1.22.0/onnxruntime-android-1.22.0.aar', '04a4617a9c797cf49225595e45b5546081cb34c86ac817581141577d3b7dbfe2'),
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
        # Direct writes avoid this host's broken atomic-new-file links.
        with urllib.request.urlopen(url, timeout=120) as response, open(cache, 'wb') as out:
            shutil.copyfileobj(response, out)
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
    url, digest = RUNTIMES[target]
    archive = download(url, digest)
    dest = DEST / target
    dest.mkdir(parents=True, exist_ok=True)
    if url.endswith('.tgz'):
        with tarfile.open(archive) as tar:
            for member in tar.getmembers():
                if member.isfile() and (Path(member.name).name.startswith('libonnxruntime.so.') or Path(member.name).name in ['LICENSE', 'ThirdPartyNotices.txt']):
                    name = 'libonnxruntime.so' if '.so.' in member.name else Path(member.name).name
                    with tar.extractfile(member) as src, open(dest / name, 'wb') as out:
                        shutil.copyfileobj(src, out)
    else:
        with zipfile.ZipFile(archive) as z:
            for name in z.namelist():
                base = Path(name).name
                runtime = (target == 'android-arm64' and name == 'jni/arm64-v8a/libonnxruntime.so') or (target == 'windows-x64' and base == 'onnxruntime.dll')
                if runtime or base in ['LICENSE', 'LICENSE.txt', 'ThirdPartyNotices.txt']:
                    with z.open(name) as src, open(dest / base, 'wb') as out:
                        shutil.copyfileobj(src, out)
    if target == 'android-arm64':
        # The Maven AAR omits notices; ship those from the matching pinned release.
        notice_url, notice_digest = RUNTIMES['linux-x64']
        with tarfile.open(download(notice_url, notice_digest)) as tar:
            for member in tar.getmembers():
                if Path(member.name).name in ['LICENSE', 'ThirdPartyNotices.txt']:
                    with tar.extractfile(member) as src, open(dest / Path(member.name).name, 'wb') as out:
                        shutil.copyfileobj(src, out)
    if target == 'windows-x64':
        prepare_windows_crt(dest)
    print('Prepared', target)

def prepare_windows_crt(dest):
    # App-local dependencies from Microsoft's pinned VC++ 2022 redistributable.
    # Extract only the four runtime DLLs ONNX Runtime imports, and retain its EULA.
    url = 'https://download.visualstudio.microsoft.com/download/pr/e9604e21-4b79-4096-a4b3-1b63d4b55b4d/CC0FF0EB1DC3F5188AE6300FAEF32BF5BEEBA4BDD6E8E445A9184072096B713B/VC_redist.x64.exe'
    data = download(url, 'cc0ff0eb1dc3f5188ae6300faef32bf5beeba4bdd6e8e445a9184072096b713b').read_bytes()
    if not shutil.which('bsdtar'):
        raise RuntimeError('Windows asset preparation requires bsdtar (libarchive-tools).')
    # This pinned WiX Burn executable has a UX cabinet and a payload cabinet.
    with tempfile.TemporaryDirectory(dir=DEST) as work:
        work = Path(work)
        for offset in [479744, 686152]:
            if data[offset:offset+4] != b'MSCF':
                raise RuntimeError('Unexpected VC++ redistributable layout')
            size = struct.unpack_from('<I', data, offset + 8)[0]
            cabinet = work / f'{offset}.cab'
            cabinet.write_bytes(data[offset:offset+size])
            subprocess.run(['bsdtar', '-xf', str(cabinet), '-C', str(work)], check=True)
        runtime_cabinet = work / 'a12'
        for name in ['msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll']:
            with open(dest / name, 'wb') as out:
                subprocess.run(['bsdtar', '-xOf', str(runtime_cabinet), name + '_amd64'], stdout=out, check=True)
        shutil.copyfile(work / 'u4', dest / 'Microsoft-VC-Redist-License.rtf')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('targets', choices=RUNTIMES, nargs='+')
    for target in parser.parse_args().targets:
        prepare(target)
