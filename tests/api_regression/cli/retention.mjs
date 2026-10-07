import fs from 'node:fs/promises';
import path from 'node:path';
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
// Only complete published evidence is aged out here. Active homes and recovery
// manifests belong to the environment owner; restricted replay assets stay local.
export async function pruneEvidence(project,now=Date.now()) {
  const budget=JSON.parse(await fs.readFile(path.join(project,'assets/budgets.v1.json'),'utf8'));
  const root=path.join(project,'artifacts/reports');
  let entries;try{entries=await fs.readdir(root,{withFileTypes:true})}catch(error){if(error.code==='ENOENT')return [];throw error}
  if((await fs.lstat(root)).isSymbolicLink())throw new Error('report root must be a directory');
  const removed=[];
  for(const entry of entries){
    if(!entry.isDirectory()||!uuid.test(entry.name))continue;
    const directory=path.join(root,entry.name);
    if(now-(await fs.stat(directory)).mtimeMs<=budget.evidence.retention_days*86400000)continue;
    let result;try{result=JSON.parse(await fs.readFile(path.join(directory,'result.json'),'utf8'))}catch{continue}
    if(result.run_id!==entry.name||result.schema_version!==1)continue;
    await fs.rm(directory,{recursive:true});removed.push(entry.name);
  }
  return removed;
}
