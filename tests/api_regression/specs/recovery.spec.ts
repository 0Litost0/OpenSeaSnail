// RECOVERY-001 / RECOVERY-002 business cases (suite: lifecycle).
// Restart means the old host process exits before a new one starts (the
// environment manager verifies process identity); automatic recovery (token
// still works) is checked before any unlock, and credential-loss recovery is
// a separate path.
import {test,expect,type Scenario} from '../fixtures/test.js';

async function expectError(scenario:Scenario,method:string,route:string,status:number,code:string,token?:string|null,data?:unknown) {
  const response=await scenario.api.response(method,route,{data,token});
  expect(response.status(),`${method} ${route} status`).toBe(status);
  const body=await response.json();
  expect(body?.error?.code,`${method} ${route} error code`).toBe(code);
}

test('RECOVERY-001 真正停旧进程再启动；不先 unlock，原 token 可用、账户活跃、数据和 provider 认证可用',{tag:'@case_RECOVERY-001'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  await scenario.api.json('POST','/dictionary/entries',{terms:['恢复词条']},200);
  await scenario.api.configureProvider(scenario.provider.endpoint);
  const firstSession=await scenario.api.upload('realtime','zh');
  await scenario.api.terminal(firstSession,'completed');
  await scenario.step('真正停旧进程再启动',async()=>{
    const before=(scenario.env.manifest.hosts as Array<{pid:number}>).map(identity=>identity.pid);
    await scenario.api.restart();
    const after=(scenario.env.manifest.hosts as Array<{pid:number}>).map(identity=>identity.pid);
    expect(after).toHaveLength(before.length+1);
    expect(after[after.length-1]).not.toBe(before[before.length-1]);
  });
  await scenario.step('不先 unlock，原 token 可用、账户活跃、数据和 provider 认证可用',async()=>{
    const accounts=await scenario.api.json('GET','/accounts',undefined,200,a.secret);
    expect(accounts).toEqual([expect.objectContaining({id:a.account_id,is_active:true})]);
    const dictionary=await scenario.api.json('GET','/dictionary',undefined,200,a.secret);
    expect(dictionary.items.map((entry:{term:string})=>entry.term)).toContain('恢复词条');
    const workspace=await scenario.api.json('GET',`/sessions/${firstSession}/workspace-detail`,undefined,200,a.secret);
    expect(workspace.cleanup_status).toBe('succeeded');
    expect(workspace.final_text).toBe('海螺支持语音转写和词典功能。');
    const secondSession=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(secondSession,'completed');
    expect(scenario.provider.requests).toHaveLength(2);
    expect(scenario.provider.requests[1].authenticated).toBe(true);
  });
});

test('RECOVERY-002 服务存活但原 token 423；密码 unlock 后数据恢复，不能重新 setup',{tag:'@case_RECOVERY-002'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  await scenario.api.json('POST','/dictionary/entries',{terms:['恢复词条']},200);
  await scenario.step('服务存活但原 token 423',async()=>{
    await scenario.api.restart('read-error');
    const status=await scenario.api.json('GET','/auth/status',undefined,200,null);
    expect(status.initialized).toBe(true);
    await expectError(scenario,'GET','/dictionary',423,'locked',a.secret);
    await expectError(scenario,'GET','/accounts',423,'locked',a.secret);
  });
  await scenario.step('密码 unlock 后数据恢复，不能重新 setup',async()=>{
    const unlocked=await scenario.api.unlock(a.account_id);
    expect(unlocked.account_id).toBe(a.account_id);
    const dictionary=await scenario.api.json('GET','/dictionary');
    expect(dictionary.items.map((entry:{term:string})=>entry.term)).toContain('恢复词条');
    await expectError(scenario,'POST','/auth/setup',410,'gone',null,{username:'fixture-second',password:scenario.api.password});
  });
});
