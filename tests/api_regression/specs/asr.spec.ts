// ASR-001 / ASR-002 business cases (suite: sessions, level: deterministic).
// The deterministic runtime only replaces model inference; upload,
// normalization, task orchestration and persistence run the production path.
import fs from 'node:fs/promises';
import {test,expect} from '../fixtures/test.js';

const FIXED_TRANSCRIPT='海螺支持语音转写和词典功能';

test('ASR-001 multipart 提交、任务到 completed、固定转写与音频字节读取一致；列表/详情可查',{tag:'@case_ASR-001'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  let id='';
  await scenario.step('multipart 提交、任务到 completed、固定转写与音频字节读取一致',async()=>{
    id=await scenario.api.upload('imported','zh');
    const view=await scenario.api.terminal(id,'completed');
    expect(view.transcript?.full_text).toBe(FIXED_TRANSCRIPT);
    expect(view.source).toBe('imported');
    expect(view.failure_reason).toBeNull();
    const bytes=await scenario.api.binary(`/sessions/${id}/audio`);
    expect(bytes).toEqual(await fs.readFile(new URL('../assets/audio/pipeline-v1.wav',import.meta.url)));
  });
  await scenario.step('列表/详情可查',async()=>{
    const list=await scenario.api.json('GET','/sessions');
    const summary=list.items.find((item:{id:string})=>item.id===id);
    expect(summary?.status).toBe('completed');
    const detail=await scenario.api.json('GET',`/sessions/${id}`);
    expect(detail.transcript?.full_text).toBe(FIXED_TRANSCRIPT);
  });
});

test('ASR-002 任务进入失败终态，错误可观察；经现有 retry API 恢复后结果正确，删除后列表/详情不可见',{tag:'@case_ASR-002'},async({scenario})=>{
  await scenario.api.setup('fixture-a');
  let id='';
  await scenario.step('任务进入失败终态，错误可观察',async()=>{
    id=await scenario.api.upload('imported','zh');
    const view=await scenario.api.terminal(id,'failed');
    expect(typeof view.failure_reason).toBe('string');
    expect(view.failure_reason.length).toBeGreaterThan(0);
    expect(view.transcript).toBeNull();
  });
  await scenario.step('经现有 retry API 恢复后结果正确，删除后列表/详情不可见',async()=>{
    const retried=await scenario.api.json('POST',`/sessions/${id}/retry`,undefined,202);
    expect(retried.status).toBe('transcribing');
    const view=await scenario.api.terminal(id,'completed');
    expect(view.transcript?.full_text).toBe(FIXED_TRANSCRIPT);
    await scenario.api.json('DELETE',`/sessions/${id}`,undefined,204);
    const missing=await scenario.api.response('GET',`/sessions/${id}`);
    expect(missing.status(),'GET /sessions/{id} status').toBe(404);
    expect((await missing.json())?.error?.code).toBe('not_found');
    const list=await scenario.api.json('GET','/sessions');
    expect(list.items.map((item:{id:string})=>item.id)).not.toContain(id);
  });
});
