import fs from 'node:fs/promises';
import {test,expect} from '../../fixtures/test.js';
const data=JSON.parse(await fs.readFile(new URL('./terms.json',import.meta.url),'utf8'));
const rules=JSON.parse(await fs.readFile(new URL('./rules.json',import.meta.url),'utf8'));
test('DICT-003 登记的独立示例资产和规则驱动词典用例',{tag:'@case_DICT-003'},async({scenario})=>{
  await scenario.step('通过真实 API 写入示例词条并按独立规则读回',async()=>{
    await scenario.api.setup('extension-fixture');await scenario.api.json('POST','/dictionary/entries',{terms:data.terms});
    const result=await scenario.api.json('GET','/dictionary');
    expect(result.items).toHaveLength(rules.expected_count);expect(result.items[0].term).toBe(rules.expected_term);
  });
});
