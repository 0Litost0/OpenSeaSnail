// AUTH-001..AUTH-004 business cases (suite: auth, level: deterministic).
// Assertions follow contracts/assertions.v1.json and the fixed matrix in
// docs/design.md; every step goes through the production API routes and
// auth middleware. Secrets are never placed in titles or asserted values.
import {randomBytes} from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import {test,expect,type Scenario} from '../fixtures/test.js';

const ROOT_SCOPES=['sessions:read','sessions:write','sessions:delete','tokens:manage'];

async function expectError(scenario:Scenario,method:string,route:string,status:number,code:string,token:string|null,data?:unknown) {
  const response=await scenario.api.response(method,route,{data,token});
  expect(response.status(),`${method} ${route} status`).toBe(status);
  const body=await response.json();
  expect(body?.error?.code,`${method} ${route} error code`).toBe(code);
}

test('AUTH-001 status 未初始化 → setup 成功 → initialized；重复 setup 返回 410；无/错误 bearer 不能访问受保护 API',{tag:'@case_AUTH-001'},async({scenario})=>{
  await scenario.step('status 未初始化 → setup 成功 → initialized',async()=>{
    const before=await scenario.api.json('GET','/auth/status',undefined,200,null);
    expect(before.initialized).toBe(false);
    const account=await scenario.api.setup('fixture-a');
    expect(account.is_root).toBe(true);
    expect(account.account_id.length).toBeGreaterThan(0);
    expect(account.scopes).toEqual(expect.arrayContaining(ROOT_SCOPES));
    expect(typeof account.secret).toBe('string');
    const after=await scenario.api.json('GET','/auth/status',undefined,200,null);
    expect(after.initialized).toBe(true);
  });
  await scenario.step('重复 setup 返回 410',async()=>{
    await expectError(scenario,'POST','/auth/setup',410,'gone',null,{username:'fixture-second',password:scenario.api.password});
  });
  await scenario.step('无/错误 bearer 不能访问受保护 API',async()=>{
    await expectError(scenario,'GET','/accounts',401,'unauthorized',null);
    await expectError(scenario,'GET','/accounts',401,'unauthorized','malformed-token');
    await expectError(scenario,'GET','/sessions',401,'unauthorized',null);
    const accounts=await scenario.api.json('GET','/accounts');
    expect(accounts).toHaveLength(1);
    expect(accounts[0].is_active).toBe(true);
  });
});

test('AUTH-002 错密码不切账户；A→B 时 A token 423；切回 A 后旧 token 可用',{tag:'@case_AUTH-002'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  const b=await scenario.api.createAccount('fixture-b');
  await scenario.step('错密码不切账户',async()=>{
    const wrong=`synthetic-wrong-${randomBytes(9).toString('hex')}`;
    await scenario.api.remember(wrong);
    await expectError(scenario,'POST',`/accounts/${a.account_id}/unlock`,403,'wrong_password',null,{password:wrong});
    const accounts=await scenario.api.json('GET','/accounts');
    const active=accounts.filter((account:{is_active:boolean})=>account.is_active).map((account:{id:string})=>account.id);
    expect(active).toEqual([b.account_id]);
  });
  await scenario.step('A→B 时 A token 423',async()=>{
    await expectError(scenario,'GET','/accounts',423,'locked',a.secret);
    await expectError(scenario,'GET','/sessions',423,'locked',a.secret);
  });
  await scenario.step('切回 A 后旧 token 可用',async()=>{
    const unlocked=await scenario.api.unlock(a.account_id);
    expect(unlocked.account_id).toBe(a.account_id);
    expect(unlocked.is_root).toBe(true);
    const accounts=await scenario.api.json('GET','/accounts',undefined,200,a.secret);
    const active=accounts.filter((account:{is_active:boolean})=>account.is_active).map((account:{id:string})=>account.id);
    expect(active).toEqual([a.account_id]);
    await expectError(scenario,'GET','/accounts',423,'locked',b.secret);
  });
});

