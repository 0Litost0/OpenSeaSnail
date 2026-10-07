import fs from 'node:fs/promises';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {test,expect} from '../fixtures/test.js';
import {project} from '../cli/identity.mjs';
const exec=promisify(execFile);

test('SHERPA-001 正式真实 runtime：中英文各三次评分达标，重启读回一致，真实 sidecar 回收',{tag:'@case_SHERPA-001'},async({scenario})=>{
  const {api,env,configuration}=scenario;
  const manifest=JSON.parse(await fs.readFile(path.join(project,'assets/manifest.json'),'utf8'));
  const observations:any[]=[];let children:any[]=[];
  const evidence=path.join(process.env.API_TEST_RAW_DIR!,'environment-proof',`SHERPA-001-quality-${scenario.retry}.json`);
  async function persist(){await fs.mkdir(path.dirname(evidence),{recursive:true,mode:0o700});await fs.writeFile(evidence,JSON.stringify({rule_version:'quality-v3',runtime:configuration.runtime,observations,sidecars:children},null,2),{mode:0o600})}
  async function sidecars(){
    const files=(await fs.readdir(path.join(env.home,'sidecars'))).filter(name=>name.endsWith('.pid'));
    expect(files).toHaveLength(1);
    const records=await Promise.all(files.map(async name=>JSON.parse(await fs.readFile(path.join(env.home,'sidecars',name),'utf8'))));
    for(const record of records){expect(await env.identity(record.child.pid)).toEqual(record.child);expect(env.manifest.hosts).toContainEqual(record.owner)}
    return records.map(record=>record.child);
  }
  await scenario.step('真实模型与 sidecar 身份正确，无 mock 回退',async()=>{
    expect(configuration.runtime.mode).toBe('sherpa');await api.setup('sherpa-fixture');
    const models=await api.json('GET','/models');
    expect(models).toContainEqual(expect.objectContaining({id:configuration.runtime.model_id,status:'active',runtime:'sherpa_onnx'}));
    children=await sidecars();await persist();
  });
  for(const sample of manifest.samples)for(let repetition=1;repetition<=3;repetition++)await scenario.step(`${sample.id} 正式转写与批准评分第 ${repetition} 次`,async()=>{
    const buffer=await fs.readFile(path.join(project,sample.audio_path));
    const response=await api.response('POST','/sessions',{multipart:{source:'imported',language:sample.language,audio:{name:path.basename(sample.audio_path),mimeType:'audio/wav',buffer}}});
    expect(response.status()).toBe(202);const id=(await response.json()).id;
    const session=await api.terminal(id);const actual=session.transcript.full_text;
    const text=path.join(env.home,`${sample.id}-${repetition}.txt`);await fs.writeFile(text,actual,{mode:0o600});
    let quality:any;let code=0;
    try{const result=await exec('python3',[path.join(project,'scripts/quality.py'),'--sample',sample.id,'--text-file',text],{timeout:5000,maxBuffer:8192});quality=JSON.parse(result.stdout)}catch(error){code=(error as any).code;quality=JSON.parse((error as any).stdout)}
    observations.push({sample_id:sample.id,repetition,id,actual,quality,scorer_exit_code:code});await persist();
    expect(code).toBe(0);expect(quality.gate).toBe('passed');expect(quality.missing_required_key_content).toEqual([]);
  });
  await scenario.step('正常重启先回收旧 sidecar，原 token 与六次转写持久化一致',async()=>{
    const pid=env.child!.pid;await api.restart();expect(env.child!.pid).not.toBe(pid);
    for(const child of children)expect(await env.identity(child.pid)).not.toEqual(child);
    const next=await sidecars();children.push(...next);await persist();
    for(const item of observations){const session=await api.json('GET',`/sessions/${item.id}`);expect(session.status).toBe('completed');expect(session.transcript.full_text).toBe(item.actual)}
  });
  await scenario.step('最终宿主与真实 sidecar 正常停止并确认退出',async()=>{
    await env.stop();for(const child of children)expect(await env.identity(child.pid)).not.toEqual(child);
    for(const stream of (env as any).diagnosticStreams??[])expect(stream.exit).toEqual({code:0,signal:null});
  });
});
