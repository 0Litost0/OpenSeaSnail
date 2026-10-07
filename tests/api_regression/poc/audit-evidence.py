#!/usr/bin/env python3
"""Audit JSON attachments and embedded HTML report ZIPs, without printing secret values."""
import base64
import io
import json
from pathlib import Path
import re
import sys
import zipfile

ROOT=Path(__file__).resolve().parent
leaks=[]
checked=0
def scan(label,data):
    global checked
    checked+=1
    if any(marker in data for marker in [b'POC_PASSWORD_CANARY_20261002',b'POC_PROVIDER_CANARY_20261002']) or re.search(rb'ss_live_[A-Za-z0-9_-]{64}',data):
        leaks.append(label)
    for payload in re.findall(rb'data:application/zip;base64,([A-Za-z0-9+/=]+)',data):
        with zipfile.ZipFile(io.BytesIO(base64.b64decode(payload))) as archive:
            for name in archive.namelist(): scan(label+'::'+name,archive.read(name))
    if label.endswith('.json'):
        try: value=json.loads(data)
        except (ValueError,UnicodeError): return
        def walk(value):
            if isinstance(value,dict):
                if isinstance(value.get('body'),str) and value.get('contentType'):
                    try: decoded=base64.b64decode(value['body'],validate=True)
                    except ValueError: decoded=value['body'].encode()
                    scan(label+'::attachment',decoded)
                for child in value.values():walk(child)
            elif isinstance(value,list):
                for child in value:walk(child)
        walk(value)

paths=sys.argv[1:] or ['artifacts/validation']
for directory in paths:
    folder=ROOT/directory
    if not folder.is_dir():raise SystemExit(f'missing evidence directory: {directory}')
    for path in folder.rglob('*'):
        if path.is_file():scan(str(path.relative_to(ROOT)),path.read_bytes())
result={'scope':paths,'checked_files_and_embedded_items':checked,'matches':leaks,'passed':not leaks,'limitation':'Only declared synthetic canaries and full SeaSnail token syntax; not proof of general secret redaction.'}
print(json.dumps(result,indent=2))
raise SystemExit(1 if leaks else 0)
