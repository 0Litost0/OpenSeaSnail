// Native Node HTTP adapter for Vitest; no Playwright runtime dependency.
async function request(url, options = {}) {
    let body;
    if (options.multipart) {
        body = new FormData();
        for (const [name, value] of Object.entries(options.multipart)) {
            if (typeof value === 'string')
                body.append(name, value);
            else
                body.append(name, new Blob([value.buffer], { type: value.mimeType }), value.name);
        }
    }
    const response = await fetch(url, { method: options.method ?? 'GET', headers: options.headers, body, signal: AbortSignal.timeout(10000) });
    return { status: () => response.status, json: () => response.json(), body: async () => Buffer.from(await response.arrayBuffer()) };
}
export const fetchClient = { get: (url, options) => request(url, options), post: (url, options) => request(url, { ...options, method: 'POST' }) };
