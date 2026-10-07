import fs from 'node:fs/promises';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {project,hash,executionAssets} from './identity.mjs';
const exec=promisify(execFile);
export class ConfigurationError extends Error {constructor(code){super(code);this.safeCode=code}}
export async function readReplay(file,requireSource=false) {
  try{await exec('python3',[path.join(project,'scripts/check_contracts.py'),'replay',path.resolve(file)],{timeout:5000,maxBuffer:8192})}catch{throw new ConfigurationError('replay_contract_invalid')}
  const configuration=JSON.parse(await fs.readFile(path.resolve(file),'utf8'));
  if(requireSource&&configuration.build.dirty){
    const source=configuration.build.source_patch;
    if(!source||source.id!=='source-'+source.sha256)throw new ConfigurationError('replay_source_identity_invalid');
    let digest;try{digest=await hash(path.join(project,'artifacts/restricted/source-assets',source.sha256+'.json'))}catch{throw new ConfigurationError('replay_source_asset_missing')}
    if(digest!==source.sha256)throw new ConfigurationError('replay_source_asset_hash_mismatch');
  }
  return configuration;
}
export async function verifyReplay(configuration,currentBuild,exact,faultRebuilt=false) {
  const {loadCatalog}=await import('./catalog.mjs');const catalog=await loadCatalog();
  const entry=catalog.cases.find(entry=>entry.case_id===configuration.case_id);
  if(!entry||configuration.catalog_version!==catalog.catalog_version)throw new ConfigurationError('replay_catalog_version_mismatch');
  if(configuration.assertion_version!==entry.case_id+'-v1')throw new ConfigurationError('replay_assertion_version_mismatch');
  const budgets=JSON.parse(await fs.readFile(path.join(project,'assets/budgets.v1.json'),'utf8'));
  if(configuration.budget_version!==budgets.version)throw new ConfigurationError('replay_budget_version_mismatch');
  if(configuration.provider.scenario_version!=='v1')throw new ConfigurationError('replay_provider_version_mismatch');
  const scenes=JSON.parse(await fs.readFile(path.join(project,'assets/scenarios/asr.json'),'utf8'));
  if(configuration.runtime.mode==='deterministic'&&!Object.hasOwn(scenes.scenarios,configuration.runtime.scenario))throw new ConfigurationError('replay_runtime_scenario_unknown');
  const needed=[...await executionAssets(),'case-catalog.json','contracts/assertions.v1.json','assets/manifest.json','assets/pipeline-audio.json','assets/audio/pipeline-v1.wav','assets/scenarios/asr.json','assets/scenarios/provider.json','assets/quality-rules.v3.json','assets/budgets.v1.json',...entry.asset_refs];
  if(needed.some(id=>!configuration.assets.some(asset=>asset.id===id)))throw new ConfigurationError('replay_required_asset_missing');
  if(configuration.provider.mode==='remote')throw new ConfigurationError('replay_remote_provider_unsupported');
  if(configuration.credential_refs.length)throw new ConfigurationError('replay_external_credentials_unsupported');
  for(const asset of configuration.assets){
    const file=path.resolve(project,asset.id);
    if(!file.startsWith(project+path.sep))throw new ConfigurationError('replay_asset_reference_invalid');
    let digest;try{digest=await hash(file)}catch{throw new ConfigurationError('replay_asset_missing')}
    if(digest!==asset.sha256)throw new ConfigurationError('replay_asset_hash_mismatch');
  }
  if(configuration.runtime.mode==='sherpa'){
    const {verifySherpa}=await import('../fixtures/sherpa.mjs');await verifySherpa(configuration);
  }
  if(configuration.fault&&exact&&!faultRebuilt)throw new ConfigurationError('fault_rebuild_controller_not_implemented');
  if(exact){
    for(const key of ['commit','dirty','binary_sha256','toolchain','target','profile','lock_sha256','features','rustflags','source_patch']){
      if(JSON.stringify(configuration.build[key])!==JSON.stringify(currentBuild[key]))throw new ConfigurationError('replay_build_'+key+'_mismatch');
    }
    if(configuration.build.dirty){
      const source=configuration.build.source_patch;
      if(!source||source.id!=='source-'+source.sha256)throw new ConfigurationError('replay_source_identity_invalid');
      let digest;try{digest=await hash(path.join(project,'artifacts/restricted/source-assets',source.sha256+'.json'))}catch{throw new ConfigurationError('replay_source_asset_missing')}
      if(digest!==source.sha256)throw new ConfigurationError('replay_source_asset_hash_mismatch');
    }
  }
  return {case_id:configuration.case_id,exact_replay:exact,original_build:configuration.build,current_build:currentBuild,build_changed:JSON.stringify(configuration.build)!==JSON.stringify(currentBuild)};
}
