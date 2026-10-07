// Entry/result contract verification for provider-eval against a local
// OpenAI-compatible service. This verifies the command, configuration, and
// result plumbing; it is not real provider quality evidence.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {randomBytes} from 'node:crypto';
import {project} from '../cli/identity.mjs';
import {localProvider} from '../fixtures/provider.mjs';
const exec=promisify(execFile);
async function cli(args,env,strip=[]){
  const merged={...process.env};for(const name of strip)delete merged[name];
  try{const result=await exec(process.execPath,[path.join(project,'cli/api-test.mjs'),...args],{cwd:project,env:{...merged,...env},timeout:300000});return {code:0,data:JSON.parse(result.stdout)}}
  catch(error){return {code:error.code,data:JSON.parse(error.stdout||error.stderr)}}
}
const secret=`synthetic-eval-${randomBytes(18).toString('hex')}`;
const provider=await localProvider('success',secret);
try{
  const env={SEASNAIL_API_TEST_EVAL_ENDPOINT:provider.endpoint,SEASNAIL_API_TEST_EVAL_CREDENTIAL:secret};
  // Explicit selection with endpoint+credential: the evaluation executes.
  const executed=await cli(['provider-eval','--provider','fixture-local-openai'],env);
  assert.equal(executed.code,0,JSON.stringify(executed.data));
  assert.equal(executed.data.gate,'passed');
  assert.equal(provider.requests.length,1);
  assert.equal(provider.requests[0].authenticated,true);
  const report=path.join(project,path.dirname(executed.data.result));
  const evidence=JSON.parse(await fs.readFile(path.join(report,'environment-proof/PROVIDER-001-eval-0.json'),'utf8'));
  assert.equal(evidence.kind,'provider-eval');
  assert.equal(evidence.provider_id,'fixture-local-openai');
  assert.equal(evidence.assertion_version,'PROVIDER-001-v1');
  assert.equal(evidence.endpoint_ref,'SEASNAIL_API_TEST_EVAL_ENDPOINT');
  assert.match(evidence.endpoint_sha256,/^[0-9a-f]{64}$/);
  assert.equal(evidence.output.cleanup_status,'succeeded');
  assert.equal(evidence.output.cleaned_text,'海螺支持语音转写和词典功能。');
  assert.equal(JSON.stringify(evidence).includes(secret),false,'credential must not enter published evidence');
  assert.equal(JSON.stringify(evidence).includes(provider.endpoint),false,'resolved endpoint value must not enter published evidence');
  const result=JSON.parse(await fs.readFile(path.join(report,'result.json'),'utf8'));
  assert.equal(result.cases[0].attempts[0].execution_status,'passed');
  assert.equal(result.selection.mode,'provider-eval');
  // Missing credential: explicit not_run, exit 2, and no remote call happens.
  const before=provider.requests.length;
  const withoutCredential=await cli(['provider-eval','--provider','fixture-local-openai'],{SEASNAIL_API_TEST_EVAL_ENDPOINT:provider.endpoint},['SEASNAIL_API_TEST_EVAL_CREDENTIAL']);
  assert.equal(withoutCredential.code,2);
  const missing=JSON.parse(await fs.readFile(path.join(project,path.dirname(withoutCredential.data.result),'result.json'),'utf8'));
  assert.equal(missing.cases[0].attempts[0].execution_status,'not_run');
  assert.equal(provider.requests.length,before,'no remote call without credential');
  // Missing endpoint: explicit not_run, exit 2, and no remote call happens.
  const withoutEndpoint=await cli(['provider-eval','--provider','fixture-local-openai'],{SEASNAIL_API_TEST_EVAL_CREDENTIAL:secret},['SEASNAIL_API_TEST_EVAL_ENDPOINT']);
  assert.equal(withoutEndpoint.code,2);
  const noEndpoint=JSON.parse(await fs.readFile(path.join(project,path.dirname(withoutEndpoint.data.result),'result.json'),'utf8'));
  assert.equal(noEndpoint.cases[0].attempts[0].execution_status,'not_run');
  assert.equal(provider.requests.length,before,'no remote call without endpoint');
  // Without explicit provider-eval selection the remote path never runs.
  const plain=await cli(['run','PROVIDER-001'],env);
  assert.equal(plain.code,2);
  const plainResult=JSON.parse(await fs.readFile(path.join(project,path.dirname(plain.data.result),'result.json'),'utf8'));
  assert.equal(plainResult.cases[0].attempts[0].execution_status,'not_run');
  assert.equal(provider.requests.length,before,'no remote call without explicit selection');
  console.log(JSON.stringify({checks:'provider-eval executed/missing-credential/missing-endpoint/explicit-selection',passed:true,eval_run:executed.data.run_id,missing_credential_run:withoutCredential.data.run_id,missing_endpoint_run:withoutEndpoint.data.run_id,plain_run:plain.data.run_id}));
}finally{await provider.close()}