test('AUTH-003 改密后旧密码 unlock 失败、新密码成功；未吊销 token 和历史数据保留',{tag:'@case_AUTH-003'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  const newPassword=`synthetic-password-${randomBytes(18).toString('hex')}`;
  const wrongCurrent=`synthetic-wrong-${randomBytes(18).toString('hex')}`;
  await scenario.api.remember(newPassword);
  await scenario.api.remember(wrongCurrent);
  let thirdParty:{id:string,secret:string,is_root:boolean}|undefined;
  await scenario.step('准备历史数据与未吊销 token',async()=>{
    await scenario.api.json('POST','/dictionary/entries',{terms:['海螺词典']},200);
    thirdParty=await scenario.api.json('POST','/tokens',{name:'fixture-third-party',scopes:['sessions:read']},201);
    expect(thirdParty!.is_root).toBe(false);
    await scenario.api.remember(thirdParty!.secret);
  });
  await scenario.step('改密后旧密码 unlock 失败、新密码成功',async()=>{
    await expectError(scenario,'POST','/auth/password',403,'wrong_password',a.secret,{current_password:wrongCurrent,new_password:newPassword});
    const changed=await scenario.api.response('POST','/auth/password',{data:{current_password:scenario.api.password,new_password:newPassword},token:a.secret});
    expect(changed.status(),'POST /auth/password status').toBe(200);
    await expectError(scenario,'POST',`/accounts/${a.account_id}/unlock`,403,'wrong_password',null,{password:scenario.api.password});
    const unlocked=await scenario.api.unlock(a.account_id,newPassword);
    expect(unlocked.account_id).toBe(a.account_id);
  });
  await scenario.step('未吊销 token 和历史数据保留',async()=>{
    const accounts=await scenario.api.json('GET','/accounts',undefined,200,a.secret);
    expect(accounts.map((account:{id:string})=>account.id)).toEqual([a.account_id]);
    const dictionary=await scenario.api.json('GET','/dictionary',undefined,200,a.secret);
    expect(dictionary.items.map((entry:{term:string})=>entry.term)).toContain('海螺词典');
    const sessions=await scenario.api.json('GET','/sessions',undefined,200,thirdParty!.secret);
    expect(Array.isArray(sessions.items)).toBe(true);
  });
});

test('AUTH-004 删除活跃账户被拒；切到 B 后删除 A，A 不再列出/解锁/访问，相关隔离存储清除（存储证据辅助）',{tag:'@case_AUTH-004'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  const b=await scenario.api.createAccount('fixture-b');
  await scenario.step('删除活跃账户被拒',async()=>{
    await expectError(scenario,'DELETE',`/accounts/${b.account_id}`,409,'conflict',b.secret);
    const accounts=await scenario.api.json('GET','/accounts');
    expect(accounts).toHaveLength(2);
  });
  await scenario.step('切到 B 后删除 A，A 不再列出/解锁/访问',async()=>{
    await scenario.api.json('DELETE',`/accounts/${a.account_id}`,undefined,204,b.secret);
    const accounts=await scenario.api.json('GET','/accounts',undefined,200,b.secret);
    expect(accounts.map((account:{id:string})=>account.id)).toEqual([b.account_id]);
    await expectError(scenario,'POST',`/accounts/${a.account_id}/unlock`,404,'not_found',null,{password:scenario.api.password});
    await expectError(scenario,'GET','/accounts',401,'unauthorized',a.secret);
  });
  await scenario.step('相关隔离存储清除（存储证据辅助）',async()=>{
    const accountDir=path.join(scenario.env.home,'data',a.account_id);
    await expect(fs.access(accountDir).then(()=>true,()=>false)).resolves.toBe(false);
    const keychain=JSON.parse(await fs.readFile(path.join(scenario.env.home,'fixture-keychain.json'),'utf8'));
    expect(Object.keys(keychain).filter(key=>key.endsWith(`:${a.account_id}`))).toEqual([]);
  });
});
