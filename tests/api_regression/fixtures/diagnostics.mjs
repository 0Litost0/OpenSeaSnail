import fs from 'node:fs/promises';
import path from 'node:path';
const limits=JSON.parse(await fs.readFile(new URL('../assets/budgets.v1.json',import.meta.url),'utf8')).evidence;

export function sanitizer(secrets=[],paths=[]) {
  const replacements=[...secrets,...paths].filter(value=>typeof value==='string'&&value.length>=8).sort((a,b)=>b.length-a.length);
  return value=>{
    let text=String(value??'');
    for(const secret of replacements)text=text.split(secret).join('[redacted]');
    return text.replace(/\x1b\[[0-9;]*m/g,'').replace(/ss_live_[A-Za-z0-9_-]{64}|Bearer\s+[A-Za-z0-9._~+/=-]{16,}|(?:POC|API_TEST)_[A-Z_]*CANARY[A-Za-z0-9_-]*/gi,'[redacted]');
  };
}
export function assertionDetails(error) {
  const matcher=error?.matcherResult;
  const value=item=>item===undefined?null:(typeof item==='string'?item:JSON.stringify(item))?.slice(0,4096)??null;
  return {message:String(error?.message??'validation failed').slice(0,8192),expected:value(matcher?.expected),actual:value(matcher?.actual)};
}
async function tail(file,maximum) {
  const info=await fs.lstat(file);
  if(!info.isFile()||info.isSymbolicLink())throw new Error('unsafe diagnostic file');
  const handle=await fs.open(file,'r');
  try {const bytes=Buffer.alloc(Math.min(info.size,maximum));const {bytesRead}=await handle.read(bytes,0,bytes.length,Math.max(0,info.size-bytes.length));return {text:bytes.subarray(0,bytesRead).toString(),truncated:info.size>maximum};}
  finally {await handle.close();}
}
/** @param {{env:any,raw:string,caseId:string,retry:number,errors?:any[]}} options */
export async function collectDiagnostics({env,raw,caseId,retry,errors=[]}) {
  let secrets=[];
  try{secrets=(await fs.readFile(path.join(raw,'secrets.ndjson'),'utf8')).split('\n').filter(Boolean).map(line=>JSON.parse(line))}catch(error){if(error.code!=='ENOENT')throw error}
  const clean=sanitizer(secrets,[env.home,env.root]);
  let remaining=limits.log_bytes_per_attempt;
  const bounded=text=>{const safe=clean(text);const bytes=Buffer.from(safe);const size=Math.min(bytes.length,remaining);remaining-=size;return {text:bytes.subarray(bytes.length-size).toString(),truncated:bytes.length>size}};
  const hosts=(env.diagnosticStreams??[]).map(stream=>({pid:stream.pid,exit:stream.exit?{code:stream.exit.code??null,signal:stream.exit.signal??null,error:stream.exit.error??null}:null,stderr:bounded(stream.stderr),stdout:bounded(stream.stdout)}));
  const logs=[];
  const directory=path.join(env.home,'logs');
  try {
    if((await fs.lstat(directory)).isSymbolicLink())throw new Error('unsafe diagnostic directory');
    for(const name of (await fs.readdir(directory)).filter(name=>/^backend\.log[.\w-]*$/.test(name)).sort().reverse()){
      if(!remaining)break;
      const entry=await tail(path.join(directory,name),remaining);logs.push({name,...bounded(entry.text),source_truncated:entry.truncated});
    }
  }catch(error){if(error.code!=='ENOENT')throw error}
  const details=errors.map(error=>({message:clean(error.message??''),stack:clean(error.stack??'').slice(0,8192)}));
  const reference=`environment-proof/${caseId}-${retry}-diagnostics.json`;
  await fs.mkdir(path.join(raw,'environment-proof'),{recursive:true,mode:0o700});
  await fs.writeFile(path.join(raw,reference),JSON.stringify({case_id:caseId,retry_index:retry,hosts,logs,errors:details,log_limit_bytes:limits.log_bytes_per_attempt},null,2),{mode:0o600});
  return {reference,clean};
}
