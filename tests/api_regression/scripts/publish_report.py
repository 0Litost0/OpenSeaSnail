#!/usr/bin/env python3
"""Bounded fail-closed report publication. Never print source values or paths."""
import argparse, base64, copy, html, io, json, os, re, shutil, stat, sys, time, zipfile
from pathlib import Path
from check_contracts import result_check, outcome
ROOT=Path(__file__).resolve().parents[1]
BUDGET=json.loads((ROOT/'assets/budgets.v1.json').read_text())
FORBIDDEN=re.compile(rb'ss_live_[A-Za-z0-9_-]{64}|Bearer\s+[A-Za-z0-9._~+/=-]{16,}|(?:POC|API_TEST)_[A-Z_]*CANARY',re.I)
SECRET_KEYS={'authorization','password','secret','credential','api_key','secret_hash','headers'}

class UnsafeReport(Exception): pass
class Scanner:
    def __init__(self,secrets):
        self.secrets=[value.encode() for value in secrets if len(value)>=8]
        self.bytes=0;self.entries=0;self.started=time.monotonic()
    def scan(self,label,data,depth=0):
        self.bytes+=len(data);self.entries+=1
        if depth>12 or self.bytes>BUDGET['evidence']['zip_expanded_bytes_max'] or self.entries>BUDGET['evidence']['zip_entries_max'] or time.monotonic()-self.started>BUDGET['milliseconds']['report_check']/1000:raise UnsafeReport()
        if FORBIDDEN.search(data) or any(secret in data for secret in self.secrets):raise UnsafeReport()
        try:
            decoded=html.unescape(data.decode('utf8')).encode()
            if FORBIDDEN.search(decoded) or any(secret in decoded for secret in self.secrets):raise UnsafeReport()
        except UnicodeError:pass
        if label.endswith('.zip') or data.startswith(b'PK\x03\x04'):
            try:
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    for member in archive.infolist():
                        parts=Path(member.filename).parts
                        if member.filename.startswith(('/', '\\')) or '..' in parts or stat.S_ISLNK(member.external_attr>>16) or member.flag_bits&1 or member.file_size>BUDGET['evidence']['zip_expanded_bytes_max']-self.bytes:raise UnsafeReport()
                        if not member.is_dir():self.scan(member.filename,archive.read(member),depth+1)
            except (ValueError,zipfile.BadZipFile,RuntimeError,OSError):raise UnsafeReport()
        for payload in re.findall(rb'data:application/zip;base64,([A-Za-z0-9+/=]+)',data):
            try:self.scan('embedded.zip',base64.b64decode(payload,validate=True),depth+1)
            except ValueError:raise UnsafeReport()
        if label.endswith('.json'):
            try:value=json.loads(data)
            except (ValueError,UnicodeError):raise UnsafeReport()
            self.walk(value,depth)
    def walk(self,value,depth):
        if isinstance(value,str):
            data=value.encode()
            if FORBIDDEN.search(data) or any(secret in data for secret in self.secrets):raise UnsafeReport()
        elif isinstance(value,list):
            for child in value:self.walk(child,depth)
        elif isinstance(value,dict):
            for key,child in value.items():
                if key.lower() in SECRET_KEYS and child not in (None,'',[],{}):raise UnsafeReport()
                self.walk(key,depth);self.walk(child,depth)
            if isinstance(value.get('body'),str) and value.get('contentType'):
                try:body=base64.b64decode(value['body'],validate=True)
                except ValueError:body=value['body'].encode()
                kind='.json' if value['contentType']=='application/json' else '.txt'
                self.scan('attachment'+kind,body,depth+1)

def filtered(value):
    if isinstance(value,dict):return {key:filtered(child) for key,child in value.items() if key.lower() not in SECRET_KEYS}
    if isinstance(value,list):return [filtered(child) for child in value]
    return value

def publish(raw,destination):
    if raw.is_symlink() or destination.parent.is_symlink() or raw.resolve()!=raw or destination.exists():raise UnsafeReport()
    result=json.loads((raw/'result.json').read_text())
    secrets=[]
    if (raw/'secrets.ndjson').exists():
        secrets=[json.loads(line) for line in (raw/'secrets.ndjson').read_text().splitlines() if line]
    scanner=Scanner(secrets);total=0
    for file in raw.rglob('*'):
        if file.is_symlink():raise UnsafeReport()
        if file.is_file() and file!=raw/'secrets.ndjson':
            total+=file.stat().st_size
            if total>BUDGET['evidence']['raw_bytes_per_run']:raise UnsafeReport()
            scanner.scan(file.name,file.read_bytes())
    staging=destination.with_name('.publish-'+destination.name)
    if staging.exists():shutil.rmtree(staging)
    staging.mkdir(parents=True,mode=0o700)
    try:
        # Only native report, allowlisted attachments and v1 result/replay are published.
        for name in ['html','attachments','playwright.json','result.json','replay','comparison.json','environment-proof','run-manifest.json']:
            source=raw/name
            if not source.exists():continue
            target=staging/name
            if source.is_dir():shutil.copytree(source,target)
            else:shutil.copyfile(source,target)
        result['report_status']='failed' if result['report_status']=='failed' else 'passed';result['gate'],result['exit_code']=outcome(result);result_check(result)
        (staging/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
        check=Scanner(secrets);published=0
        for file in staging.rglob('*'):
            if file.is_dir():file.chmod(0o700)
            if file.is_file():
                if file.suffix=='.json':file.write_text(json.dumps(filtered(json.loads(file.read_text())),ensure_ascii=False,indent=2))
                published+=file.stat().st_size
                if published>BUDGET['evidence']['published_bytes_per_run']:raise UnsafeReport()
                check.scan(file.name,file.read_bytes());file.chmod(0o600)
        # Removal is part of publication success; fail closed if raw evidence survives.
        shutil.rmtree(raw)
        os.rename(staging,destination)
        return result
    except Exception:
        shutil.rmtree(staging,ignore_errors=True);raise

def main():
    parser=argparse.ArgumentParser();parser.add_argument('raw',type=Path);parser.add_argument('destination',type=Path);args=parser.parse_args()
    try:
        args.destination.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
        result=publish(args.raw,args.destination)
        print(json.dumps({'report_status':'passed','gate':result['gate'],'exit_code':result['exit_code']}))
        return 0
    except Exception:
        print(json.dumps({'report_status':'failed','code':'report_check_failed'}));return 1
    finally:
        # Restricted source identities are independent local replay assets, not raw reports.
        if args.raw.exists() and not args.raw.is_symlink():shutil.rmtree(args.raw,ignore_errors=True)
if __name__=='__main__':raise SystemExit(main())
