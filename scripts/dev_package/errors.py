"""Classified stage failures, independent of human log text."""
import os
from pathlib import Path
from .state import atomic_json, error


class Failure(Exception):
    def __init__(self, exit_code, code, reason, *, path=None, child_code=None, retry='investigate'):
        super().__init__(reason)
        self.exit_code=exit_code
        self.detail=error(code, 'prepare', reason, path=path, child_code=child_code, retry=retry,
                          action='Inspect the reported input/environment and retry after fixing the cause')


def phase_main(callback):
    try: callback(); return 0
    except Failure as exc:
        failure=exc
    except OSError as exc:
        failure=Failure(8,'filesystem_error',str(exc))
    except (ValueError,KeyError,TypeError) as exc:
        failure=Failure(5,'input_invalid',str(exc))
    except Exception as exc:
        failure=Failure(9,'phase_internal_error',str(exc))
    report=os.environ.get('DEV_PACKAGE_ERROR_REPORT')
    if report:
        try: atomic_json(Path(report),{'schema_version':1,'exit_code':failure.exit_code,'error':failure.detail})
        except OSError: return 8
    print(f"{failure.detail['code']}: {failure}",flush=True)
    return failure.exit_code
