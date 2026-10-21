// Runs the real WiscLite engine (WebAssembly) off the main thread.
importScripts('web/pkg/wisclite_wasm.js?v=7');
const ready = wasm_bindgen({ module_or_path: 'web/pkg/wisclite_wasm_bg.wasm?v=7' });
onmessage = async ({ data: { n, valueSize } }) => {
  try {
    await ready;
    const results = [];
    for (const mode of ['baseline', 'wisckey']) {
      const db = new wasm_bindgen.WiscDb(mode, 4 << 20);
      const CH = 1000; let t0 = performance.now();
      for (let done = 0; done < n; done += CH) {
        db.put_random(Math.min(CH, n - done), valueSize);
        postMessage({ type: 'progress', mode, phase: 'put', done: Math.min(n, done + CH), total: n });
      }
      db.flush();
      const secs = (performance.now() - t0) / 1000;
      postMessage({ type: 'progress', mode, phase: 'get', done: n, total: n });
      const G = 10000; t0 = performance.now(); const hits = db.get_random(G); const getUs = (performance.now() - t0) * 1000 / G;
      const S = JSON.parse(db.stats());
      results.push({ mode, n, secs, puts_per_sec: n / secs, get_us: getUs, hits, stats: S.stats, disk_bytes: S.disk_bytes, files: JSON.parse(db.list_files()) });
      db.free();
    }
    postMessage({ type: 'done', results });
  } catch (e) { postMessage({ type: 'error', message: String(e && e.message || e) }); }
};
