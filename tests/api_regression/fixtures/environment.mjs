import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import fs from 'node:fs/promises';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
const exec = promisify(execFile);
const project = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(project, '../..');
const budget = JSON.parse(await fs.readFile(path.join(project,'assets/budgets.v1.json'),'utf8')).milliseconds;
const pause = ms => new Promise(resolve=>setTimeout(resolve,ms));
const same = (a,b)=>Boolean(a && b && Number.isInteger(a.pid) && a.pid>0 && a.pid===b.pid && a.birth===b.birth && a.executable===b.executable);

async function atomic(file,value) {
  const temporary=`${file}.${randomUUID()}.tmp`;
  await fs.writeFile(temporary,JSON.stringify(value,null,2),{mode:0o600});
  await fs.rename(temporary,file);
}
export class AttemptEnvironment {
  constructor({root,caseId,attempt=0,runtime='deterministic',scenario='asr-success-v1',keychain='persistent',host=path.join(repository,'target/debug/seasnail-api-test-host'),runId=String(randomUUID())}) {
    if (!/^[A-Z]+-\d{3}$/.test(caseId) || !Number.isInteger(attempt) || attempt<0) throw new Error('invalid case/attempt');
    if(!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(runId))throw new Error('invalid run ID');
    this.root=path.resolve(root??path.join(project,'artifacts/environments'));
    this.home=path.join(this.root,runId,caseId,`attempt-${attempt}`);
    if (!this.home.startsWith(this.root+path.sep)) throw new Error('invalid run ID');
    this.recoveryPath=this.home+".environment.json";
    this.host=path.resolve(host); this.settings={runtime,scenario,keychain};
    this.manifest={version:1,run_id:runId,case_id:caseId,attempt,home:this.home,state:'new',hosts:[],cleanup_errors:[]};
  }
  async identity(pid) {
    const {stdout}=await exec(this.host,['--process-identity',String(pid)],{timeout:Math.max(1,Math.min(budget.api_request,(this.cleanupDeadline??Infinity)-Date.now())),maxBuffer:8192,killSignal:'SIGKILL'});
    return JSON.parse(stdout);
  }
  async save() {await atomic(this.recoveryPath,this.manifest);try{await atomic(path.join(this.home,'environment.json'),this.manifest)}catch(error){if(error.code!=='ENOENT')throw error}}
  async prepare() {
    await fs.mkdir(this.root,{recursive:true,mode:0o700});
    if ((await fs.lstat(this.root)).isSymbolicLink() || await fs.realpath(this.root)!==this.root) throw new Error('symlink root rejected');
    const marker=path.join(this.root,'.api-regression-root');
    try {await fs.writeFile(marker,'seasnail-api-regression-v1\n',{flag:'wx',mode:0o600})} catch(error) {if(error.code!=='EEXIST')throw error}
    if ((await fs.lstat(marker)).isSymbolicLink() || (await fs.readFile(marker,'utf8')).trim()!=='seasnail-api-regression-v1') throw new Error('invalid isolation marker');
    await fs.mkdir(path.dirname(this.home),{recursive:true,mode:0o700});
    await fs.mkdir(this.home,{recursive:false,mode:0o700});
    if (await fs.realpath(this.home)!==this.home) throw new Error('symlink home rejected');
    this.manifest.runner=await this.identity(process.pid);
    if(!this.manifest.runner)throw new Error("runner identity unavailable");
    await this.save();
  }
  async start() {
    if (this.child) throw new Error('old host must exit before start');
    this.modelReady=undefined;this.processEnded=false;
    this.diagnosticStreams??=[];
    const stream={pid:null,stdout:'',stderr:'',exit:null};this.diagnosticStreams.push(stream);
    this.manifest.state='starting';this.manifest.host_admission='pending';this.manifest.candidate_host_pid=null;await this.save();
    const config={root:this.root,home:this.home,...this.settings,scenario_file:path.join(project,'assets/scenarios/asr.json')};
    const configPath=path.join(this.home,'host-config.json'); await atomic(configPath,config);
    const environment={...process.env}; delete environment.SEASNAIL_DATA_DIR;
    this.child=spawn(this.host,[configPath],{env:environment,detached:true,stdio:['pipe','pipe','pipe']});
    const child=this.child; let output='',diagnostics='',ready=null,ended=false,spawnError=null;
    stream.pid=child.pid??null;
    this.exit=new Promise(resolve=>{child.once('exit',(code,signal)=>{ended=true;if(this.child===child)this.processEnded=true;resolve({code,signal})}); child.once('error',error=>{spawnError=error;ended=true;if(!child.pid)this.manifest.host_admission="not_spawned";resolve({error:error.code})})});
    child.stdout.on('data',chunk=>{
      stream.stdout=(stream.stdout+chunk.toString()).slice(-262144);
      output+=chunk.toString();
      if(output.length>262144){spawnError=new Error('host discovery output limit');output='';return}
      const lines=output.split('\n');output=lines.pop();
      for(const line of lines){try{const event=JSON.parse(line);if(event.event==='ready')ready=event;if(event.event==='model-ready'&&this.child===child)this.modelReady=event.success}catch{}}
    });
    child.stderr.on('data',chunk=>{diagnostics=(diagnostics+chunk.toString()).slice(-262144);stream.stderr=diagnostics});
    this.exit.then(status=>{stream.exit=status});
    if(child.pid){
      this.manifest.candidate_host_pid=child.pid;await this.save();
      const identity=await this.identity(child.pid);
      if(identity){this.manifest.hosts.push(identity);this.manifest.host_admission="registered";await this.save()}
    }
    const deadline=Date.now()+budget.startup;
    try {
      while(!ready && Date.now()<deadline){if(ended||spawnError)throw new Error('host exited before discovery');await pause(25)}
      if(!ready)throw new Error('startup deadline');
      if(ready.pid!==child.pid||ready.runtime!==this.settings.runtime||!Number.isInteger(ready.port))throw new Error('invalid discovery');
      const expected=['logging','singleton','parent_watch','bind','bootstrap','restore_and_compose','reconcile','reap','warmup_scheduled'];
      if(JSON.stringify(ready.stages)!==JSON.stringify(expected))throw new Error('startup stage contract mismatch');
      this.port=ready.port;this.baseURL=`http://127.0.0.1:${ready.port}/api/v1`;
      while(Date.now()<deadline){
        try{const response=await fetch(`http://127.0.0.1:${ready.port}/`,{signal:AbortSignal.timeout(Math.min(budget.api_request,Math.max(1,deadline-Date.now())))});if(response.ok)break}catch{}
        if(ended)throw new Error('host exited during health');await pause(25);
      }
      if(Date.now()>=deadline)throw new Error('HTTP health deadline');
      this.manifest.state='ready';this.manifest.port=ready.port;await this.save();
      return this;
    } catch(error) {this.manifest.state='startup_failed';await this.save();throw error}
    finally {await fs.writeFile(path.join(this.home,'host.stderr.log'),diagnostics,{mode:0o600})}
  }
  async waitModelReady() {
    const deadline=Date.now()+budget.model_ready;
    while(this.modelReady===undefined && Date.now()<deadline){if(this.processEnded)throw new Error("host exited before model readiness");await pause(40)}
    if(this.modelReady!==true)throw new Error(this.modelReady===false?'runtime warmup failed; no fallback':'model ready deadline');
  }
  async waitExited(identity,deadline) {
    while(Date.now()<deadline){const current=await this.identity(identity.pid);if(!same(current,identity))return;await pause(40)}
    throw new Error('process exit not confirmed');
  }
  async stopOwned(identity,deadline) {
    let current=await this.identity(identity.pid);if(!same(current,identity))return;
    try{process.kill(identity.pid,'SIGTERM')}catch(error){if(error.code!=='ESRCH')throw error}
    const graceful=Math.min(deadline,Date.now()+budget.graceful_stop);
    try {await this.waitExited(identity,graceful);return}catch{}
    current=await this.identity(identity.pid);
    if(same(current,identity)){try{process.kill(identity.pid,'SIGKILL')}catch(error){if(error.code!=='ESRCH')throw error}}
    await this.waitExited(identity,Math.min(deadline,Date.now()+budget.force_stop_wait));
  }
  async stop({deadline=Date.now()+budget.teardown_total}={}) {
    this.cleanupDeadline=deadline;
    if(this.manifest.host_admission==="pending" && !this.manifest.candidate_host_pid)throw new Error("host spawn admission not verified");
    if(this.manifest.candidate_host_pid){
      const current=await this.identity(this.manifest.candidate_host_pid);
      if(current && (this.manifest.host_admission==="pending" || !this.manifest.hosts.some(identity=>same(identity,current))))throw new Error("unverified startup host");
    } else if(["starting","ready","startup_failed"].includes(this.manifest.state) && this.manifest.host_admission!=="not_spawned")throw new Error("missing host admission identity");
    this.child?.stdin?.end();
    for(const identity of this.manifest.hosts)await this.stopOwned(identity,deadline);
    this.child=null;
    // Child records persist across abnormal host exit and task cancellation.
    let entries=[];try{entries=await fs.readdir(path.join(this.home,'sidecars'))}catch(e){if(e.code!=='ENOENT')throw e}
    for(const name of entries.filter(v=>v.endsWith('.pid'))){
      const file=path.join(this.home,'sidecars',name);
      if((await fs.lstat(file)).isSymbolicLink())throw new Error('unsafe child record');
      const record=JSON.parse(await fs.readFile(file,'utf8'));
      if(record.version!==1 || !this.manifest.hosts.some(host=>same(host,record.owner)))throw new Error('unverified subprocess owner');
      await this.stopOwned(record.child,deadline);
      await fs.unlink(file);
    }
    this.manifest.state='stopped';await this.save();this.cleanupDeadline=null;
  }
  async restart(overrides={}) {await this.stop();this.settings={...this.settings,...overrides};return this.start()}
  /** @param {{remove?:boolean,deadline?:number,beforeRemove?:()=>Promise<void>}} options */
  async teardown({remove=true,deadline=Date.now()+budget.teardown_total,beforeRemove}={}) {
    if(this.removed)return {status:"passed",home_removed:true};
    try{
      await this.stop({deadline});
      if(beforeRemove)await beforeRemove();
      if(remove){
        let present=true;try{if(await fs.realpath(this.home)!==this.home)throw new Error('unsafe removal path')}catch(error){if(error.code==='ENOENT')present=false;else throw error}
        if(!this.home.startsWith(this.root+path.sep))throw new Error('unsafe removal path');
        if(present)await exec(process.execPath,['--input-type=module','-e',"import fs from 'node:fs';fs.rmSync(process.argv[1],{recursive:true,force:false,maxRetries:0});",this.home],{timeout:Math.max(1,Math.min(budget.directory_cleanup,deadline-Date.now())),killSignal:'SIGKILL',maxBuffer:8192});
        await fs.unlink(this.recoveryPath);
      }
      this.removed=remove;
      return {status:'passed',home_removed:remove};
    }catch(error){
      this.manifest.state='cleanup_failed';this.manifest.cleanup_errors.push({code:'cleanup_unverified',message:error.message});
      try{await this.save()}catch{}
      return {status:'failed',home_removed:false,error:{code:'cleanup_unverified',message:error.message}};
    }
  }
}
export async function cleanupStale(root,host) {
  root=path.resolve(root); const results=[];
  if((await fs.readFile(path.join(root,'.api-regression-root'),'utf8')).trim()!=='seasnail-api-regression-v1')throw new Error('invalid recovery root');
  if(await fs.realpath(root)!==root || (await fs.lstat(path.join(root,".api-regression-root"))).isSymbolicLink())throw new Error("unsafe recovery root");
  async function visit(directory,depth=0){
    for(const entry of await fs.readdir(directory,{withFileTypes:true})){
      if(entry.isSymbolicLink())throw new Error('recovery symlink rejected');
      const file=path.join(directory,entry.name);
      if(entry.isDirectory() && depth<2)await visit(file,depth+1);
      else if(entry.name.endsWith('.environment.json')){
        const manifest=JSON.parse(await fs.readFile(file,'utf8'));
        if(manifest.version!==1||manifest.home+".environment.json"!==file)throw new Error('invalid recovery manifest');
        const environment=new AttemptEnvironment({root,caseId:manifest.case_id,attempt:manifest.attempt,runId:manifest.run_id,host});
        if(environment.home!==manifest.home || environment.recoveryPath!==file){results.push({home:manifest.home,status:"unverified",reason:"manifest_path_mismatch"});continue}
        environment.manifest=manifest;
        // A running host may belong to a live runner: do not claim it without runner identity.
        if(!manifest.runner){results.push({home:manifest.home,status:"unverified"});continue}
        if(manifest.runner && same(await environment.identity(manifest.runner.pid),manifest.runner)){results.push({home:manifest.home,status:'active'});continue}
        results.push({home:manifest.home,...await environment.teardown()});
      }
    }
  }
  await visit(root);return results;
}
