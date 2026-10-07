# 新增用例与版本化资产维护

开发与交付约定见[根 README](../../../README.md#测试与-agent-开发约定)，环境与报告说明见[测试 README](../README.md)。以下命令均从仓库根目录执行。

## 新增独立扩展用例

当前默认范围为 case-catalog.json 的 23 项、acceptance-required.v1.json 的 22 必验与 17 quick。示例使用显式 `--extension`，声明 `isolated-extension-v1`；不会加入默认 quick 或 acceptance。

1. 复制模板，例如 `cp -R tests/api_regression/examples/new-case tests/api_regression/examples/my-case`。
2. 按下表同步登记、断言和资产。
3. 执行类型检查、发现、单跑及相关 suite，确认实际执行范围与清理结果。

可复制 `examples/new-case/`，保持在 examples 的直接子目录，使 spec 的 `../../fixtures/test.js` 导入可用。修改 case ID（现有类别前缀 + 三位数字）、业务目的、前置条件、assertion、spec 文件及数据/规则。将 extension.json 中 asset_refs 改为复制后资产相对测试项目根目录的路径。每项资产都有配置 SHA，spec 和 extension.json 自动纳入配置。不要覆盖默认 ID、前置/断言登记，也不要设 required/quick=true。

| 文件 | 维护内容 |
| --- | --- |
| `extension.json` | `scope` 固定为 `isolated-extension-v1`；登记 `cases`、`preconditions`、`assertions`、`spec_files` |
| `dictionary.spec.ts` | 修改测试标题中的 ID、`@case_<ID>` tag 与业务步骤；从 `../../fixtures/test.js` 导入 `test` / `expect` |
| `terms.json` | 版本化输入数据；调整后同步 `asset_refs` |
| `rules.json` | 独立预期与规则；修改语义时更新版本，并同步断言 |

用例 ID、tag、`assertion_ref`、`design_case_id` 与前置登记引用需保持一致。`asset_refs` 相对 `tests/api_regression/`，`spec_files` 相对扩展目录；复制后务必将资产路径中的 `new-case` 改为自己的目录。使用现有受支持的 ID 类别前缀，新增类别需同步契约和校验。测试通过 `scenario.api` 建立前置状态和访问业务 API，通过 `scenario.step` 记录每个可诊断步骤；fixture 管理宿主、独立环境和 teardown。

模板的核心写法如下（输入和预期从版本化 JSON 读取）：

```ts
import fs from 'node:fs/promises';
import { test, expect } from '../../fixtures/test.js';

const data = JSON.parse(await fs.readFile(new URL('./terms.json', import.meta.url), 'utf8'));
const rules = JSON.parse(await fs.readFile(new URL('./rules.json', import.meta.url), 'utf8'));
test('DICT-003 登记的独立示例资产和规则驱动词典用例',
  { tag: '@case_DICT-003' }, async ({ scenario }) => {
    await scenario.step('写入词条并通过 API 读回', async () => {
      await scenario.api.setup('extension-fixture');
      await scenario.api.json('POST', '/dictionary/entries', { terms: data.terms });
      const result = await scenario.api.json('GET', '/dictionary');
      expect(result.items).toHaveLength(rules.expected_count);
      expect(result.items[0].term).toBe(rules.expected_term);
    });
  });
```

验证原模板时执行以下命令；验证复制目录时替换 `examples/new-case` 和 case ID：

```sh
pnpm --dir tests/api_regression typecheck
pnpm --dir tests/api_regression api-test list --json --extension examples/new-case
pnpm --dir tests/api_regression api-test run DICT-003 --extension examples/new-case
pnpm --dir tests/api_regression api-test suite dictionary --extension examples/new-case
```

`list` 显示 24 项，DICT-003 为非必验示例；`run` 只执行示例；`suite dictionary` 将示例与 DICT-001/002 组合。无需修改 CLI、fixture、配置、发现器或 implemented.mjs。完整独立登记应包含 scope、cases、preconditions、assertions、spec_files 五项；登记的目的与 assertion 的 business_promise 必须一致，文件/文档引用必须存在，发现结果与登记严格一致。无 `--extension` 时示例不被发现。

## 资产与 replay

开发期 `SYSTEM-004` 会复制模板到忽略的 examples/.isolated-<run_id>，执行上述操作并核验执行器/默认登记/独立必验快照 SHA 不变，最后销毁副本。其子报告保留真实执行结果；已删除临时示例资产的子配置不可精确 replay，不能将这种维护演示报告当作持久可执行资产。需持久 replay 时在稳定版本化目录保存扩展，使用 `api-test replay <replay.json> --extension <目录>`，保持原文件/规则/构建身份一致。

## 纳入默认与必验范围

正式加入默认范围或新增必验属于范围变更：同步需求/设计矩阵、`case-catalog.json`、`contracts/acceptance-required.v1.json`、`contracts/preconditions.v1.json`、`contracts/assertions.v1.json`、业务 spec 和 `specs/implemented.mjs`，再 review。必验快照只纳入批准的必验 ID。当前契约和校验还包含 23 个默认、22 个必验、17 个 quick 的数量约束，范围改变时应同步相关 schema、`scripts/check_contracts.py` 与文档；不能只增加 spec 或修改一个目录文件。登记后执行 `pnpm --dir tests/api_regression check:contracts`、`typecheck`、目标用例、相关组合及根 README 要求的回归。禁止在示例里偷偷改默认必验集合。版本/内容变化会改变资产 SHA；精确 replay 拒绝变化，修复构建重跑按现有 `run --config` 规则记录构建差异。

examples/gate-failures 是 SYSTEM-002 使用的独立故障演示范围，仅在显式选择后执行；其超时、首次断言失败和 canary 被真实门禁拒绝，不能把子运行失败改为通过。
