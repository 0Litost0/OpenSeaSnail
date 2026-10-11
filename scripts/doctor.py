#!/usr/bin/env python3
"""Compatibility entry: shared read-only dev-package prerequisites."""
import argparse
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from scripts.dev_package.checks import inspect, supported_node


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--frontend-only',action='store_true')
    args=parser.parse_args()
    data=inspect(frontend_only=args.frontend_only,inputs=False)
    for row in data['checks']:
        print(f"{'OK' if row['state']=='ready' else 'FAIL'} {row['name']}: {row.get('actual')}")
    for row in data['errors']: print(row['reason']+'; '+row['required_action'],file=sys.stderr)
    return 1 if data['errors'] else 0


if __name__=='__main__': sys.exit(main())
