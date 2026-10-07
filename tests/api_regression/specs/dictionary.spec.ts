// DICT-001 / DICT-002 business cases (suite: dictionary, level: deterministic).
// Cross-account delete is idempotent (204 no-op on the caller's repository), so
// isolation is proven by the other account's data surviving, not by a status
// code alone; dictionary participation is observed at the provider boundary.
import {randomUUID} from 'node:crypto';
import {test,expect,type Scenario} from '../fixtures/test.js';

async function expectError(scenario:Scenario,method:string,route:string,status:number,code:string,token?:string|null,data?:unknown) {
  const response=await scenario.api.response(method,route,{data,token});
  expect(response.status(),`${method} ${route} status`).toBe(status);
  const body=await response.json();
  expect(body?.error?.code,`${method} ${route} error code`).toBe(code);
}

async function terms(scenario:Scenario,token?:string|null):Promise<string[]> {
  const page=await scenario.api.json('GET','/dictionary',undefined,200,token);
  return page.items.map((entry:{term:string})=>entry.term);
}

test('DICT-001 增改删查、重复处理和无效输入符合契约；服务重启后原 token 读取一致',{tag:'@case_DICT-001'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  let firstId='';let secondId='';
  await scenario.step('增改删查、重复处理和无效输入符合契约',async()=>{
    const added=await scenario.api.json('POST','/dictionary/entries',{terms:['海螺','SeaSnail']},200);
    expect(added.added).toHaveLength(2);
    expect(added.skipped_count).toBe(0);
    firstId=added.added[0].id;secondId=added.added[1].id;
    expect(await terms(scenario)).toEqual(expect.arrayContaining(['海螺','SeaSnail']));
    const duplicate=await scenario.api.json('POST','/dictionary/entries',{terms:['海螺']},200);
    expect(duplicate.added).toHaveLength(0);
    expect(duplicate.skipped_count).toBe(1);
    await expectError(scenario,'POST','/dictionary/entries',422,'dictionary_invalid_term',undefined,{terms:['']});
    const edited=await scenario.api.json('PUT',`/dictionary/entries/${firstId}`,{term:'海螺词典'},200);
    expect(edited.term).toBe('海螺词典');
    await expectError(scenario,'PUT',`/dictionary/entries/${firstId}`,409,'dictionary_conflict',undefined,{term:'SeaSnail'});
    await expectError(scenario,'PUT',`/dictionary/entries/${randomUUID()}`,404,'not_found',undefined,{term:'不存在词条'});
    await expectError(scenario,'PUT','/dictionary/entries/not-a-uuid',400,'bad_request',undefined,{term:'任意'});
    await scenario.api.json('DELETE',`/dictionary/entries/${secondId}`,undefined,204);
    const remaining=await terms(scenario);
    expect(remaining).toContain('海螺词典');
    expect(remaining).not.toContain('SeaSnail');
  });
  await scenario.step('服务重启后原 token 读取一致',async()=>{
    await scenario.api.restart();
    expect(await terms(scenario,a.secret)).toEqual(['海螺词典']);
  });
});

test('DICT-002 列表/ID 操作不泄露或修改另一账户词条；realtime clean 请求包含当前账户词条且不混入另一账户词条',{tag:'@case_DICT-002'},async({scenario})=>{
  const a=await scenario.api.setup('fixture-a');
  const addedA=await scenario.api.json('POST','/dictionary/entries',{terms:['海螺术语']},200);
  const aEntryId=addedA.added[0].id;
  const b=await scenario.api.createAccount('fixture-b');
  await scenario.api.json('POST','/dictionary/entries',{terms:['隔离术语']},200);
  await scenario.step('列表/ID 操作不泄露或修改另一账户词条',async()=>{
    expect(await terms(scenario,b.secret)).toEqual(['隔离术语']);
    await expectError(scenario,'PUT',`/dictionary/entries/${aEntryId}`,404,'not_found',b.secret,{term:'越权改写'});
    // Cross-account delete is a 204 no-op on the caller's own repository;
    // proof of isolation is A's entry surviving below.
    await scenario.api.json('DELETE',`/dictionary/entries/${aEntryId}`,undefined,204,b.secret);
    await scenario.api.unlock(a.account_id);
    expect(await terms(scenario)).toEqual(['海螺术语']);
  });
  await scenario.step('realtime clean 请求包含当前账户词条且不混入另一账户词条',async()=>{
    await scenario.api.configureProvider(scenario.provider.endpoint);
    const aSession=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(aSession,'completed');
    await scenario.api.unlock(b.account_id);
    await scenario.api.configureProvider(scenario.provider.endpoint);
    const bSession=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(bSession,'completed');
    expect(scenario.provider.requests).toHaveLength(2);
    expect(scenario.provider.requests[0].input.dictionary_terms).toContain('海螺术语');
    expect(scenario.provider.requests[0].input.dictionary_terms).not.toContain('隔离术语');
    expect(scenario.provider.requests[1].input.dictionary_terms).toContain('隔离术语');
    expect(scenario.provider.requests[1].input.dictionary_terms).not.toContain('海螺术语');
    // 固定矩阵：B token 请求已知 A session 标识同样不可见。
    await expectError(scenario,'GET',`/sessions/${aSession}`,404,'not_found',b.secret);
    const bSessions=await scenario.api.json('GET','/sessions',undefined,200,b.secret);
    expect(bSessions.items.map((item:{id:string})=>item.id)).not.toContain(aSession);
  });
});
