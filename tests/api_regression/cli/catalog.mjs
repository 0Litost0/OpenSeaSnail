import fs from 'node:fs/promises';
import path from 'node:path';
import {project} from './identity.mjs';
import {ConfigurationError} from './replay.mjs';
export async function extensionDirectory(input) {
  const directory=path.resolve(project,input);
  if(!directory.startsWith(project+path.sep)||await fs.realpath(directory)!==directory)throw new ConfigurationError('extension_directory_invalid');
  return directory;
}
export async function loadCatalog() {
  const catalog=JSON.parse(await fs.readFile(path.join(project,'case-catalog.json'),'utf8'));
  if(process.env.API_TEST_EXTENSION){
    const directory=await extensionDirectory(process.env.API_TEST_EXTENSION);
    const extension=JSON.parse(await fs.readFile(path.join(directory,'extension.json'),'utf8'));
    for(const entry of extension.cases)entry.asset_refs=[...new Set([...entry.asset_refs,path.relative(project,path.join(directory,'extension.json')),...extension.spec_files.map(file=>path.relative(project,path.join(directory,file)))])];
    catalog.cases.push(...extension.cases);
  }
  return catalog;
}
