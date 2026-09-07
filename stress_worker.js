// Web Worker for WASM-Stress Multi-Threaded Execution
import init, { run_stress_test } from './wasm_stress.js';

let wasmReady = false;

async function ensureWasm() {
  if (!wasmReady) {
    await init('./wasm_stress_bg.wasm');
    wasmReady = true;
  }
}

self.addEventListener('message', async (event) => {
  const data = event.data;
  if (!data) return;

  if (data.type === 'PING') {
    try {
      await ensureWasm();
      self.postMessage({ type: 'PONG', workerId: data.workerId });
    } catch (err) {
      self.postMessage({ type: 'ERROR', workerId: data.workerId, error: String(err) });
    }
    return;
  }

  if (data.type === 'RUN_BATCH') {
    const { taskId, workerId, config } = data;
    try {
      await ensureWasm();

      const batchResult = await run_stress_test(
        config.url,
        config.concurrency,
        config.concurrency,
        config.method,
        config.headersJson,
        config.requestBody,
        config.timeout,
        config.followRedirects,
        config.pattern,
        config.expectedStatus || 0,
        config.expectedText || '',
        config.customScript || '',
        () => {
          self.postMessage({ type: 'PROGRESS_TICK', taskId, workerId });
        },
        config.rateLimit,
        config.circuitThreshold,
        config.retryAttempts,
        config.rotateUserAgent,
        config.wafBypass,
        config.wafCacheBuster,
        config.wafXffRotation,
        config.wafRandomHeaders,
        config.rateLimitWait,
        config.retryBase,
        config.retryMax,
        config.circuitTimeout ? BigInt(config.circuitTimeout) : null,
        config.circuitWindow,
        config.circuitWindowMs ? BigInt(config.circuitWindowMs) : null,
        true,
        config.templating,
        config.thinkTime
      );

      self.postMessage({
        type: 'BATCH_COMPLETE',
        taskId,
        workerId,
        result: batchResult,
      });
    } catch (err) {
      self.postMessage({
        type: 'BATCH_FAILED',
        taskId,
        workerId,
        error: String(err?.message || err),
      });
    }
  }
});
