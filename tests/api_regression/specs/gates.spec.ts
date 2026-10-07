import fs from 'node:fs/promises';
import path from 'node:path';
import {randomBytes} from 'node:crypto';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {test,expect} from '../fixtures/system.js';
import {project} from '../cli/identity.mjs';
const exec=promisify(execFile);
const budget=JSON.parse(await fs.readFile(new URL('../assets/system-budgets.v1.json',import.meta.url),'utf8')).milliseconds;
const extension=path.join(project,'examples/gate-failures');

test('SYSTEM-002 依赖、超时、强杀、flaky、缺项与 canary 均诚实拒绝，异常资源可恢复',{tag:'@case_SYSTEM-002'},async({controller})=>{
  test.setTimeout(budget['SYSTEM-002']);
  await controller.step('缺少真实制品为环境错误，不能污染为业务失败',async()=>{
    const child=await controller.run(['run','SHERPA-001'],{env:{SEASNAIL_ASR_ROOT:path.join(project,'artifacts/restricted/missing-model')}});
    expect(child.exit_code).toBe(2);expect(child.result.gate).toBe('incomplete');
    const attempt=child.result.cases[0].attempts[0];expect(attempt.execution_status).toBe('environment_error');expect(attempt.teardown_status).toBe('passed');
    expect(attempt.errors.some((error:any)=>error.category==='environment')).toBe(true);expect(attempt.errors.some((error:any)=>error.category==='business')).toBe(false);
  });
  await controller.step('业务超时失败，与独立 teardown 结果同时保留',async()=>{
    const child=await controller.run(['run','AUTH-005','--extension',extension]);
    expect(child.exit_code).toBe(1);expect(child.result.gate).toBe('failed');
    const attempt=child.result.cases[0].attempts[0];expect(attempt.execution_status).toBe('failed');expect(attempt.teardown_status).toBe('passed');expect(attempt.errors.some((error:any)=>error.category==='timeout')).toBe(true);
  });
  await controller.step('首败重试成功仍为 flaky，两个 attempt 使用不同 home',async()=>{
    const child=await controller.run(['run','AUTH-006','--extension',extension,'--','--retries=1']);
    expect(child.exit_code).toBe(1);expect(child.result.gate).toBe('failed');const entry=child.result.cases[0];expect(entry.flaky).toBe(true);expect(entry.attempts.map((attempt:any)=>attempt.execution_status)).toEqual(['failed','passed']);
    expect(entry.attempts.every((attempt:any)=>attempt.teardown_status==='passed')).toBe(true);
    const a=JSON.parse(await fs.readFile(path.join(child.directory,'environment-proof/AUTH-006-0.json'),'utf8'));const b=JSON.parse(await fs.readFile(path.join(child.directory,'environment-proof/AUTH-006-1.json'),'utf8'));expect(a.home_identity_sha256).not.toBe(b.home_identity_sha256);
  });
  await controller.step('必验筛选仍保留独立 22 项与 21 缺项，不递归执行完整验收',async()=>{
    const child=await controller.run(['acceptance','--','--grep','@case_AUTH-001']);
    expect(child.exit_code).toBe(2);expect(child.result.gate).toBe('incomplete');expect(child.result.required_case_ids).toHaveLength(22);expect(child.result.missing_case_ids).toHaveLength(21);expect(child.result.selection.actual_case_ids).toEqual(['AUTH-001']);
  });
  await controller.step('原生 canary 阻止发布，原 raw 销毁，仅安全失败摘要留存',async()=>{
    const secret='API_TEST_REPORT_CANARY_'+randomBytes(16).toString('hex');
    const child=await controller.run(['run','AUTH-007','--extension',extension],{env:{REPORT_FAILURE_SECRET:secret},allowReportFailure:true});
    expect(child.exit_code).toBe(1);expect(child.result.report_status).toBe('failed');expect(child.result.gate).toBe('failed');expect(JSON.stringify(child.result)).not.toContain(secret);
    await expect(fs.stat(path.join(project,'artifacts/restricted/runs',child.result.run_id))).rejects.toMatchObject({code:'ENOENT'});
  });
  await controller.step('真实 Sherpa sidecar 存活时强杀框架 runner，原报告 interrupted/exit 2，stale 回收全部所属资源',async()=>{
    const child=await controller.run(['run','SHERPA-001'],{interruptSherpa:true});
    expect(child.exit_code).toBe(2);expect(child.result.gate).toBe('incomplete');expect(child.result.errors.some((error:any)=>error.category==='interrupted')).toBe(true);
    expect(child.result.cases[0].attempts[0].execution_status).toBe('interrupted');expect(child.recovery).toMatchObject({cleanup_status:'passed',home_removed:true,host_gone:true,sidecar_gone:true});
  });
  await controller.step('身份不明不误杀、PID 重用/清理失败保留恢复依据，质量负例明确拒绝',async()=>{
    const environment=await exec(process.execPath,['--test',path.join(project,'scripts/test_environment.mjs')],{timeout:60000,maxBuffer:65536});
    expect(environment.stdout).toContain('# pass 15');expect(environment.stdout).toContain('# fail 0');
    const assets=await exec('python3',[path.join(project,'scripts/check_assets.py')],{timeout:5000,maxBuffer:8192});expect(assets.stdout).toContain('6 negative controls rejected');
    const proof=path.join(process.env.API_TEST_RAW_DIR!,'environment-proof',`SYSTEM-002-checks-${process.env.API_TEST_RUN_ID}.json`);
    await fs.writeFile(proof,JSON.stringify({environment_checks:15,quality_negative_controls:6,quality_rule:'quality-v3',note:'negative controls validate scorer rejection; they are not real ASR acceptance outputs'},null,2),{mode:0o600});
  });
});
