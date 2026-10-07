// PROVIDER-001 optional real-provider evaluation entry (suite: provider-eval).
// Executes only when explicitly selected via `api-test provider-eval
// --provider <id>`; endpoint and credential resolve from environment variables
// at run time and never enter files, titles, or the report (the evidence keeps
// only the endpoint_ref name and a hash for cross-run correlation). The
// evaluation record is an observation of a non-deterministic remote model,
// not a gate.
import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {test,expect,type EvalProvider} from '../fixtures/test.js';

const INPUT_TRANSCRIPT='海螺支持语音转写和词典功能';
const KEY_TERMS=['海螺','词典'];

test('PROVIDER-001 显式配置真实 provider/model：记录输入、配置、输出与质量结果；无凭据时说明未执行，不计通过',{tag:'@case_PROVIDER-001'},async({scenario})=>{
  const configuration=scenario.configuration;
  if(configuration.provider?.mode!=='remote')test.skip(true,'requires explicit api-test provider-eval selection');
  const {provider_id,model,endpoint_ref}=configuration.provider;
  const evalProvider=scenario.provider as EvalProvider;
  if(!evalProvider.credentialPresent)test.skip(true,`credential environment variable for provider ${provider_id} not provided`);
  const endpoint=evalProvider.endpoint;
  if(!endpoint)test.skip(true,`endpoint environment variable ${endpoint_ref} not provided`);
  const parsed=new URL(endpoint);
  if(parsed.username||parsed.password)throw new Error('provider-eval endpoint must not embed userinfo; credentials resolve via the credential environment variable');
  let id='';
  await scenario.step('显式配置真实 provider 并执行 realtime clean',async()=>{
    const credential=process.env[configuration.credential_refs[0].reference]!;
    await scenario.api.setup('fixture-eval');
    await scenario.api.configureProvider(endpoint,true,{name:`eval-${provider_id}`,providerType:evalProvider.providerType,model,credential});
    id=await scenario.api.upload('realtime','zh');
    await scenario.api.terminal(id,'completed');
  });
  await scenario.step('记录输入、配置、输出与质量结果',async()=>{
    const workspace=await scenario.api.json('GET',`/sessions/${id}/workspace-detail`);
    const cleanup=await scenario.api.json('GET',`/sessions/${id}/cleanup-detail`);
    // Evidence is written before the assertions below, so a failed or
    // low-quality evaluation still leaves its observation record.
    const evidence={
      kind:'provider-eval',case_id:'PROVIDER-001',assertion_version:configuration.assertion_version,
      provider_id,provider_type:evalProvider.providerType,model,
      endpoint_ref,endpoint_sha256:createHash('sha256').update(endpoint).digest('hex'),
      input:{audio_asset:'pipeline-v1.wav',transcript:INPUT_TRANSCRIPT},
      output:{cleanup_status:workspace.cleanup_status,cleanup_error_code:workspace.cleanup_error_code??null,cleaned_text:cleanup.cleaned_text??null,final_text:workspace.final_text,cleanup_elapsed_ms:cleanup.cleanup_elapsed_ms??null},
      quality:{output_deterministic:false,output_non_empty:typeof cleanup.cleaned_text==='string'&&cleanup.cleaned_text.length>0,key_terms_preserved:Object.fromEntries(KEY_TERMS.map(term=>[term,String(cleanup.cleaned_text??'').includes(term)]))},
      note:'remote model output is non-deterministic; this record is an observation, not a pass gate',
      executed_at:new Date().toISOString(),
    };
    const raw=process.env.API_TEST_RAW_DIR!;
    const directory=path.join(raw,'environment-proof');
    await fs.mkdir(directory,{recursive:true,mode:0o700});
    await fs.writeFile(path.join(directory,`PROVIDER-001-eval-${scenario.retry}.json`),JSON.stringify(evidence,null,2),{mode:0o600});
    // An executed evaluation requires real provider output; a failed or
    // unreachable provider is surfaced as a case failure with evidence.
    expect(workspace.cleanup_status).toBe('succeeded');
    expect(cleanup.original_text).toBe(INPUT_TRANSCRIPT);
    expect(typeof cleanup.cleaned_text).toBe('string');
    expect(cleanup.cleaned_text.length).toBeGreaterThan(0);
    expect(workspace.text_source).toBe('cleanup');
    expect(workspace.final_text).toBe(cleanup.cleaned_text);
  });
});
