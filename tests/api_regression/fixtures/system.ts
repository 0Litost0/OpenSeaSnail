// Controllers retain real child reports and exits; only the controller's checks pass.
import {test as base,expect} from '@playwright/test';
import fs from 'node:fs/promises';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {ConfigurationError} from '../cli/replay.mjs';
import {AttemptEnvironment,cleanupStale} from './environment.mjs';
import {project} from '../cli/identity.mjs';
import {admitInterruption,sameIdentity,finalizeChild} from './child-process.mjs';
import {assertionDetails,sanitizer} from './diagnostics.mjs';
export {expect};
const systemBudget=JSON.parse(await fs.readFile(new URL('../assets/system-budgets.v1.json',import.meta.url),'utf8')).milliseconds;
export type Child={exit_code:number,summary:any,result:any,directory:string,recovery?:any};
export type Controller={children:Child[],step<T>(title:string,action:()=>Promise<T>):Promise<T>,run(args:string[],options?:{env?:Record<string,string>,allowReportFailure?:boolean,interruptSherpa?:boolean}):Promise<Child>};
export const test=base.extend<{controller:Controller}>({
  controller:async({},use,info)=>{
    const id=info.title.match(/^SYSTEM-\d{3}/)?.[0];const raw=process.env.API_TEST_RAW_DIR;
    if(!id||!raw)throw new Error('use the API CLI for system controllers');
    const config=JSON.parse(await fs.readFile(path.join(raw,'configurations.json'),'utf8'))[id];
    const evidenceDirectory=path.join(raw,'environment-proof',`${id}-${info.retry}`);
    await fs.mkdir(evidenceDirectory,{recursive:true,mode:0o700});
    const active=new Map<any,()=>Promise<void>>();const cleanupErrors:string[]=[];const interruptionRefs:string[]=[];
    const steps:any[]=[];const children:Child[]=[];const began=Date.now();let details:any;let failedStep='controller';let environmentCode:string|undefined;
    const controller:Controller={children,async step(title,action){
      const item={step_id:`step-${steps.length}`,title,status:'passed',duration_ms:0};steps.push(item);const start=Date.now();
      try{return await base.step(title,action)}catch(error){item.status='failed';failedStep=item.step_id;details=assertionDetails(error);environmentCode=(error as any)?.safeCode;throw error}finally{item.duration_ms=Date.now()-start}
    },async run(args,options={}){
      const runsRoot=path.join(project,'artifacts/restricted/runs');
      const previousRuns=new Set(await fs.readdir(runsRoot));
      const child=spawn(process.execPath,[path.join(project,'cli/api-test.mjs'),...args],{cwd:project,env:{...process.env,API_TEST_RAW_DIR:'',API_TEST_RUN_ID:'',API_TEST_HOST:'',API_TEST_RETRIES:'0',...options.env},stdio:['ignore','pipe','pipe']});
      let stdout='';let stderr='';child.stdout.on('data',chunk=>{stdout=(stdout+chunk).slice(-8192)});child.stderr.on('data',chunk=>{stderr=(stderr+chunk).slice(-8192)});
      const completion=new Promise<{code:number|null,signal:NodeJS.Signals|null}>(resolve=>{child.once('exit',(code,signal)=>resolve({code,signal}));child.once('error',()=>resolve({code:null,signal:null}))});
      const watchdog=setTimeout(()=>child.kill('SIGINT'),systemBudget.child_run);const forceWatchdog=setTimeout(()=>child.kill('SIGKILL'),systemBudget.child_run+systemBudget.child_force_stop_wait);
      let interrupted:any;let recovery:any;let record:Child|undefined;let retained:string|undefined;let finalization:Promise<void>|undefined;
      const inspector=new AttemptEnvironment({root:path.join(project,'artifacts/environments'),caseId:'SHERPA-001'});
      const recover=async()=>{
        if(!interrupted)return false;
        const root=path.join(project,'artifacts/environments');const env=new AttemptEnvironment({root,caseId:'SHERPA-001',runId:interrupted.run_id});
        recovery={interrupted,cleanup_status:'failed',home_removed:false,host_gone:false,sidecar_gone:false};
        const reference=`environment-proof/${id}-${info.retry}/interruption-${interrupted.run_id}.json`;
        if(!interruptionRefs.includes(reference))interruptionRefs.push(reference);
        try{
          const deadline=Date.now()+systemBudget.child_force_stop_wait;
          await env.stopOwned(interrupted.framework,deadline);await env.stopOwned(interrupted.worker,deadline);
          const recovered=await cleanupStale(root);const item=recovered.find((item:any)=>item.home===env.home);
          let removed=false;try{await fs.stat(env.home)}catch(error){if((error as NodeJS.ErrnoException).code==='ENOENT')removed=true;else throw error}
          recovery={...recovery,cleanup_status:item?.status??(removed?'passed':'unverified'),home_removed:removed,host_gone:!sameIdentity(await env.identity(interrupted.host.pid),interrupted.host),sidecar_gone:!sameIdentity(await env.identity(interrupted.sidecar.pid),interrupted.sidecar)};
          if(recovery.cleanup_status!=='passed'||!recovery.home_removed||!recovery.host_gone||!recovery.sidecar_gone)throw new Error('owned environment recovery unverified');
          return true;
        }finally{
          await fs.writeFile(path.join(raw,reference),JSON.stringify(recovery,null,2),{mode:0o600});
          if(retained)await fs.writeFile(path.join(retained,'interruption.json'),JSON.stringify(recovery,null,2),{mode:0o600});
          if(record)record.recovery=recovery;
        }
      };
      const finish=()=>finalization??=(async()=>{await finalizeChild({child,completion,timers:[watchdog,forceWatchdog],recover,recordFailure:(code:string)=>cleanupErrors.push(code),grace:systemBudget.child_force_stop_wait,force:3000});if(options.interruptSherpa&&!interrupted)cleanupErrors.push('child_environment_recovery_unverified');active.delete(child)})();
      active.set(child,finish);
      try{
        const cliIdentity=child.pid?await inspector.identity(child.pid):null;
        if(options.interruptSherpa){
          const deadline=Date.now()+90000;
          while(!interrupted&&Date.now()<deadline){
            for(const name of await fs.readdir(runsRoot)){
              if(previousRuns.has(name))continue;
              let manifest:any;try{manifest=JSON.parse(await fs.readFile(path.join(runsRoot,name,'run-manifest.json'),'utf8'))}catch{continue}
              if(manifest.run_id!==name||manifest.runner_pid!==child.pid)continue;
              const env=new AttemptEnvironment({root:path.join(project,'artifacts/environments'),caseId:'SHERPA-001',runId:manifest.run_id});
              try{
                const proof=JSON.parse(await fs.readFile(env.recoveryPath,'utf8'));
                for(const file of (await fs.readdir(path.join(env.home,'sidecars'))).filter((name:string)=>name.endsWith('.pid'))){
                  const sidecar=JSON.parse(await fs.readFile(path.join(env.home,'sidecars',file),'utf8'));
                  if(!await admitInterruption({manifest,proof,sidecar,cliIdentity,previousRuns,inspect:(pid:number)=>env.identity(pid)}))continue;
                  interrupted={run_id:manifest.run_id,cli:cliIdentity,sidecar:sidecar.child,host:sidecar.owner,worker:proof.runner,framework:manifest.framework_identity};
                  process.kill(interrupted.framework.pid,'SIGKILL');break;
                }
              }catch{}
              if(interrupted)break;
            }
            if(!interrupted)await new Promise(resolve=>setTimeout(resolve,50));
          }
          if(!interrupted)throw new ConfigurationError('live_sherpa_interruption_unavailable');
        }
        const status=await completion;
        let summary;for(const line of (stdout+'\n'+stderr).split('\n')){try{const value=JSON.parse(line);if(['passed','failed','incomplete'].includes(value.gate)&&Number.isInteger(value.exit_code))summary=value}catch{}}
        if(!summary)throw new ConfigurationError('child_safe_result_missing');
        if(!summary.result&&options.allowReportFailure&&summary.report_status==='failed')summary.result=`artifacts/reports/${summary.run_id}/result.json`;
        if(!summary.result)throw new ConfigurationError(`child_${summary.code??'result_missing'}`);
        const resultFile=path.resolve(project,summary.result);
        if(!resultFile.startsWith(path.join(project,'artifacts/reports')+path.sep))throw new Error('child report outside owned report root');
        const directory=path.dirname(resultFile);const result=JSON.parse(await fs.readFile(resultFile,'utf8'));
        expect(status.signal).toBeNull();expect(status.code).toBe(result.exit_code);expect(summary.exit_code).toBe(result.exit_code);expect(result.report_status).toBe(options.allowReportFailure?'failed':'passed');
        retained=path.join(evidenceDirectory,result.run_id);await fs.mkdir(retained,{mode:0o700});
        for(const name of ['result.json','playwright.json','replay','environment-proof','comparison.json']){
          try{await fs.cp(path.join(directory,name),path.join(retained,name),{recursive:true})}catch(error){if((error as NodeJS.ErrnoException).code!=='ENOENT')throw error}
        }
        record={exit_code:status.code!,summary,result,directory};children.push(record);return record;
      }finally{await finish()}

    }};
    try{await use(controller)}finally{
      for(const finish of active.values())await finish();
      const cleanupFailed=cleanupErrors.length>0;
      const failed=info.status!==info.expectedStatus;
      const reference=`environment-proof/${id}-${info.retry}/children.json`;
      await fs.writeFile(path.join(raw,reference),JSON.stringify({case_id:id,cleanup_errors:cleanupErrors,interruption_evidence:interruptionRefs,children:children.map(child=>({run_id:child.result.run_id,exit_code:child.exit_code,gate:child.result.gate,original_report:child.summary.result,retained_report:`${child.result.run_id}/result.json`}))},null,2),{mode:0o600});
      const clean=sanitizer();
      const result={attempt_id:`${id}-attempt-${info.retry}`,retry_index:info.retry,execution_status:failed?(environmentCode?'environment_error':'failed'):'passed',teardown_status:cleanupFailed?'failed':'passed',duration_ms:Date.now()-began,config,steps,errors:failed?[{category:environmentCode?'environment':'business',step_id:failedStep,code:environmentCode??'system_validation_failed',message:clean(details?.message??'controller validation failed'),expected:details?.expected==null?null:clean(details.expected),actual:details?.actual==null?null:clean(details.actual),evidence_refs:[reference]}]:[],evidence_refs:[reference,...interruptionRefs]};
      for(const code of [...new Set(cleanupErrors)])result.errors.push({category:'teardown',step_id:'controller',code,message:code==='child_exit_unverified'?'child exit could not be verified; recovery manifests retained':'owned environment recovery not verified; manifests retained',expected:null,actual:null,evidence_refs:[reference,...interruptionRefs]});
      await fs.mkdir(path.join(raw,'attempts'),{recursive:true,mode:0o700});await fs.writeFile(path.join(raw,'attempts',`${id}-${info.retry}.json`),JSON.stringify(result,null,2),{mode:0o600});
      if(cleanupFailed)throw new Error('controller teardown failed; recovery evidence retained');
    }
  },
});
