import { password } from './environment.mjs';
export async function dictionaryScenario(env, call, step, equal, injectFault) {
    await step('创建账户', async () => {
        const setup = await call('POST', '/auth/setup', { username: 'poc', password }, 201);
        env.token = setup.secret;
        env.secrets.push(setup.secret);
    });
    await step('词典增改删查', async () => {
        const added = await call('POST', '/dictionary/entries', { terms: ['SeaSnail', 'discard'] });
        const id = added.added.find(e => e.term === 'SeaSnail').id;
        const discard = added.added.find(e => e.term === 'discard').id;
        await call('PUT', `/dictionary/entries/${id}`, { term: 'SeaSnail v2' });
        await call('DELETE', `/dictionary/entries/${discard}`, undefined, 204);
        const before = await call('GET', '/dictionary');
        equal(before.items.map(e => e.term), ['SeaSnail v2']);
    });
    await step('真正重启服务', async () => {
        const pid = env.child.pid;
        await env.restart();
        equal(env.child.pid !== pid, true);
    });
    await step('重启后读取', async () => {
        const after = await call('GET', '/dictionary');
        equal(after.items.map(e => e.term), [injectFault ? 'INJECTED_WRONG_EXPECTATION' : 'SeaSnail v2']);
    });
}
