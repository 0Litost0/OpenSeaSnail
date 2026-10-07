// CLEAN-001..005 business cases (suite: clean, level: deterministic).
// The local provider fixture only observes and stubs the network boundary;
// orchestration, fallback and persistence are production behavior. Expected
// values mirror assets/scenarios/provider.json (versioned assertion data).
import {test,expect,type Scenario} from '../fixtures/test.js';

const RAW_TRANSCRIPT='海螺支持语音转写和词典功能';
const CLEANED_TRANSCRIPT='海螺支持语音转写和词典功能。';

test('CLEAN-001 realtime + enabled + 成功 provider：原文、cleaned/final text 和 cleanup 状态正确；词典参与；正常重启后可读取相同结果',{tag:'@case_CLEAN-001'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  await scenario.api.json('POST','/dictionary/entries',{terms:['SeaSnail']},200);
  await scenario.api.configureProvider(scenario.provider.endpoint);
  let id='';
  await scenario.step('原文、cleaned/final text 和 cleanup 状态正确',async()=>{
    id=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(id,'completed');
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.cleanup_status).toBe('succeeded');
    expect(workspace.text_source).toBe('cleanup');
    expect(workspace.final_text).toBe(CLEANED_TRANSCRIPT);
    const cleanup=await scenario.api.json('GET',`/sessions/${id}/cleanup-detail`);
    expect(cleanup.original_text).toBe(RAW_TRANSCRIPT);
    expect(cleanup.cleaned_text).toBe(CLEANED_TRANSCRIPT);
  });
  await scenario.step('词典参与',async()=>{
    expect(scenario.provider.requests).toHaveLength(1);
    expect(scenario.provider.requests[0].authenticated).toBe(true);
    expect(scenario.provider.requests[0].input.transcript).toBe(RAW_TRANSCRIPT);
    expect(scenario.provider.requests[0].input.dictionary_terms).toContain('SeaSnail');
  });
  await scenario.step('正常重启后可读取相同结果',async()=>{
    await scenario.api.restart();
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.cleanup_status).toBe('succeeded');
    expect(workspace.final_text).toBe(CLEANED_TRANSCRIPT);
  });
});

test('CLEAN-005 分别关闭 enabled、使用 imported：两种条件均不调用 provider，final text 为原文，详情分别体现关闭/未请求语义',{tag:'@case_CLEAN-005'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  const providerConfig=await scenario.api.configureProvider(scenario.provider.endpoint,false);
  await scenario.step('关闭 enabled 时不调用 provider，final text 为原文且体现关闭语义',async()=>{
    const settings=await scenario.api.json('GET','/cleanup/settings');
    expect(settings.enabled).toBe(false);
    const id=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(id,'completed');
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.cleanup_status).toBe('disabled');
    expect(workspace.text_source).toBe('raw');
    expect(workspace.final_text).toBe(RAW_TRANSCRIPT);
    expect(scenario.provider.requests).toHaveLength(0);
  });
  await scenario.step('source=imported 时不调用 provider，final text 为原文且体现未请求语义',async()=>{
    await scenario.api.json('PUT','/cleanup/settings',{enabled:true,selected_provider_config_id:providerConfig.id,custom_prompt:null},200);
    const settings=await scenario.api.json('GET','/cleanup/settings');
    expect(settings.enabled).toBe(true);
    const id=await scenario.api.upload('imported','zh');
    await scenario.api.terminal(id,'completed');
    const view=await scenario.api.json('GET',`/sessions/${id}`);
    expect(view.source).toBe('imported');
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.cleanup_status).toBe('not_requested');
    expect(workspace.text_source).toBe('raw');
    expect(workspace.final_text).toBe(RAW_TRANSCRIPT);
    expect(scenario.provider.requests).toHaveLength(0);
  });
});

async function expectFallback(scenario:Scenario,id:string,errorCode:string) {
  const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
  expect(workspace.cleanup_status).toBe('failed');
  expect(workspace.cleanup_error_code).toBe(errorCode);
  expect(workspace.text_source).toBe('raw');
  expect(workspace.final_text).toBe(RAW_TRANSCRIPT);
}

test('CLEAN-002 realtime + HTTP 503：上游失败可观察、最终文本回退为原文；失败信息和结果持久化',{tag:'@case_CLEAN-002'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  await scenario.api.configureProvider(scenario.provider.endpoint);
  let id='';
  await scenario.step('上游失败可观察、最终文本回退为原文',async()=>{
    id=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(id,'completed');
    await expectFallback(scenario,id,'cleanup_http_server');
    expect(scenario.provider.requests).toHaveLength(1);
    expect(scenario.provider.requests[0].authenticated).toBe(true);
  });
  await scenario.step('失败信息和结果持久化',async()=>{
    await scenario.api.restart();
    await expectFallback(scenario,id,'cleanup_http_server');
  });
});

test('CLEAN-003 realtime + 非法输出：HTTP 成功但非法内容不能成为最终文本；按契约失败/回退并保留原文',{tag:'@case_CLEAN-003'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  await scenario.api.configureProvider(scenario.provider.endpoint);
  let id='';
  await scenario.step('HTTP 成功但非法内容不能成为最终文本',async()=>{
    id=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(id,'completed');
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.cleanup_status).toBe('failed');
    expect(workspace.cleanup_error_code).toBe('cleanup_response_invalid_json');
    expect(workspace.final_text).not.toBe('not JSON');
    expect(scenario.provider.requests).toHaveLength(1);
  });
  await scenario.step('按契约失败/回退并保留原文',async()=>{
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    expect(workspace.text_source).toBe('raw');
    expect(workspace.final_text).toBe(RAW_TRANSCRIPT);
    const cleanup=await scenario.api.json('GET',`/sessions/${id}/cleanup-detail`);
    expect(cleanup.original_text).toBe(RAW_TRANSCRIPT);
  });
});

test('CLEAN-004 realtime + 超时 provider：真实超时路径有界进入约定回退，不无限挂起；错误 code 和持久化正确',{tag:'@case_CLEAN-004'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  await scenario.api.configureProvider(scenario.provider.endpoint);
  let id='';
  await scenario.step('真实超时路径有界进入约定回退，不无限挂起',async()=>{
    id=await scenario.api.upload('realtime','zh');
    // The bounded poll itself is the unbounded-hang guard: a stuck cleanup
    // would breach the business deadline and fail this step.
    await scenario.api.terminal(id,'completed');
    await expectFallback(scenario,id,'cleanup_timeout');
    expect(scenario.provider.requests).toHaveLength(1);
  });
  await scenario.step('错误 code 和持久化正确',async()=>{
    await scenario.api.restart();
    await expectFallback(scenario,id,'cleanup_timeout');
  });
});
