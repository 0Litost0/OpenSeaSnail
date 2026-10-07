import fs from 'node:fs/promises';
import path from 'node:path';
import {test,expect,type Controller,type Child} from '../fixtures/system.js';
import {ConfigurationError} from '../cli/replay.mjs';
import {hash,host,project} from '../cli/identity.mjs';

const systemBudget=JSON.parse(await fs.readFile(new URL('../assets/system-budgets.v1.json',import.meta.url),'utf8')).milliseconds;

function passed(child:Child,ids:string[]) {
  expect(child.exit_code).toBe(0);expect(child.result.gate).toBe('passed');
  expect(child.result.selection.actual_case_ids.slice().sort()).toEqual(ids.slice().sort());
  for(const entry of child.result.cases) {
    expect(entry.flaky).toBe(false);expect(entry.attempts).toHaveLength(1);
    expect(entry.attempts[0].execution_status).toBe('passed');expect(entry.attempts[0].teardown_status).toBe('passed');
  }
}
async function proof(child:Child,id:string) {return JSON.parse(await fs.readFile(path.join(child.directory,`environment-proof/${id}-0.json`),'utf8'))}

test('SYSTEM-001 新环境单跑、重复 3 次和组合执行的判断一致，无顺序依赖',{tag:'@case_SYSTEM-001'},async({controller})=>{
  test.setTimeout(systemBudget['SYSTEM-001']);
  const representatives=['AUTH-002','CLEAN-002','DICT-001'];const homes=new Set<string>();const runs=new Set<string>();
  async function verify(child:Child,ids:string[]) {
    passed(child,ids);expect(runs.has(child.result.run_id)).toBe(false);runs.add(child.result.run_id);
    for(const id of ids){const evidence=await proof(child,id);expect(evidence.cleanup_status).toBe('passed');expect(homes.has(evidence.home_identity_sha256)).toBe(false);homes.add(evidence.home_identity_sha256)}
  }
  for(const id of representatives)await controller.step(`${id} 独立运行及重复三次`,async()=>{
    for(let index=0;index<3;index++)await verify(await controller.run(['run',id]),[id]);
  });
  await controller.step('代表用例组合执行，使用与单跑相同的判断标准',async()=>{
    const catalog=JSON.parse(await fs.readFile(path.join(project,'case-catalog.json'),'utf8'));
    await verify(await controller.run(['quick']),catalog.cases.filter((entry:any)=>entry.quick).map((entry:any)=>entry.case_id));
  });
});

async function detection(controller:Controller,faultId:string,id:string,regressions:string[][]) {
  const normalHash=await hash(host);
  const productFile=path.resolve(project,'../../crates/daemon/src/application',id==='CLEAN-002'?'transcription.rs':'dictionary.rs');const productHash=await hash(productFile);let original:Child;let reproduced:Child;
  function rejected(child:Child) {
    if(child.result.gate==='incomplete')throw new ConfigurationError('fault_child_environment_incomplete');
    expect(child.exit_code).toBe(1);expect(child.result.gate).toBe('failed');
    const attempt=child.result.cases[0].attempts[0];expect(attempt.execution_status).toBe('failed');expect(attempt.teardown_status).toBe('passed');
    expect(attempt.config.fault.id).toBe(faultId);expect(attempt.config.fault.binary_sha256).toBe(child.result.build.binary_sha256);
    expect(attempt.errors.some((error:any)=>error.category==='business'&&error.expected!==null&&error.actual!==null)).toBe(true);
  }
  await controller.step(`${faultId} 隔离错误构建被原 ${id} 断言检测`,async()=>{
    original=await controller.run(['run',id,'--fault',faultId]);rejected(original);
    expect(await hash(host)).toBe(normalHash);
  });
  await controller.step(`${faultId} 从记录重建构建，在第二个新环境精确 replay`,async()=>{
    const fault=original.result.cases[0].attempts[0].config.fault;
    const project=path.resolve(original.directory,'../../..');
    // Remove the entire mutation build: replay must reconstruct it from source/patch assets.
    const workspace=path.join(project,'artifacts/restricted/mutation-workspaces',fault.base_source.sha256,faultId);
    // Keep the isolated compiler cache; discard the source and host executable.
    await fs.rm(path.join(workspace,'source'),{recursive:true,force:true});await fs.rm(path.join(workspace,'target/debug/seasnail-api-test-host'),{force:true});
    reproduced=await controller.run(['replay',path.join(original.directory,`replay/${id}.json`)]);rejected(reproduced);
    expect(reproduced.result.build).toEqual(original.result.build);
    const a=await proof(original,id);const b=await proof(reproduced,id);expect(a.run_id).not.toBe(b.run_id);expect(a.home_identity_sha256).not.toBe(b.home_identity_sha256);
    expect(reproduced.result.cases[0].attempts[0].errors.find((error:any)=>error.category==='business').actual).toBe(original.result.cases[0].attempts[0].errors.find((error:any)=>error.category==='business').actual);
    const comparison=JSON.parse(await fs.readFile(path.join(reproduced.directory,'comparison.json'),'utf8'));expect(comparison.exact_replay).toBe(true);
    await fs.rm(path.join(workspace,'source'),{recursive:true,force:true});
  });
  await controller.step(`${faultId} 撤销故障，当前正常构建重跑目标`,async()=>{
    const repaired=await controller.run(['run',id,'--config',path.join(original.directory,`replay/${id}.json`)]);passed(repaired,[id]);
    expect(repaired.result.cases[0].attempts[0].config.fault).toBeNull();expect(repaired.result.build.binary_sha256).toBe(normalHash);
    const comparison=JSON.parse(await fs.readFile(path.join(repaired.directory,'comparison.json'),'utf8'));expect(comparison.build_changed).toBe(true);
  });
  for(const args of regressions)await controller.step(`${faultId} 撤销后的相关回归 ${args.join(' ')}`,async()=>{
    const child=await controller.run(args);expect(child.result.gate).toBe('passed');expect(child.exit_code).toBe(0);
  });
  expect(await hash(host)).toBe(normalHash);expect(await hash(productFile)).toBe(productHash);
}

