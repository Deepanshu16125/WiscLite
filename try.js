// WiscLite landing: "Try it" console (two toy engines) + write-amplification estimator.
(function () {
  const $ = id => document.getElementById(id);
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const fmt = (x, d = 2) => Number(+x + 1e-9).toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d });
  const kb = b => b >= 1e6 ? fmt(b / 1e6, 2) + ' MB' : b >= 1e3 ? fmt(b / 1e3, 1) + ' KB' : b + ' B';
  const enc = s => new TextEncoder().encode(s).length;

  // ---------- (a) console: the REAL engine via WebAssembly ----------
  const WASM = 'web/pkg/wisclite_wasm_bg.wasm?v=7', MEMT = 4096;
  let base = null, wisc = null, ready = false;
  function out(html, cls) { const d = document.createElement('div'); if (cls) d.className = cls; d.innerHTML = html; $('t-log').appendChild(d); $('t-log').scrollTop = 1e9; }
  function open() { if (base) { base.free(); wisc.free(); } base = new wasm_bindgen.WiscDb('baseline', MEMT); wisc = new wasm_bindgen.WiscDb('wisckey', MEMT); }
  const both = f => [f(base), f(wisc)];
  const statOf = d => JSON.parse(d.stats());
  function lane(d, name, color) {
    const S = statOf(d), s = S.stats, files = JSON.parse(d.list_files()), mx = Math.max(1, ...files.map(f => f[1]));
    const wa = s.logical_bytes ? fmt(s.write_amplification) + '×' : '–';
    let h = `<h4 style="color:${color}">${name}</h4>`;
    h += `<div class="lane"><span>memtable</span><span class="cells">${cell(s.memtable_entries + ' entries (RAM)')}</span></div>`;
    h += `<div class="lane"><span>files</span><span class="cells files" style="display:block">` + files.map(([n, l]) =>
      `<div><span class="cell" style="border:0;padding:0">${esc(n)}<span style="color:var(--muted)">${kb(l)}</span></span><div class="fbar"><i style="width:${100 * l / mx}%;background:${n.startsWith('vlog') ? 'var(--accent)' : n.endsWith('.sst') ? 'var(--contrast)' : 'var(--accent-soft)'}"></i></div></div>`).join('') + `</span></div>`;
    h += `<div class="wa">written <b style="color:${color}">${kb(s.physical_bytes)}</b> for ${kb(s.logical_bytes)} of data → write amplification <b style="color:${color}">${wa}</b><br>WAL ${kb(s.wal_bytes)} · ${s.vlog_bytes ? 'vlog ' + kb(s.vlog_bytes) + ' · ' : ''}flush ${kb(s.sst_bytes)} · compaction ${kb(s.compaction_bytes)} · ${s.flushes} flushes · ${s.compactions} compactions · ${s.sstables} sstables</div>`;
    return h;
  }
  const cell = (txt, cls) => `<span class="cell ${cls || ''}">${esc(txt)}</span>`;
  function render() { if (!ready) return; $('t-base').innerHTML = lane(base, 'Baseline: values inline', 'var(--contrast)'); $('t-wisc').innerHTML = lane(wisc, 'WiscKey: values in the vlog', 'var(--accent)'); }
  const delta = (before) => { const a = both(d => statOf(d).stats); const ev = [];
    if (a[0].compactions > before[0].compactions || a[1].compactions > before[1].compactions) ev.push('compaction');
    else if (a[0].flushes > before[0].flushes || a[1].flushes > before[1].flushes) ev.push('flush');
    return ev.length ? ` <span class="ok">→ ${ev[0] === 'compaction' ? 'a real compaction ran (see the files: sstables merged)' : 'a memtable reached 4 KiB and flushed to a real .sst file'}</span>` : ''; };
  function run(line) {
    line = line.trim(); if (!line) return;
    out(esc(line), 'c');
    if (!ready) { out('engine still loading…', 'er'); return; }
    const [cmd, ...r] = line.split(/\s+/);
    const before = both(d => statOf(d).stats);
    try {
      switch ((cmd || '').toLowerCase()) {
        case 'put': if (r.length < 2) throw 'usage: put KEY VALUE'; both(d => d.put(r[0], r.slice(1).join(' '))); out('OK' + delta(before)); break;
        case 'get': { if (!r[0]) throw 'usage: get KEY'; const [a, b] = both(d => d.get(r[0])); out(a == null ? '(not found)' : esc(a) + (b === a ? ' <span class="ok">(WiscKey followed its pointer into the vlog)</span>' : ' <span class="er">mismatch!</span>')); break; }
        case 'del': if (!r[0]) throw 'usage: del KEY'; both(d => d.del(r[0])); out('OK <span class="ok">tombstone written</span>' + delta(before)); break;
        case 'scan': { const rows = JSON.parse(base.scan(r[0] || '', r[1] || '')); out(rows.length ? rows.slice(0, 40).map(([k, v]) => esc(k) + '\t' + esc(v)).join('\n') + `\n(${rows.length} keys, in order)` : '(no keys in range)'); break; }
        case 'fill': { const n = Math.min(5000, Math.max(1, +r[0] || 50)); both(d => d.put_random(n, 1024)); out(`OK  ${n} random puts of 1 KB into each engine` + delta(before)); break; }
        case 'flush': both(d => d.flush()); out('OK <span class="ok">memtables written to .sst files, WAL truncated</span>'); break;
        case 'compact': both(d => d.compact()); out('OK <span class="ok">all sstables merged into one, tombstones dropped</span>'); break;
        case 'gc': { const [a, b] = both(d => JSON.parse(d.gc())); out(`OK  reclaimed ${kb(a.reclaimed_bytes)} (baseline) · ${kb(b.reclaimed_bytes)} (WiscKey: new vlog generation, live values only)`); break; }
        case 'crash': { const [a, b] = both(d => JSON.parse(d.crash_and_recover()));
          out(`<span class="er">⚡ crashed: engines dropped without flushing (memtables had ${a.memtable_before} / ${b.memtable_before} entries).</span>`);
          out(`<span class="ok">recovered: reopened on the same files; WAL replay rebuilt ${a.replayed_entries} / ${b.replayed_entries} memtable entries from ${kb(a.wal_bytes)} / ${kb(b.wal_bytes)} of WAL.</span>`); break; }
        case 'stats': out(esc(JSON.stringify(statOf(wisc).stats))); break;
        case 'files': out(both(d => d.list_files()).map(esc).join('\n')); break;
        case 'reset': open(); out('fresh engines'); break;
        case 'help': out('put KEY VALUE · get KEY · del KEY · scan [START] [END] · fill N (1 KB values) · flush · compact · gc · crash · stats · files · reset'); break;
        default: throw 'unknown command: ' + cmd + '. Try help';
      }
    } catch (e) { out(esc(e && e.message ? e.message : e), 'er'); }
    render();
  }
  if ($('t-form')) {
    $('t-form').addEventListener('submit', e => { e.preventDefault(); run($('t-in').value); $('t-in').value = ''; });
    document.querySelectorAll('.tchips button').forEach(b => b.addEventListener('click', () => run(b.dataset.c)));
    out('Loading the real engine…');
    if (typeof wasm_bindgen === 'undefined') { $('t-status').textContent = 'Could not load the WebAssembly engine.'; $('t-status').classList.add('err'); }
    else wasm_bindgen({ module_or_path: WASM }).then(() => {
      open(); ready = true; render();
      $('t-status').textContent = 'This is the actual WiscLite engine compiled to WebAssembly · loaded';
      out('Ready: two real engines. Type <b>help</b>, or try <b>fill 50</b> then <b>crash</b>.');
    }).catch(e => { $('t-status').textContent = 'WebAssembly failed to load: ' + e; $('t-status').classList.add('err'); });
  }

  // ---------- in-browser benchmark (Web Worker) ----------
  if ($('bgo')) {
    $('bgo').addEventListener('click', () => {
      const n = +$('bn').value; $('bgo').disabled = true; $('bout').innerHTML = ''; $('bprog').style.width = '0%';
      $('bstat').textContent = 'starting worker…';
      const w = new Worker('bench-worker.js?v=7');
      w.onmessage = ({ data: m }) => {
        if (m.type === 'progress') { $('bprog').style.width = (100 * m.done / m.total) + '%'; $('bstat').textContent = `${m.mode}: ${m.phase} ${m.done.toLocaleString()} / ${m.total.toLocaleString()}`; }
        else if (m.type === 'done') { $('bgo').disabled = false; $('bstat').textContent = 'done, measured on your machine'; $('bprog').style.width = '100%'; drawBench(m.results); w.terminate(); }
        else if (m.type === 'error') { $('bgo').disabled = false; $('bstat').textContent = 'error: ' + m.message; w.terminate(); }
      };
      w.onerror = e => { $('bgo').disabled = false; $('bstat').textContent = 'worker error: ' + e.message; };
      w.postMessage({ n, valueSize: 1024 });
    });
  }
  function drawBench(R) {
    const b = R.find(r => r.mode === 'baseline'), w = R.find(r => r.mode === 'wisckey');
    const max = Math.max(b.stats.write_amplification, w.stats.write_amplification) * 1.08;
    const row = (r, c) => { const s = r.stats, L = s.logical_bytes; return `<div class="row"><div class="name">${r.mode}</div><div class="bar">` +
      [[s.wal_bytes, 'var(--accent-soft)'], [s.vlog_bytes, 'var(--accent)'], [s.sst_bytes, 'var(--contrast)'], [s.compaction_bytes, 'var(--accent-deep)']].map(([v, col]) => `<i style="width:${100 * v / L / max}%;background:${col}"></i>`).join('') +
      `<b class="tick" style="left:${100 / max}%"></b></div><div class="val" style="color:${c}">${fmt(s.write_amplification, 3)}× WA</div></div>`; };
    const tm = Math.max(b.puts_per_sec, w.puts_per_sec) * 1.1;
    const trow = (r, c) => `<div class="row"><div class="name">${r.mode} put</div><div class="bar"><i style="width:${100 * r.puts_per_sec / tm}%;background:${c}"></i></div><div class="val">${fmt(r.puts_per_sec / 1000, 1)}k ops/s</div></div>`;
    $('bout').innerHTML = `<div class="chart">${row(b, 'var(--contrast)')}${row(w, 'var(--accent)')}</div>
      <div class="legend"><span><i style="background:var(--accent-soft)"></i>WAL</span><span><i style="background:var(--accent)"></i>vlog</span><span><i style="background:var(--contrast)"></i>flush</span><span><i style="background:var(--accent-deep)"></i>compaction</span></div>
      <div class="chart">${trow(b, 'var(--contrast)')}${trow(w, 'var(--accent)')}</div>
      <div class="kpis">${[[fmt(b.stats.write_amplification / w.stats.write_amplification) + '×', 'less write amplification'], [b.stats.compactions + ' → ' + w.stats.compactions, 'compactions'], [fmt(b.get_us, 1) + ' / ' + fmt(w.get_us, 1) + ' µs', 'mean get (baseline / WiscKey)'], [kb(b.disk_bytes) + ' / ' + kb(w.disk_bytes), 'files in memory']].map(k => `<div class="kpi"><b>${k[0]}</b><span>${k[1]}</span></div>`).join('')}</div>
      <p class="note">${b.n.toLocaleString()} puts × 1 KB per engine · ${fmt(b.secs + w.secs, 1)} s total · real engine in WebAssembly in a Web Worker on your device · ${esc(navigator.userAgent.split(') ')[0].replace('Mozilla/5.0 (', ''))}</p>`;
  }

  // ---------- (b) estimator ----------
  const MEMB = 4 << 20, K = 16;
  function est(V, N) {
    const lg = K + V, rew = t => Math.max(0, Math.log(t / MEMB) / Math.log(4) - 1);
    const bWal = 8 + 4 + K + 5 + V, bSst = (4 + K + 5 + V) + (4 + K + 8), bTree = N * bSst, bR = rew(bTree);
    const wWal = 8 + 4 + K + 13, wV = 8 + V, wSst = (4 + K + 13) + (4 + K + 8), wTree = N * wSst, wR = rew(wTree);
    return {
      base: { wal: bWal / lg, vlog: 0, flush: bSst / lg, cmp: bSst * bR / lg, rew: bR, tree: bTree },
      wisc: { wal: wWal / lg, vlog: wV / lg, flush: wSst / lg, cmp: wSst * wR / lg, rew: wR, tree: wTree }, logical: N * lg
    };
  }
  function draw() {
    const x = +$('e-vs').value, V = Math.round(Math.pow(2, x)), N = Math.round(Math.pow(10, +$('e-n').value));
    const label = V >= 1024 ? fmt(V / 1024, V % 1024 ? 1 : 0) + ' KB' : V + ' B';
    $('e-vs-big').textContent = label; $('e-vs').setAttribute('aria-valuetext', label);
    $('e-vs').style.setProperty('--p', (100 * (x - 4) / 10) + '%');
    $('e-n-l').textContent = N.toLocaleString();
    const e = est(V, N), tot = y => y.wal + y.vlog + y.flush + y.cmp, tb = tot(e.base), tw = tot(e.wisc), max = Math.max(tb, tw, 1.2) * 1.05;
    $('e-bb').style.width = (100 * tb / max) + '%'; $('e-bw').style.width = (100 * tw / max) + '%';
    $('e-bv').textContent = fmt(tb) + '×'; $('e-wv').textContent = fmt(tw) + '×';
    // 1.0x line, positioned inside the bar track column
    const track = $('e-bb').parentElement, box = track.parentElement.parentElement.getBoundingClientRect(), tr = track.getBoundingClientRect();
    $('e-one').style.left = (tr.left - box.left + tr.width / max) + 'px';
    const win = tb / tw;
    $('e-gap').textContent = win >= 1 ? fmt(win, 1) + '× less writing' : fmt(1 / win, 1) + '× more writing';
    $('e-gap').classList.toggle('neg', win < 1);
    $('e-gapsub').textContent = win >= 1 ? 'with WiscKey (estimate)' : 'with WiscKey: values this small cost more to separate than they save';
    $('e-kpis').innerHTML = [[fmt(tb) + '×', 'baseline WA (estimate)'], [fmt(tw) + '×', 'WiscKey WA (estimate)'], [kb(e.base.tree) + ' → ' + kb(e.wisc.tree), 'LSM tree size'], [fmt(e.base.rew, 1) + '× vs ' + fmt(e.wisc.rew, 1) + '×', 'compaction rewrites per tree byte']]
      .map(k => `<div class="kpi"><b>${k[0]}</b><span>${k[1]}</span></div>`).join('');
    $('e-note').textContent = V < 64 ? 'With tiny values the 13-byte pointer and the vlog record header outweigh the savings, so WiscKey only pays off once values are larger than the key and pointer. The paper reports the same effect.' : '';
  }
  if ($('e-vs')) { ['input', 'change'].forEach(ev => { $('e-vs').addEventListener(ev, draw); $('e-n').addEventListener(ev, draw); }); window.addEventListener('resize', draw); draw(); }
})();
