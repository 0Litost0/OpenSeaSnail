import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
const exec=promisify(execFile);
export const sameIdentity=(a,b)=>Boolean(a&&b&&a.pid===b.pid&&a.birth===b.birth&&a.executable===b.executable);
export async function parentOf(pid){
  const {stdout}=await exec('ps',['-p',String(pid),'-o','ppid='],{timeout:5000,maxBuffer:1024});
  const parent=Number(stdout.trim());if(!Number.isInteger(parent)||parent<=0)throw new Error('parent identity unavailable');return parent;
}
export async function admitInterruption({manifest,proof,sidecar,cliIdentity,previousRuns,inspect,parent=parentOf}) {
  if(previousRuns.has(manifest.run_id)||manifest.run_id!==proof.run_id||proof.case_id!=='SHERPA-001'||JSON.stringify(manifest.selected_case_ids)!==JSON.stringify(['SHERPA-001']))return false;
  if(manifest.runner_pid!==cliIdentity?.pid||!sameIdentity(manifest.runner_identity,cliIdentity)||!sameIdentity(await inspect(cliIdentity.pid),cliIdentity))return false;
  const framework=manifest.framework_identity,worker=proof.runner,host=sidecar.owner,child=sidecar.child;
  if(manifest.framework_pid!==framework?.pid||!proof.hosts.some(value=>sameIdentity(value,host)))return false;
  for(const identity of [framework,worker,host,child])if(!identity||!sameIdentity(await inspect(identity.pid),identity))return false;
  // The registered environment runner is a Playwright worker, not its coordinator.
  for(const [descendant,owner] of [[framework,cliIdentity],[worker,framework],[host,worker],[child,host]]){
    if(await parent(descendant.pid)!==owner.pid||!sameIdentity(await inspect(descendant.pid),descendant))return false;
  }
  return sameIdentity(await inspect(framework.pid),framework);
}
export async function bounded(promise,milliseconds){
  let timer;
  try{return await Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('child exit unverified')),milliseconds)})])}finally{clearTimeout(timer)}
}
export async function stopChild(child,completion,grace=15000,force=3000){
  if(child.exitCode!==null||child.signalCode!==null){await completion;return {forced:child.signalCode==='SIGKILL'}}
  child.kill('SIGINT');
  try{await bounded(completion,grace);return {forced:false}}catch{child.kill('SIGKILL');await bounded(completion,force);return {forced:true}}
}
// Even report parsing/copying failures must release watchdogs, verify runner exit,
// and recover the already admitted environment. Cleanup failure is separate evidence.
export async function finalizeChild({child,completion,timers,recover,recordFailure,grace,force}){
  for(const timer of timers)clearTimeout(timer);
  let forced=false;let recovered=false;
  try{forced=(await stopChild(child,completion,grace,force)).forced}catch{forced=true;recordFailure('child_exit_unverified')}
  if(recover)try{recovered=await recover()===true}catch{recordFailure('child_environment_recovery_failed')}
  if(forced&&!recovered)recordFailure('child_environment_recovery_unverified')
}
