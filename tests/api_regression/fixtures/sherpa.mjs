import fs from 'node:fs/promises';
import path from 'node:path';
import {project,hash} from '../cli/identity.mjs';
import {ConfigurationError} from '../cli/replay.mjs';
export async function verifySherpa(configuration) {
  const pinned=JSON.parse(await fs.readFile(path.join(project,'assets/sherpa-baseline.v2.json'),'utf8'));
  const resources=path.resolve(project,'../../dist/SeaSnail.app/Contents/Resources');
  const root=process.env.SEASNAIL_ASR_ROOT??path.join(resources,'asr');
  const ffmpeg=process.env.FFMPEG_PATH??path.join(resources,'ffmpeg');
  const identities=pinned.bundle_files.map(file=>({id:'sherpa-bundle/'+file.path,sha256:file.sha256}));
  identities.push({id:'ffmpeg-v1',sha256:pinned.ffmpeg.sha256});
  if(JSON.stringify(configuration.runtime.artifacts)!==JSON.stringify(identities))throw new ConfigurationError('sherpa_configuration_identity_mismatch');
  for(const file of pinned.bundle_files){
    let digest;try{digest=await hash(path.join(root,'sensevoice-small/sherpa_onnx/int8',file.path))}catch{throw new ConfigurationError('sherpa_artifact_missing')}
    if(digest!==file.sha256)throw new ConfigurationError('sherpa_artifact_hash_mismatch');
  }
  let digest;try{digest=await hash(ffmpeg)}catch{throw new ConfigurationError('sherpa_ffmpeg_missing')}
  if(digest!==pinned.ffmpeg.sha256)throw new ConfigurationError('sherpa_ffmpeg_hash_mismatch');
  process.env.SEASNAIL_ASR_ROOT=root;process.env.FFMPEG_PATH=ffmpeg;
  return identities;
}
