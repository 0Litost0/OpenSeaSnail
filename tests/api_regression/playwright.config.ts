import {defineConfig} from '@playwright/test';
import path from 'node:path';
import fs from 'node:fs/promises';
const raw=process.env.API_TEST_RAW_DIR;
const extension=process.env.API_TEST_EXTENSION;
const extra=extension?JSON.parse(await fs.readFile(path.join(extension,'extension.json'),'utf8')):null;
export default defineConfig({
  projects:[{name:'default',testDir:'./specs',testMatch:'**/*.spec.ts'},...(extension?[{name:'extension',testDir:extension,testMatch:extra.spec_files}]:[])], fullyParallel:false, workers:1,
  retries:Number(process.env.API_TEST_RETRIES??0), timeout:90_000,
  expect:{timeout:5_000}, forbidOnly:true, failOnFlakyTests:true,
  use:{trace:'off'},
  outputDir:raw?path.join(raw,'attachments'):'./artifacts/restricted/framework-attachments',
  reporter:raw?[
    ['json',{outputFile:path.join(raw,'playwright.json')}],
    ['html',{outputFolder:path.join(raw,'html'),open:'never'}],
  ]:[['list']],
});
