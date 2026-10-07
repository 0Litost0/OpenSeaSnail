import { createServer } from 'node:http';
export async function provider(mode) {
    const requests = [];
    const server = createServer(async (req, res) => {
        const buffers = [];
        for await (const chunk of req)
            buffers.push(chunk);
        const body = JSON.parse(Buffer.concat(buffers).toString());
        const input = JSON.parse(body.messages[1].content);
        requests.push({ input, authenticated: !!req.headers.authorization });
        if (mode === 'timeout')
            return;
        if (mode === 'error') {
            res.writeHead(503);
            res.end('controlled failure');
            return;
        }
        res.setHeader('Content-Type', 'application/json');
        const content = mode === 'invalid' ? 'not JSON' : JSON.stringify({ cleaned_text: input.transcript + '。', corrections: [] });
        res.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content } }] }));
    });
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
    return { endpoint: `http://127.0.0.1:${server.address().port}/v1`, requests,
        close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }) };
}
