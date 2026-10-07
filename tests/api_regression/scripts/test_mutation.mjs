import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {readSource,prepareMutation} from '../cli/mutation.mjs';
import {project,sha} from '../cli/identity.mjs';
test('fault replay refuses missing base identity, changed source bytes and escaping paths before building',async()=>{
  await assert.rejects(prepareMutation('MUT-DICT-PERSIST-001',{}, {fault:{}}),error=>error.safeCode==='fault_base_source_missing');
  const snapshot={version:1,entries:[{path:'crates/../../outside',body:Buffer.from('public').toString('base64'),sha256:sha('public')}]};
  const bytes=JSON.stringify(snapshot);const digest=sha(bytes);const directory=path.join(project,'artifacts/restricted/source-assets');await fs.mkdir(directory,{recursive:true});
  const file=path.join(directory,digest+'.json');const reference={id:'source-'+digest,sha256:digest};
  try {
    await fs.writeFile(file,bytes,{mode:0o600});await assert.rejects(readSource(reference),error=>error.safeCode==='fault_source_path_invalid');
    await fs.writeFile(file,bytes+' ');await assert.rejects(readSource(reference),error=>error.safeCode==='fault_source_asset_hash_mismatch');
  }finally{await fs.rm(file,{force:true})}
});
