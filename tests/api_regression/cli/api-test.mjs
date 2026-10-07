#!/usr/bin/env node
import fs from 'node:fs/promises';
import path from 'node:path';
import {spawn,execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {fileURLToPath} from 'node:url';
import {randomUUID} from 'node:crypto';
import {buildIdentity,baseConfiguration,hash} from './identity.mjs';
import {adapt,aggregate,diagnostic,failureSummary} from './result.mjs';
import {readReplay,verifyReplay,ConfigurationError} from './replay.mjs';
import {prepareMutation,faults} from './mutation.mjs';
import {loadCatalog,extensionDirectory} from './catalog.mjs';
import {pruneEvidence} from './retention.mjs';
import {AttemptEnvironment,cleanupStale} from '../fixtures/environment.mjs';
const exec=promisify(execFile);
const systemBudget=JSON.parse(await fs.readFile(new URL('../assets/system-budgets.v1.json',import.meta.url),'utf8')).milliseconds;
const project=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const playwright=path.join(project,'node_modules/@playwright/test/cli.js');
let catalog;
const required=JSON.parse(await fs.readFile(path.join(project,'contracts/acceptance-required.v1.json'),'utf8')).required_case_ids;
export async function discover() {
  const {stdout}=await exec(process.execPath,[playwright,'test','--list','--reporter=json'],{cwd:project,timeout:30000,maxBuffer:8*1024*1024});
  const report=JSON.parse(stdout);const found=[];
  function walk(suite){for(const spec of suite.specs??[]){const id=spec.title.match(/^([A-Z]+-\d{3})\b/)?.[1];if(!id)throw new Error('unregistered discovered title');found.push(id)}for(const child of suite.suites??[])walk(child)}
  for(const suite of report.suites??[])walk(suite);
  if(new Set(found).size!==found.length || found.length!==catalog.cases.length || catalog.cases.some(entry=>!found.includes(entry.case_id)))throw new Error('catalog/discovery mismatch');
  return found;
}
async function execute(selected,extra,mode,replayConfiguration=null,evalSelection=null,faultId=null) {
  await pruneEvidence(project);
  const runId=randomUUID();const raw=path.join(project,'artifacts/restricted/runs',runId);
  const destination=path.join(project,'artifacts/reports',runId);
  await exec(process.execPath,[path.join(project,'cli/build-host.mjs')],{timeout:180000,maxBuffer:8192});
  const normalBuild=await buildIdentity();
  let mutation=null;
  if(replayConfiguration?.fault&&mode==='replay')await verifyReplay(replayConfiguration,replayConfiguration.build,false);
  if(faultId||(replayConfiguration?.fault&&mode==='replay'))mutation=await prepareMutation(faultId??replayConfiguration.fault.id,normalBuild,replayConfiguration);
  const build=mutation?.build??normalBuild;
  const configurations={};for(const entry of selected)configurations[entry.case_id]=await baseConfiguration(entry,build);
  if(evalSelection)configurations['PROVIDER-001']={...configurations['PROVIDER-001'],provider:{mode:'remote',provider_id:evalSelection.providerId,model:evalSelection.entry.model,endpoint_ref:evalSelection.entry.endpoint_ref,output_deterministic:false,scenario_version:'v1'},credential_refs:[{id:'provider-credential',source:'environment',reference:evalSelection.entry.credential_ref}]};
  let comparison=null;
  if(replayConfiguration){
    comparison=await verifyReplay(replayConfiguration,build,mode==='replay',Boolean(mutation));
    const {schema_version,case_id,...original}=replayConfiguration;
    configurations[case_id]={...original,build,fault:mutation?.fault??null};
  }
  if(mutation)for(const entry of selected)configurations[entry.case_id].fault=mutation.fault;
  await fs.mkdir(raw,{recursive:true,mode:0o700});
  if(comparison)await fs.writeFile(path.join(raw,'comparison.json'),JSON.stringify(comparison,null,2),{mode:0o600});
  await fs.writeFile(path.join(raw,'configurations.json'),JSON.stringify(configurations),{mode:0o600});
  const grep=`@case_(${selected.map(entry=>entry.case_id).join('|')})\\b`;
  const child=spawn(process.execPath,[playwright,'test','--grep',grep,...extra],{cwd:project,env:{...process.env,API_TEST_RAW_DIR:raw,API_TEST_RUN_ID:runId,API_TEST_HOST:mutation?.host??''},stdio:['ignore','pipe','pipe']});
  const runnerCompletion=new Promise(resolve=>{child.once('exit',(code,signal)=>resolve({code,signal}));child.once('error',()=>resolve({code:2,signal:'spawn_error'}))});
  // Framework stdout/errors can contain secrets. Persist restricted, never echo them.
  let output='';child.stdout.on('data',chunk=>{output=(output+chunk).slice(-262144)});child.stderr.on('data',chunk=>{output=(output+chunk).slice(-262144)});
  const interrupted=()=>child.kill('SIGINT');
  process.on('SIGINT',interrupted);process.on('SIGTERM',interrupted);
  const inspector=new AttemptEnvironment({runId,caseId:selected[0].case_id});
  const identities=await Promise.allSettled([inspector.identity(process.pid),child.pid?inspector.identity(child.pid):Promise.resolve(null)]);
  const runnerIdentity=identities[0].status==='fulfilled'?identities[0].value:null;const frameworkIdentity=identities[1].status==='fulfilled'?identities[1].value:null;
  await fs.writeFile(path.join(raw,'run-manifest.json'),JSON.stringify({run_id:runId,runner_pid:process.pid,runner_identity:runnerIdentity,framework_pid:child.pid,framework_identity:frameworkIdentity,selected_case_ids:selected.map(entry=>entry.case_id),build,started_at:new Date().toISOString()}),{mode:0o600});
  // A framework crash/hang must not strand the CLI indefinitely.
  const watchdog=setTimeout(()=>child.kill('SIGINT'),mode==='acceptance'?systemBudget.acceptance_runner_total:systemBudget.runner_total);
  const forceWatchdog=setTimeout(()=>child.kill('SIGKILL'),(mode==='acceptance'?systemBudget.acceptance_runner_total:systemBudget.runner_total)+systemBudget.child_force_stop_wait);
  const runnerStatus=await runnerCompletion;clearTimeout(watchdog);clearTimeout(forceWatchdog);
  process.off('SIGINT',interrupted);process.off('SIGTERM',interrupted);
  await fs.writeFile(path.join(raw,'runner.log'),output,{mode:0o600});
  const framework={name:'playwright',version:'1.63.0',lock_sha256:await hash(path.join(project,'pnpm-lock.yaml'))};
  const result=await adapt({raw,runId,build,framework,budgetVersion:'macos-baseline-v1',catalog,required,selected,mode,configurations,runnerStatus});
  if(mode==='acceptance'||selected.some(entry=>entry.case_id==='SHERPA-001')){
    const rules=JSON.parse(await fs.readFile(path.join(project,'assets/quality-rules.v3.json'),'utf8'));
    const budgets=JSON.parse(await fs.readFile(path.join(project,'assets/budgets.v1.json'),'utf8'));
    if(rules.status!=='approved'||budgets.status!=='approved'||!rules.approval||!budgets.approval){result.errors.push(diagnostic('configuration','quality_rules_or_budgets_unapproved','quality gate remains pending confirmation'));aggregate(result)}
  }
  await fs.writeFile(path.join(raw,'result.json'),JSON.stringify(result,null,2),{mode:0o600});
  await fs.mkdir(path.join(raw,'replay'),{mode:0o700});
  for(const entry of selected)await fs.writeFile(path.join(raw,'replay',entry.case_id+'.json'),JSON.stringify({schema_version:1,case_id:entry.case_id,...configurations[entry.case_id]},null,2),{mode:0o600});
  let secretRegistry='';try{secretRegistry=await fs.readFile(path.join(raw,'secrets.ndjson'),'utf8')}catch{}
  try{
    await exec('python3',[path.join(project,'scripts/publish_report.py'),raw,destination],{timeout:5000,maxBuffer:8192,killSignal:'SIGKILL'});
    const published=JSON.parse(await fs.readFile(path.join(destination,'result.json'),'utf8'));
    console.log(JSON.stringify({run_id:runId,scope:mode,gate:published.gate,exit_code:published.exit_code,result:path.relative(project,path.join(destination,'result.json')),html:path.relative(project,path.join(destination,'html/index.html'))}));
    return published.exit_code;
  }catch{
    await fs.rm(raw,{recursive:true,force:true});
    await fs.rm(path.join(path.dirname(destination),".publish-"+runId),{recursive:true,force:true});
    result.report_status='failed';result.errors.push(diagnostic('report','report_check_failed','unsafe or unparseable report was not published'));aggregate(result);
    // Step titles and assertion values are user-controlled; never publish them
    // after the native scan failed. Even this reduced summary must pass scanning.
    const summary=failureSummary(result);
    const safeRaw=path.join(project,'artifacts/restricted/runs',runId+'-summary');
    await fs.mkdir(safeRaw,{recursive:true,mode:0o700});
    await fs.writeFile(path.join(safeRaw,'result.json'),JSON.stringify(summary),{mode:0o600});
    await fs.writeFile(path.join(safeRaw,'secrets.ndjson'),secretRegistry,{mode:0o600});
    try{await exec('python3',[path.join(project,'scripts/publish_report.py'),safeRaw,destination],{timeout:5000,maxBuffer:8192,killSignal:'SIGKILL'})}
    catch{await fs.rm(safeRaw,{recursive:true,force:true});await fs.rm(path.join(path.dirname(destination),'.publish-'+runId),{recursive:true,force:true})}
    console.log(JSON.stringify({run_id:runId,scope:mode,gate:result.gate,exit_code:result.exit_code,report_status:'failed'}));
    return result.exit_code;
  }
}
async function main() {
  const args=process.argv.slice(2);const command=args.shift();
  const extensionIndex=args.indexOf('--extension');
  if(extensionIndex>=0){if(!args[extensionIndex+1])throw new ConfigurationError('extension_argument_invalid');process.env.API_TEST_EXTENSION=await extensionDirectory(args[extensionIndex+1]);args.splice(extensionIndex,2)}
  catalog=await loadCatalog();
  if(command==='--help'||command==='help'||!command){console.log('api-test list [--json] | run <case-id> [--config <replay.json> | --fault <fault-id>] | suite <suite-id> | quick | acceptance | replay <replay.json> | provider-eval --provider <id> | cleanup-stale [--extension <directory>]');return 0}
  const separator=args.indexOf('--');const extra=separator<0?[]:args.splice(separator).slice(1);
  if(command==='cleanup-stale'){
    const root=path.join(project,'artifacts/environments');let results=[];
    try{results=await cleanupStale(root)}catch(error){if(error.code!=='ENOENT')throw error}
    console.log(JSON.stringify({cleanup:results.map(result=>({status:result.status,home_removed:result.home_removed??false}))}));
    return results.some(result=>result.status==='unverified')?2:results.some(result=>result.status==='failed')?1:0;
  }
  let configuration=null;let faultId=null;
  const faultIndex=args.indexOf('--fault');
  if(faultIndex>=0){if(command!=='run'||faultIndex!==1||args.length!==3||faults[args[2]]?.caseId!==args[0])throw new ConfigurationError('fault_selection_invalid');faultId=args[2];args.splice(1);}
  if(command==='replay'){if(args.length!==1)throw new ConfigurationError('replay_argument_invalid');configuration=await readReplay(args[0],true);args[0]=configuration.case_id;}
  const configIndex=args.indexOf('--config');
  if(configIndex>=0){if(command!=='run'||configIndex!==1||args.length!==3)throw new ConfigurationError('run_config_argument_invalid');configuration=await readReplay(args[2]);args.splice(1);if(configuration.case_id!==args[0])throw new ConfigurationError('run_config_case_mismatch');}
  try{await exec('python3',[path.join(project,'scripts/check_contracts.py'),'catalog'],{timeout:5000,maxBuffer:8192});await exec('python3',[path.join(project,'scripts/check_assets.py')],{timeout:5000,maxBuffer:8192})}catch{throw new ConfigurationError('catalog_contract_invalid')}
  const discovered=await discover();
  if(command==='list'){
    if(args.some(arg=>arg!=='--json'))throw new Error('invalid list options');
    if(args.includes('--json'))console.log(JSON.stringify({catalog_version:catalog.catalog_version,cases:catalog.cases.map(entry=>({...entry,discovered:discovered.includes(entry.case_id)}))},null,2));
    else for(const entry of catalog.cases)console.log(`${entry.case_id} [${entry.suite}] ${entry.purpose}`);
    return 0;
  }
  let selected;let evalSelection=null;
  if(command==='run'||command==='replay')selected=catalog.cases.filter(entry=>entry.case_id===args[0]);
  else if(command==='suite')selected=catalog.cases.filter(entry=>entry.suite===args[0]);
  else if(command==='quick')selected=catalog.cases.filter(entry=>entry.quick);
  else if(command==='acceptance')selected=catalog.cases.filter(entry=>required.includes(entry.case_id));
  else if(command==='provider-eval'){
    if(args.length!==2||args[0]!=='--provider')throw new ConfigurationError('provider_eval_argument_invalid');
    const entry=await evalEntry(args[1]);
    selected=catalog.cases.filter(item=>item.case_id==='PROVIDER-001');
    evalSelection={providerId:args[1],entry};
  }
  else throw new Error('unknown command');
  if(!selected.length || args.length>({run:1,suite:1,replay:1,'provider-eval':2}[command]??0))throw new Error('invalid selection');
  return execute(selected,extra,command,configuration,evalSelection,faultId);
}
async function evalEntry(providerId) {
  const reference=/^[a-zA-Z0-9][a-zA-Z0-9._/-]*$/;
  const registry=JSON.parse(await fs.readFile(path.join(project,'assets/provider-eval.v1.json'),'utf8'));
  let providers={...registry.providers};
  try{const local=JSON.parse(await fs.readFile(path.join(project,'assets/provider-eval.local.json'),'utf8'));providers={...providers,...local.providers}}catch(error){if(error.code!=='ENOENT')throw new ConfigurationError('provider_eval_registry_invalid')}
  const entry=providers[providerId];
  if(!entry||!entry.provider_type||!entry.model||!reference.test(entry.endpoint_ref??'')||!reference.test(entry.credential_ref??'')||entry.endpoint_ref===entry.credential_ref)throw new ConfigurationError('provider_eval_unknown_provider');
  return entry;
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  try{process.exitCode=await main()}catch(error){console.error(JSON.stringify({gate:'incomplete',exit_code:2,code:error.safeCode??'configuration_or_dependency_error'}));process.exitCode=2}
}
