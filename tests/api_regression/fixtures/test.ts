import {test as base,expect} from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';
import {randomUUID,createHash} from 'node:crypto';
import {AttemptEnvironment} from './environment.mjs';
import {localProvider} from './provider.mjs';
import {verifySherpa} from './sherpa.mjs';
import {Api} from './api.js';
import {assertionDetails,collectDiagnostics} from './diagnostics.mjs';
const budget=JSON.parse(await fs.readFile(new URL('../assets/budgets.v1.json',import.meta.url),'utf8')).milliseconds;
export {expect};
async function evalEntry(providerId:string) {
  const registry=JSON.parse(await fs.readFile(new URL('../assets/provider-eval.v1.json',import.meta.url),'utf8'));
  let providers={...registry.providers};
  try{const local=JSON.parse(await fs.readFile(new URL('../assets/provider-eval.local.json',import.meta.url),'utf8'));providers={...providers,...local.providers}}catch(error){if((error as NodeJS.ErrnoException).code!=='ENOENT')throw error}
  return providers[providerId];
}
async function withinDeadline(action:()=>Promise<unknown>|undefined,deadline:number){
  let timer:ReturnType<typeof setTimeout>|undefined;
  try{await Promise.race([Promise.resolve().then(action),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('teardown deadline exceeded')),Math.max(0,deadline-Date.now()))})])}
  finally{if(timer)clearTimeout(timer)}
}
export type Step={step_id:string,title:string,status:'passed'|'failed',duration_ms:number};
// Local providers serve a real fixture endpoint; for remote evaluation the
// endpoint/credential resolve from environment variables (empty string when
// unset) and the case skips itself with an explicit reason.
export type EvalProvider={endpoint:string,credentialPresent:boolean,providerType:string|undefined,requests:any[],close():Promise<void>};
export type Scenario={api:Api,env:AttemptEnvironment,provider:Awaited<ReturnType<typeof localProvider>>|EvalProvider,configuration:any,retry:number,steps:Step[],step<T>(title:string,action:()=>Promise<T>):Promise<T>};
export const test=base.extend<{scenario:Scenario}>({
  scenario:[async({},use,testInfo)=>{
    const id=testInfo.title.match(/^([A-Z]+-\d{3})\b/)?.[1];if(!id)throw new Error('unknown case identity');
    const raw=process.env.API_TEST_RAW_DIR;if(!raw)throw new Error('use api-test CLI to run registered business cases');
    const runId=process.env.API_TEST_RUN_ID??randomUUID();
    const configuration=JSON.parse(await fs.readFile(path.join(raw,'configurations.json'),'utf8'))[id];
    const env=new AttemptEnvironment({host:process.env.API_TEST_HOST||undefined,root:path.resolve('artifacts/environments'),runId,caseId:id,attempt:testInfo.retry,runtime:configuration.runtime.mode,scenario:configuration.runtime.scenario??'asr-success-v1',keychain:configuration.keychain.mode==='persistent-fixture'?'persistent':configuration.keychain.mode==='read-error-master-dek'?'read-error':configuration.keychain.mode});
    const steps:Step[]=[];let api:Api|undefined;let provider:Awaited<ReturnType<typeof localProvider>>|EvalProvider|undefined;
    const began=Date.now();let execution='environment_error';let cleanup:any={status:'unknown'};
    const errors:any[]=[];let caught:any;let failedAssertion:any;
    try{
      await env.prepare();if(configuration.runtime.mode==='sherpa')await verifySherpa(configuration);await env.start();await env.waitModelReady();
      api=await Api.create(env,path.join(raw,'secrets.ndjson'));
      if(configuration.provider.mode==='remote'){
        // Remote evaluation: endpoint/credential resolve from the environment at
        // run time; the credential is only registered for the publication scan.
        const credentialRef=configuration.credential_refs?.[0]?.reference;
        const credential=credentialRef?process.env[credentialRef]:undefined;
        if(credential)await api.remember(credential);
        const entry=await evalEntry(configuration.provider.provider_id);
        const endpoint=process.env[configuration.provider.endpoint_ref];if(endpoint)await api.remember(endpoint);
        provider={endpoint:process.env[configuration.provider.endpoint_ref]??'',credentialPresent:Boolean(credential),providerType:entry?.provider_type,requests:[],async close(){}};
      }else{
        provider=await localProvider(configuration.provider.mode,api.providerSecret);
      }
      const scenario:Scenario={env,api,provider,configuration,retry:testInfo.retry,steps,async step(title,action){const start=Date.now();const item:Step={step_id:`step-${steps.length}`,title,status:'passed',duration_ms:0};steps.push(item);try{return await base.step(title,action)}catch(error){item.status='failed';failedAssertion=assertionDetails(error);throw error}finally{item.duration_ms=Date.now()-start}}};
      execution='passed';await use(scenario);
      if(testInfo.status==='skipped')execution='not_run';else if(testInfo.status!==testInfo.expectedStatus)execution='failed';
    }catch(error){caught=error;if(execution==='environment_error')errors.push({category:'environment',code:'environment_setup_failed',message:'attempt environment unavailable'});else execution='failed';throw error}
    finally{
      const deadline=Date.now()+budget.teardown_total;
      try{await withinDeadline(()=>api?.close(),deadline)}catch{errors.push({category:'teardown',code:'request_context_close_failed',message:'request disposal failed'})}
      let diagnosticEvidence:any;
      const capture=async()=>{diagnosticEvidence=await collectDiagnostics({env,raw,caseId:id,retry:testInfo.retry,errors:[...testInfo.errors,...(caught?[caught]:[])]})};
      cleanup=await env.teardown({deadline,beforeRemove:capture});
      if(!diagnosticEvidence){try{await capture()}catch{errors.push({category:'teardown',code:'diagnostic_capture_failed',message:'diagnostic evidence unavailable'})}}
      try{await withinDeadline(()=>provider?.close(),deadline)}catch{errors.push({category:'teardown',code:'provider_close_failed',message:'provider disposal failed'})}
      if(execution==='not_run'){const note=(testInfo.annotations??[]).find(annotation=>annotation.type==='skip')?.description;errors.push({category:'configuration',code:'case_not_run',message:note??'case skipped before business execution'})}
      if(execution==='failed')errors.unshift({category:testInfo.status==='timedOut'?'timeout':'business',code:testInfo.status==='timedOut'?'business_timeout':'business_assertion_failed',message:'business validation failed'});
      if(cleanup.status!=='passed')errors.push({category:'teardown',code:'environment_cleanup_failed',message:'attempt resources not verified clean'});
      const proof={case_id:id,retry_index:testInfo.retry,run_id:runId,home_identity_sha256:createHash('sha256').update(env.home).digest('hex'),host_identities:env.manifest.hosts,port:env.port??null,runtime:configuration.runtime,provider:configuration.provider,keychain:configuration.keychain,cleanup_status:cleanup.status};
      await fs.mkdir(path.join(raw,'environment-proof'),{recursive:true,mode:0o700});
      await fs.writeFile(path.join(raw,'environment-proof',`${id}-${testInfo.retry}.json`),JSON.stringify(proof,null,2),{mode:0o600});
      const clean=diagnosticEvidence?.clean??((value:string)=>value);
      const errorEvidence=diagnosticEvidence?[diagnosticEvidence.reference]:[];
      const result={attempt_id:`${id}-attempt-${testInfo.retry}`,retry_index:testInfo.retry,execution_status:execution,teardown_status:cleanup.status==='passed'&&errors.every(error=>error.category!=='teardown')?'passed':'failed',duration_ms:Date.now()-began,config:configuration,steps,errors:errors.map(error=>({step_id:steps.find(step=>step.status==='failed')?.step_id??'environment',expected:failedAssertion?.expected==null?null:clean(failedAssertion.expected),actual:failedAssertion?.actual==null?null:clean(failedAssertion.actual),evidence_refs:errorEvidence,...error,message:clean(error.category==='business'&&failedAssertion?failedAssertion.message:error.message)})),evidence_refs:[`environment-proof/${id}-${testInfo.retry}.json`,...errorEvidence]};
      await fs.mkdir(path.join(raw,'attempts'),{recursive:true,mode:0o700});await fs.writeFile(path.join(raw,'attempts',`${id}-${testInfo.retry}.json`),JSON.stringify(result,null,2),{mode:0o600});
      if(result.teardown_status==='failed')throw new Error('attempt teardown failed');
    }
  },{timeout:budget.startup+budget.model_ready+budget.api_request+budget.teardown_total}],
});
