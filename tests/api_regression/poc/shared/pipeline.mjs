import { fetchJson, password, providerSecret } from './environment.mjs';
import { provider } from './provider.mjs';
import { syntheticWav } from './audio.mjs';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
export async function cleanupScenario({ env, client, expect, step, poll, attach }, mode) {
    const upstream = await provider(mode);
    try {
        await step('经 API 配置账户、词典和 provider', async () => {
            const setup = await fetchJson(env, 'POST', '/auth/setup', { username: 'poc', password }, 201);
            env.token = setup.secret;
            env.secrets.push(setup.secret);
            await fetchJson(env, 'POST', '/dictionary/entries', { terms: ['SeaSnail'] });
            const config = await fetchJson(env, 'POST', '/reasoning/provider-configs', { name: 'PoC', provider_type: 'openai_compatible_self_hosted_private', endpoint: upstream.endpoint, model: 'poc' }, 201);
            await fetchJson(env, 'PUT', `/internal/reasoning/provider-configs/${config.id}/credential`, { mode: 'credential', credential: providerSecret });
            await fetchJson(env, 'PUT', '/cleanup/settings', { enabled: true, selected_provider_config_id: config.id, custom_prompt: null });
        });
        let id;
        const wav = syntheticWav();
        await step('上传规范 WAV 并等待业务终态', async () => {
            const response = await client.post(`${env.base}/sessions`, { headers: { Authorization: `Bearer ${env.token}` }, multipart: { source: 'realtime', language: 'zh', audio: { name: 'synthetic.wav', mimeType: 'audio/wav', buffer: wav } } });
            expect(response.status()).toBe(202);
            id = (await response.json()).id;
            await poll(async () => (await fetchJson(env, 'GET', `/sessions/${id}`)).status, 'completed', 45000);
        });
        await step('检查原文、最终文本、词典参与与回退', async () => {
            const raw = await fetchJson(env, 'GET', `/sessions/${id}`);
            expect(raw.transcript.full_text).toBe('你好世界');
            const detail = await fetchJson(env, 'GET', `/sessions/${id}/workspace-detail`);
            expect(detail.cleanup_status).toBe(mode === 'success' ? 'succeeded' : 'failed');
            expect(detail.final_text).toBe(mode === 'success' ? '你好世界。' : '你好世界');
            if (mode === 'timeout')
                expect(detail.cleanup_error_code).toBe('cleanup_timeout');
            if (mode !== 'success')
                expect(detail.cleanup_error_code).toBeTruthy();
            expect(upstream.requests).toHaveLength(1);
            expect(upstream.requests[0].input.dictionary_terms).toContain('SeaSnail');
            expect(upstream.requests[0].authenticated).toBe(true);
            const audio = await client.get(`${env.base}/sessions/${id}/audio`, { headers: { Authorization: `Bearer ${env.token}` } });
            expect(audio.status()).toBe(200);
            expect(await audio.body()).toEqual(wav);
            await attach('business-result', { body: JSON.stringify({ mode, cleanup_status: detail.cleanup_status, error_code: detail.cleanup_error_code }), contentType: 'application/json' });
        });
        await step('真正重启后读取最终文本', async () => {
            await env.restart();
            const after = await fetchJson(env, 'GET', `/sessions/${id}/workspace-detail`);
            expect(after.final_text).toBe(mode === 'success' ? '你好世界。' : '你好世界');
        });
    }
    finally {
        await upstream.close();
    }
}
export async function sherpaScenario({ env, client, expect, poll, attach }) {
    expect(process.env.POC_RUNTIME, 'explicit real runtime required').toBe('sherpa');
    const data = JSON.parse(await readFile(new URL('../data/speech.json', import.meta.url), 'utf8'));
    const wav = await readFile(new URL(data.audio_path, import.meta.url));
    expect(createHash('sha256').update(wav).digest('hex')).toBe(data.sha256);
    const setup = await fetchJson(env, 'POST', '/auth/setup', { username: 'poc', password }, 201);
    env.token = setup.secret;
    env.secrets.push(setup.secret);
    const models = await fetchJson(env, 'GET', '/models');
    expect(models.some(m => m.id === 'sensevoice-small-sherpa-int8' && m.status === 'active' && m.runtime === 'sherpa_onnx')).toBe(true);
    const response = await client.post(`${env.base}/sessions`, { headers: { Authorization: `Bearer ${env.token}` }, multipart: { source: 'imported', language: 'en', audio: { name: 'speech.wav', mimeType: 'audio/wav', buffer: wav } } });
    expect(response.status()).toBe(202);
    const id = (await response.json()).id;
    await poll(async () => (await fetchJson(env, 'GET', `/sessions/${id}`)).status, 'completed', 60000);
    const result = await fetchJson(env, 'GET', `/sessions/${id}`);
    const normalized = result.transcript.full_text.toLowerCase().replace(/[^a-z ]/g, '').replace(/\s+/g, ' ').trim();
    for (const phrase of data.required_phrases)
        expect(normalized).toContain(phrase);
    await attach('quality-sample', { body: JSON.stringify({ sha256: data.sha256, reference: data.reference, actual: result.transcript.full_text, scope: 'PoC sample only; not release quality acceptance' }), contentType: 'application/json' });
    await env.restart();
    const restored = await fetchJson(env, 'GET', `/sessions/${id}`);
    expect(restored.transcript.full_text).toBe(result.transcript.full_text);
}
