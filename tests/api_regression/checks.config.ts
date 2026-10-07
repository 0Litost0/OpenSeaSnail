import {defineConfig} from '@playwright/test';
export default defineConfig({testDir:'./checks',testMatch:'transport.spec.ts',workers:1,retries:0,timeout:60000,use:{trace:'off'},reporter:[['list']],outputDir:'./artifacts/restricted/fixture-self-check'});
