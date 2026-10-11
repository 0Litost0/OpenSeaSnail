"""Public command contract and milestone dispatch."""
import argparse
import datetime as dt
import json
from pathlib import Path
import signal
import sys

from . import checks
from .process import Busy, Controller, Interrupted, RepositoryLock, Residual, process_identity
from .state import Run, error, list_runs, now, query, result, source_identity

ROOT = Path(__file__).resolve().parents[2]


def utc(value):
    try:
        parsed=dt.datetime.fromisoformat(value.replace('Z','+00:00'))
        if parsed.tzinfo is None or parsed.utcoffset()!=dt.timedelta(0): raise ValueError()
        return parsed
    except ValueError: raise argparse.ArgumentTypeError('since must be an ISO-8601 UTC timestamp')


def limit(value):
    try: n=int(value)
    except ValueError: raise argparse.ArgumentTypeError('limit must be an integer')
    if not 1<=n<=100: raise argparse.ArgumentTypeError('limit must be between 1 and 100')
    return n


def parser():
    p=argparse.ArgumentParser(description='SeaSnail unified development workflow (no tool installation).')
    commands=p.add_subparsers(dest='command',required=True)
    for name in ('check','build','test','package','version','status','runs'):
        q=commands.add_parser(name)
        if name=='version':
            actions=q.add_subparsers(dest='action',required=True)
            for action in ('show','check','bump','increment','set'):
                a=actions.add_parser(action); a.add_argument('--json',action='store_true')
                if action=='bump': a.add_argument('part',choices=('patch','minor','major'))
                if action=='set': a.add_argument('value')
                if action in ('bump','increment','set'): a.add_argument('--dry-run',action='store_true')
            continue
        q.add_argument('--json',action='store_true')
        if name in ('build','package'): q.add_argument('--full',action='store_true')
        if name=='test': q.add_argument('--suite',choices=('unit','integration','all'),default='unit')
        if name=='status':
            group=q.add_mutually_exclusive_group(required=True)
            group.add_argument('run_id',nargs='?'); group.add_argument('--current',action='store_true')
        if name=='runs':
            q.add_argument('--operation',choices=('build','test','package','version.bump','version.increment','version.set'))
            q.add_argument('--since',type=utc); q.add_argument('--limit',type=limit,default=20)
    return p


def emit(data, machine):
    if machine: print(json.dumps(data,ensure_ascii=False))
    else:
        print(f"{data['operation']}: {data['status']} (stage: {data['stage']})")
        if data['run_id']: print('run_id: '+data['run_id'])
        for row in data['checks']: print(f"  {row['name']}: {row['state']}")
        for row in data['errors']: print(f"  {row['code']}: {row['reason']}\n  {row['required_action']}")
        for row in data.get('runs',[]): print(f"  {row['run_id']} {row['operation']} {row['status']} activity={row['activity']}")
        if data['log_path']: print('log: '+data['log_path'])


def unavailable(operation):
    return result(operation,stage='dispatch',errors=[error('operation_unavailable','dispatch',
        'This operation has not yet been integrated at the current milestone',
        action='Consult requirement_road_map.md; do not treat this result as a successful task')]),6


def dispatch(args, root=ROOT):
    name=args.command
    if name=='check':
        data=checks.inspect(root)
        code=data['errors'][0]['exit_code'] if data['errors'] else 0
        return result('check',status='checked' if not code else 'blocked',stage='check',finished_at=now(),
                      checks=data['checks'],errors=data['errors'],environment={'tools':data['tools']},
                      ready_for_task=data['ready_for_task'],prepared=False,
                      validation_scope=['read_only_environment','existing_declared_inputs'],
                      support_policy=checks.POLICY),code
    if name=='status':
        if args.current:
            return result('status.current',status='checked',stage='query',runs=list_runs(root,current=True,limit=100)),0
        return query(root,args.run_id),0
    if name=='runs':
        return result('runs',status='checked',stage='query',runs=list_runs(root,args.operation,args.since,args.limit)),0
    if name=='version' and (args.action in ('show','check') or args.dry_run):
        return unavailable('version.'+args.action)
    operation=name if name!='version' else 'version.'+args.action
    # Every modifying operation, including not-yet-integrated tasks, fails honestly
    # with a discoverable run; no preparation is performed by this dispatch stub.
    scope={key:getattr(args,key) for key in ('full','suite','part','value') if hasattr(args,key)}
    run=None
    try:
        with RepositoryLock(root) as lock:
            run=Run(root,operation,scope)
            run.data['owner']={'pid':__import__('os').getpid(),'identity':process_identity(__import__('os').getpid())}
            run.save()
            event=run.event('run_started',started_at=run.data['started_at'],
                            query_command=f'./scripts/dev_package/dev-package.sh status {run.id} --json')
            print('DEV_PACKAGE_EVENT '+json.dumps(event),file=sys.stderr,flush=True)
            with Controller(root,run,lock) as controller:
                run.data['source']=source_identity(root); run.save()
                if controller.signal: raise Interrupted(controller.signal)
                data,code=unavailable(operation)
                return run.finish(data['status'],code,stage=data['stage'],errors=data['errors']),code
    except Busy as exc:
        return result(operation,stage='lock',errors=[error('repository_busy','lock',str(exc),retry='safe',action='Query active runs; wait for the current task to finish')]),7
    except Residual as exc:
        data=result(operation,stage='lock',errors=[error('residual_processes','lock',str(exc))])
        return (run.finish('blocked',7,stage='lock',errors=data['errors']) if run else data),7
    except Interrupted as exc:
        code=128+exc.number
        data=run.finish('interrupted',code,errors=[error('interrupted',run.data['stage'],'Operation interrupted',retry='safe',action='Query this run before retrying')]) if run else result(operation,status='interrupted')
        return data,code
    except Exception as exc:
        code=8 if isinstance(exc,OSError) else 9
        row=error('filesystem_error' if code==8 else 'internal_error',run.data['stage'] if run else 'bootstrap',str(exc))
        if run:
            try: return run.finish('failed',code,errors=[row]),code
            except OSError: pass
        return result(operation,status='failed',errors=[row]),code


def main(argv=None):
    argv=list(sys.argv[1:] if argv is None else argv)
    if not argv: argv=['package']
    args=parser().parse_args(argv)
    # Validate all syntax before acquiring a lock or creating any directory.
    if args.command=='status' and args.run_id:
        from .state import RUN_ID
        if not RUN_ID.fullmatch(args.run_id): parser().error('invalid run_id')
    if args.command=='version' and args.action=='set':
        import re
        if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)',args.value): parser().error('version must be X.Y.Z with no leading zeroes')
    try: data,code=dispatch(args)
    except FileNotFoundError as exc:
        data,code=result(args.command,stage='query',errors=[error('run_not_found','query',str(exc))]),5
    except (OSError,ValueError,KeyError,json.JSONDecodeError) as exc:
        data,code=result(args.command,status='failed',errors=[error('record_unreadable','query',str(exc))]),9
    emit(data,args.json)
    return code


if __name__=='__main__': sys.exit(main())
