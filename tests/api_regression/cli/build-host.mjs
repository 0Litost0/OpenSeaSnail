import {spawnSync} from 'node:child_process';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const project=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const repository=path.resolve(project,'../..');
const result=spawnSync('cargo',['build','--offline','--locked','--manifest-path',path.join(project,'Cargo.toml')],{cwd:repository,env:{...process.env,CARGO_TARGET_DIR:path.join(repository,'target')},stdio:'inherit'});
process.exitCode=result.status??2;