test('SYSTEM-003 原业务断言检测错误；新环境 replay 同样失败；撤销故障后该用例和相关套件通过',{tag:'@case_SYSTEM-003'},async({controller})=>{
  test.setTimeout(systemBudget['SYSTEM-003']);
  await detection(controller,'MUT-CLEAN-FALLBACK-001','CLEAN-002',[['suite','clean']]);
  await detection(controller,'MUT-DICT-PERSIST-001','DICT-001',[['suite','dictionary'],['run','RECOVERY-001']]);
});

test('SYSTEM-004 隔离新增 case、资产、规则和登记即可 list、单跑与组合；默认必验集合不变',{tag:'@case_SYSTEM-004'},async({controller})=>{
  test.setTimeout(systemBudget['SYSTEM-004']);
  const {execFile}=await import('node:child_process');const {promisify}=await import('node:util');const exec=promisify(execFile);
  const {executionAssets}=await import('../cli/identity.mjs');
  const files=[...await executionAssets(),'scripts/check_contracts.py','playwright.config.ts','contracts/acceptance-required.v1.json','case-catalog.json'];
  const before=await Promise.all(files.map(file=>hash(path.join(project,file))));
  const directory=path.join(project,'examples',`.isolated-${process.env.API_TEST_RUN_ID}`);
  await fs.cp(path.join(project,'examples/new-case'),directory,{recursive:true});
  try{
    const manifest=JSON.parse(await fs.readFile(path.join(directory,'extension.json'),'utf8'));
    for(const entry of manifest.cases)entry.asset_refs=entry.asset_refs.map((file:string)=>file.replace('examples/new-case',path.relative(project,directory)));
    await fs.writeFile(path.join(directory,'extension.json'),JSON.stringify(manifest,null,2),{mode:0o600});
    await controller.step('隔离扩展声明独立范围并被 list 发现',async()=>{
      const listed=JSON.parse((await exec(process.execPath,[path.join(project,'cli/api-test.mjs'),'list','--json','--extension',directory],{timeout:30000,maxBuffer:262144})).stdout);
      expect(listed.cases).toHaveLength(24);expect(listed.cases.find((entry:any)=>entry.case_id==='DICT-003')).toMatchObject({required:false,quick:false,discovered:true});
    });
    await controller.step('新增示例单跑与原词典套件组合，无通用执行器改动',async()=>{
      passed(await controller.run(['run','DICT-003','--extension',directory]),['DICT-003']);
      passed(await controller.run(['suite','dictionary','--extension',directory]),['DICT-001','DICT-002','DICT-003']);
    });
    await controller.step('默认登记、22 必验快照及执行器内容均不变',async()=>{
      expect(await Promise.all(files.map(file=>hash(path.join(project,file))))).toEqual(before);
      const listed=JSON.parse((await exec(process.execPath,[path.join(project,'cli/api-test.mjs'),'list','--json'],{timeout:30000,maxBuffer:262144})).stdout);
      expect(listed.cases).toHaveLength(23);expect(listed.cases.filter((entry:any)=>entry.required)).toHaveLength(22);expect(listed.cases.some((entry:any)=>entry.case_id==='DICT-003')).toBe(false);
    });
  }finally{await fs.rm(directory,{recursive:true,force:true})}
});
