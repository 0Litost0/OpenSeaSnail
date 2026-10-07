// Faults are built exclusively from hash-verified snapshots into isolated source/target trees.
import fs from 'node:fs/promises';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {project,repository,sourceIdentity,sha,hash} from './identity.mjs';
import {ConfigurationError} from './replay.mjs';
const exec=promisify(execFile);
const systemBudget=JSON.parse(await fs.readFile(new URL('../assets/system-budgets.v1.json',import.meta.url),'utf8')).milliseconds;
export const faults={
  'MUT-CLEAN-FALLBACK-001':{caseId:'CLEAN-002',file:'crates/daemon/src/application/transcription.rs'},
  'MUT-DICT-PERSIST-001':{caseId:'DICT-001',file:'crates/daemon/src/application/dictionary.rs'},
};
export async function readSource(reference) {
  if(!reference||reference.id!=='source-'+reference.sha256||!/^[a-f0-9]{64}$/.test(reference.sha256))throw new ConfigurationError('fault_source_identity_invalid');
  const file=path.join(project,'artifacts/restricted/source-assets',reference.sha256+'.json');
  let bytes;try{if((await fs.lstat(file)).isSymbolicLink())throw new Error();bytes=await fs.readFile(file)}catch{throw new ConfigurationError('fault_source_asset_missing')}
  if(sha(bytes)!==reference.sha256)throw new ConfigurationError('fault_source_asset_hash_mismatch');
  const snapshot=JSON.parse(bytes);
  for(const entry of snapshot.entries){
    if(!/^(crates|proto|resources|tests\/api_regression)\//.test(entry.path)&&!['Cargo.toml','Cargo.lock'].includes(entry.path))throw new ConfigurationError('fault_source_path_invalid');
    if(entry.path.split('/').some(part=>['','..','.'].includes(part)))throw new ConfigurationError('fault_source_path_invalid');
    if(!entry.deleted&&sha(Buffer.from(entry.body,'base64'))!==entry.sha256)throw new ConfigurationError('fault_source_entry_hash_mismatch');
  }
  return snapshot;
}
export async function prepareMutation(id,normalBuild,replay=null) {
  const definition=faults[id];if(!definition)throw new ConfigurationError('fault_unknown');
  if(replay&&!replay.fault.base_source)throw new ConfigurationError('fault_base_source_missing');
  const baseSource=replay?replay.fault.base_source:await sourceIdentity();
  const snapshot=await readSource(baseSource);
  const patchPath=path.join(project,'assets/mutations',id+'.patch');const patchSha=await hash(patchPath);
  if(replay&&(replay.fault.patch_sha256!==patchSha||replay.case_id!==definition.caseId))throw new ConfigurationError('fault_patch_or_case_mismatch');
  const root=path.join(project,'artifacts/restricted/mutation-workspaces',baseSource.sha256,id);
  await fs.mkdir(root,{recursive:true,mode:0o700});
  if(await fs.realpath(root)!==root)throw new ConfigurationError('fault_workspace_symlink');
  const source=path.join(root,'source');const target=path.join(root,'target');
  await fs.rm(source,{recursive:true,force:true});await fs.mkdir(source,{mode:0o700});
  for(const entry of snapshot.entries){if(entry.deleted)continue;const file=path.join(source,entry.path);await fs.mkdir(path.dirname(file),{recursive:true,mode:0o700});await fs.writeFile(file,Buffer.from(entry.body,'base64'),{mode:0o600})}
  try {
    await exec('git',['apply','--check',patchPath],{cwd:source,timeout:5000,maxBuffer:8192});
    await exec('git',['apply',patchPath],{cwd:source,timeout:5000,maxBuffer:8192});
    // Each variant owns a separate Cargo target. Never copy the developer's
    // entire target (including unrelated builds/incremental state).
    await fs.mkdir(target,{recursive:true,mode:0o700});
    if(await fs.realpath(target)!==target)throw new Error('unsafe target');
    if(await fs.realpath(source)!==source)throw new Error('unsafe source');
    await exec('cargo',['build','--offline','--locked','--manifest-path',path.join(source,'tests/api_regression/Cargo.toml'),'--target-dir',target],{cwd:source,env:{...process.env,CARGO_TARGET_DIR:target,CARGO_INCREMENTAL:'0'},timeout:systemBudget.mutation_build,maxBuffer:1024*1024});
  }catch(error){
    const log=String(error.stderr??error.message??'fault build failed').slice(-262144);
    await fs.writeFile(path.join(root,'build-error.log'),log,{mode:0o600});
    throw new ConfigurationError('fault_patch_or_build_failed');
  }
  const binary=path.join(target,'debug/seasnail-api-test-host');
  const modified=structuredClone(snapshot);
  const entry=modified.entries.find(entry=>entry.path===definition.file);
  const bytes=await fs.readFile(path.join(source,definition.file));entry.sha256=sha(bytes);entry.body=bytes.toString('base64');
  const serialized=JSON.stringify(modified);const digest=sha(serialized);
  await fs.writeFile(path.join(project,'artifacts/restricted/source-assets',digest+'.json'),serialized,{mode:0o600});
  const build={...normalBuild,commit:replay?.fault.base_commit??normalBuild.commit,dirty:true,binary_sha256:await hash(binary),source_patch:{id:'source-'+digest,sha256:digest}};
  const fault={id,patch_sha256:patchSha,base_commit:build.commit,binary_sha256:build.binary_sha256,base_source:baseSource};
  if(replay&&JSON.stringify(replay.build)!==JSON.stringify(build))throw new ConfigurationError('fault_rebuilt_identity_mismatch');
  return {host:binary,build,fault,source};
}
