import argparse
import os
from pathlib import Path
import re
import sys
from ..errors import phase_main
from ..network import download
from ..state import safe_path
ROOT=Path(__file__).resolve().parents[3]


def prepare():
    parser=argparse.ArgumentParser(); parser.add_argument('--cache-dir',required=True)
    args=parser.parse_args()
    cache=Path(args.cache_dir)
    if not cache.is_absolute(): cache=ROOT/cache
    cache=safe_path(ROOT,cache.relative_to(ROOT))
    recipe=Path(__file__).with_name('build-ffmpeg-macos-arm64.sh').read_text()
    pin=re.search(r'^SHA256="([0-9a-f]{64})"$',recipe,re.M)[1]
    download(ROOT,cache,dict(file_name='FFmpeg-n9.0.1.tar.gz',url='https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/refs/tags/n9.0.1',sha256=pin,size_bytes=int(re.search(r'^ARCHIVE_SIZE_BYTES="([0-9]+)"$',recipe,re.M)[1])),os.environ.get('DEV_PACKAGE_RUN_ID','private-stage'))


if __name__=='__main__': sys.exit(phase_main(prepare))
