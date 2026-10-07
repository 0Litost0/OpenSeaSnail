import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {fileURLToPath} from 'node:url';
const exec=promisify(execFile);
export const project=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
export const repository=path.resolve(project,'../..');
export const host=path.join(repository,'target/debug/seasnail-api-test-host');
export const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
export async function hash(file){return sha(await fs.readFile(file))}
export async function sourceIdentity() {
  const {stdout:tracked}=await exec('git',['ls-files','-z'],{cwd:repository,maxBuffer:8*1024*1024});
  const {stdout:untracked}=await exec('git',['ls-files','--others','--exclude-standard','-z'],{cwd:repository,maxBuffer:8*1024*1024});
  const prefix='tests/api_regression/';
  const files=[...new Set((tracked+untracked).split('\0').filter(Boolean))].filter(file=>file.startsWith('crates/')||file.startsWith('proto/')||file.startsWith('resources/')||file.startsWith(prefix)||['Cargo.toml','Cargo.lock'].includes(file)).sort();
  const entries=[];for(const file of files){try{const bytes=await fs.readFile(path.join(repository,file));entries.push({path:file,sha256:sha(bytes),body:bytes.toString('base64')})}catch(error){if(error.code!=='ENOENT')throw error;entries.push({path:file,deleted:true})}}
  const serialized=JSON.stringify({version:1,scope:'daemon/runtime plus formal API regression source and assets',entries});
  const digest=sha(serialized);const directory=path.join(project,'artifacts/restricted/source-assets');await fs.mkdir(directory,{recursive:true,mode:0o700});
  await fs.writeFile(path.join(directory,digest+'.json'),serialized,{mode:0o600});
  return {id:'source-'+digest,sha256:digest};
}
export async function buildIdentity() {
  const {stdout:commit}=await exec('git',['rev-parse','HEAD'],{cwd:repository});
  const {stdout:status}=await exec('git',['status','--porcelain'],{cwd:repository});
  const {stdout:toolchain}=await exec('rustc',['-vV'],{cwd:repository});
  return {commit:commit.trim(),dirty:Boolean(status.trim()),binary_sha256:await hash(host),source_patch:status.trim()?await sourceIdentity():null,toolchain:toolchain.split('\n')[0],target:toolchain.match(/^host: (.+)$/m)?.[1]??'unknown',profile:'debug',lock_sha256:await hash(path.join(project,'Cargo.lock')),features:[],rustflags:process.env.RUSTFLAGS??''};
}
export async function executionAssets() {
  const files=[];
  for(const directory of ['cli','fixtures','specs'])for(const name of (await fs.readdir(path.join(project,directory))).sort())if(/\.(?:mjs|ts)$/.test(name))files.push(directory+'/'+name);
  return [...files,'scripts/publish_report.py','scripts/check_contracts.py','contracts/result.schema.json','contracts/replay.schema.json','assets/system-budgets.v1.json','scripts/quality.py','scripts/check_assets.py','scripts/generate_audio.py'];
}
export async function baseConfiguration(entry,build) {
  const files=[...await executionAssets(),'case-catalog.json','contracts/assertions.v1.json','assets/manifest.json','assets/pipeline-audio.json','assets/audio/pipeline-v1.wav','assets/scenarios/asr.json','assets/scenarios/provider.json','assets/quality-rules.v3.json','assets/budgets.v1.json','assets/provider-eval.v1.json',...entry.asset_refs];
  const assets=[];for(const file of [...new Set(files)].sort())assets.push({id:file,sha256:await hash(path.join(project,file))});
  const providerModes={'CLEAN-001':'success','CLEAN-002':'http-503','CLEAN-003':'invalid-json','CLEAN-004':'timeout','CLEAN-005':'success','DICT-002':'success','RECOVERY-001':'success','RECOVERY-002':'success'};
  const pinned=JSON.parse(await fs.readFile(path.join(project,'assets/sherpa-baseline.v2.json'),'utf8'));
  const runtime=entry.case_id==='SHERPA-001'?{mode:'sherpa',model_id:'sensevoice-small-sherpa-int8',artifacts:[...pinned.bundle_files.map(file=>({id:'sherpa-bundle/'+file.path,sha256:file.sha256})),{id:'ffmpeg-v1',sha256:pinned.ffmpeg.sha256}]}:{mode:'deterministic',scenario:entry.case_id==='ASR-002'?'asr-retry-v1':'asr-success-v1'};
  return {catalog_version:'v1',build,assets,assertion_version:entry.case_id+'-v1',runtime,provider:{mode:providerModes[entry.case_id]??'disabled',scenario_version:'v1'},keychain:{mode:'persistent-fixture'},budget_version:'macos-baseline-v1',fault:null,credential_refs:[]};
}
