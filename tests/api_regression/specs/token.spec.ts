// TOKEN-001 / TOKEN-002 business cases (suite: auth, level: deterministic).
// Scope grant/deny and revocation semantics per contracts/assertions.v1.json
// and the fixed matrix in docs/design.md; secrets stay out of titles
// and asserted values.
import fs from 'node:fs/promises';
import {test,expect,type Scenario} from '../fixtures/test.js';

async function expectError(scenario:Scenario,method:string,route:string,status:number,code:string,token?:string|null,data?:unknown) {
  const response=await scenario.api.response(method,route,{data,token});
  expect(response.status(),`${method} ${route} status`).toBe(status);
  const body=await response.json();
  expect(body?.error?.code,`${method} ${route} error code`).toBe(code);
}

test('TOKEN-001 受限 token 可读允许资源，写/manage 被拒；不可授 scope 被拒；token 列表不返回 secret/hash',{tag:'@case_TOKEN-001'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  let limited:{id:string,secret:string,scopes:string[],is_root:boolean}|undefined;
  await scenario.step('受限 token 可读允许资源，写/manage 被拒',async()=>{
    limited=await scenario.api.json('POST','/tokens',{name:'fixture-limited',scopes:['sessions:read']},201);
    expect(limited!.is_root).toBe(false);
    expect(limited!.scopes).toEqual(['sessions:read']);
    await scenario.api.remember(limited!.secret);
    const sessions=await scenario.api.json('GET','/sessions',undefined,200,limited!.secret);
    expect(Array.isArray(sessions.items)).toBe(true);
    const audio=await fs.readFile(new URL('../assets/audio/pipeline-v1.wav',import.meta.url));
    const denied=await scenario.api.response('POST','/sessions',{token:limited!.secret,multipart:{source:'imported',language:'zh',audio:{name:'pipeline-v1.wav',mimeType:'audio/wav',buffer:audio}}});
    expect(denied.status(),'POST /sessions status').toBe(403);
    expect((await denied.json())?.error?.code).toBe('insufficient_scope');
    await expectError(scenario,'GET','/tokens',403,'insufficient_scope',limited!.secret);
    await expectError(scenario,'GET','/accounts',403,'insufficient_scope',limited!.secret);
  });
  await scenario.step('不可授 scope 被拒',async()=>{
    for(const scopes of [['sessions:write'],['tokens:manage'],['sessions:read','tokens:manage'],['is_root']]) {
      await expectError(scenario,'POST','/tokens',403,'scope_not_grantable',undefined,{name:'fixture-ungrantable',scopes});
    }
  });
  await scenario.step('token 列表不返回 secret/hash',async()=>{
    const tokens=await scenario.api.json('GET','/tokens');
    expect(tokens.length).toBeGreaterThanOrEqual(2);
    for(const item of tokens) {
      expect(Object.keys(item).filter(key=>key==='secret'||key.toLowerCase().includes('hash'))).toEqual([]);
    }
    const row=tokens.find((item:{id:string})=>item.id===limited!.id);
    expect(row?.scopes).toEqual(['sessions:read']);
    expect(row?.is_root).toBe(false);
  });
});

test('TOKEN-002 吊销后所属活跃账户 API 返回 401；新 token 可用，拒绝不误作用于其他 token',{tag:'@case_TOKEN-002'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  const first=await scenario.api.json('POST','/tokens',{name:'fixture-first',scopes:['sessions:read']},201);
  const second=await scenario.api.json('POST','/tokens',{name:'fixture-second',scopes:['sessions:read']},201);
  await scenario.api.remember(first.secret);
  await scenario.api.remember(second.secret);
  await scenario.step('吊销后所属活跃账户 API 返回 401',async()=>{
    await scenario.api.json('GET','/sessions',undefined,200,first.secret);
    await scenario.api.json('DELETE',`/tokens/${first.id}`,undefined,204,a.secret);
    await expectError(scenario,'GET','/sessions',401,'unauthorized',first.secret);
  });
  await scenario.step('新 token 可用，拒绝不误作用于其他 token',async()=>{
    await scenario.api.json('GET','/sessions',undefined,200,second.secret);
    const accounts=await scenario.api.json('GET','/accounts',undefined,200,a.secret);
    expect(accounts.map((account:{id:string})=>account.id)).toEqual([a.account_id]);
    const third=await scenario.api.json('POST','/tokens',{name:'fixture-third',scopes:['sessions:read']},201);
    await scenario.api.remember(third.secret);
    await scenario.api.json('GET','/sessions',undefined,200,third.secret);
    await expectError(scenario,'DELETE',`/tokens/${first.id}`,404,'not_found',a.secret);
  });
});
