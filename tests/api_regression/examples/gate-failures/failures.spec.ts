import {test,expect} from '../../fixtures/test.js';
test('AUTH-005 受控业务超时必须失败',{tag:'@case_AUTH-005'},async({scenario})=>{
  test.setTimeout(150);
  await scenario.step('已准备环境后的业务等待超时',async()=>{await new Promise(resolve=>setTimeout(resolve,2000))});
});
test('AUTH-006 首败重试成功必须保留首次失败且 gate 非零',{tag:'@case_AUTH-006'},async({scenario})=>{
  await scenario.step('首次受控断言失败，第二新环境真实 setup 成功',async()=>{
    expect(scenario.retry).toBeGreaterThan(0);await scenario.api.setup();expect(await scenario.api.json('GET','/auth/status',undefined,200,null)).toEqual({initialized:true});
  });
});
test('AUTH-007 原生断言 canary 必须阻止发布',{tag:'@case_AUTH-007'},async({scenario})=>{
  const secret=process.env.REPORT_FAILURE_SECRET!;await scenario.api.remember(secret);
  await scenario.step('原生框架错误携带受控敏感值',async()=>{expect(secret).toBe('safe-public-value')});
});
