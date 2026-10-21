// WiscLite landing: "Try it" console (two toy engines) + write-amplification estimator.
(function () {
  const $ = id => document.getElementById(id);
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const fmt = (x, d = 2) => Number(+x + 1e-9).toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d });
  const kb = b => b >= 1e6 ? fmt(b / 1e6, 2) + ' MB' : b >= 1e3 ? fmt(b / 1e3, 1) + ' KB' : b + ' B';
  const enc = s => new TextEncoder().encode(s).length;

  // ---------- (a) console ----------
  const MEM = 4, FANIN = 4;
  let logical = 0, eng;
  const mk = inline => ({ inline, wal: [], mem: new Map(), ssts: [], vlog: [], b: { wal: 0, vlog: 0, flush: 0, compact: 0 }, fresh: null });
  const entryLen = (e, K) => e.t === 'del' ? 1 : e.t === 'ptr' ? 13 : 5 + enc(e.v);
  const sstLen = (m) => { let n = 0; m.forEach((e, k) => { const K = enc(k); n += 4 + K + entryLen(e, K) + 4 + K + 8; }); return n + 24; };
  function write(E, k, e) {
    const K = enc(k);
    E.wal.push([k, e]); E.b.wal += 8 + 4 + K + entryLen(e, K);
    E.mem.delete(k); E.mem.set(k, e); E.fresh = k;
    let ev = '';
    if (E.mem.size >= MEM) {
      const m = new Map([...E.mem].sort((a, b) => a[0] < b[0] ? -1 : 1));
      E.b.flush += sstLen(m); E.ssts.push(m); E.mem = new Map(); E.wal = []; ev = 'flush';
      if (E.ssts.length >= FANIN) {
        const all = new Map(); E.ssts.forEach(t => t.forEach((v, kk) => all.set(kk, v)));
        [...all].forEach(([kk, v]) => { if (v.t === 'del') all.delete(kk); }); // merged down to the oldest table: drop tombstones
        const m2 = new Map([...all].sort((a, b) => a[0] < b[0] ? -1 : 1));
        E.b.compact += sstLen(m2); E.ssts = m2.size ? [m2] : []; ev = 'compact';
      }
    }
    return ev;
  }
  function lookup(E, k) {
    let e = E.mem.get(k);
    if (!e) for (let i = E.ssts.length - 1; i >= 0 && !e; i--) e = E.ssts[i].get(k);
    if (!e || e.t === 'del') return null;
    return e.t === 'ptr' ? E.vlog[e.i].v : e.v;
  }
  function put(k, v) {
    logical += enc(k) + enc(v);
    const a = write(eng.base, k, { t: 'inl', v });
    eng.wisc.vlog.push({ k, v }); eng.wisc.b.vlog += 8 + enc(v);
    write(eng.wisc, k, { t: 'ptr', i: eng.wisc.vlog.length - 1 });
    return a;
  }
  function del(k) { logical += enc(k); const a = write(eng.base, k, { t: 'del' }); write(eng.wisc, k, { t: 'del' }); return a; }
  function scan(a, z) {
    const keys = new Set();
    [eng.base.mem, ...eng.base.ssts].forEach(m => m.forEach((_, k) => keys.add(k)));
    return [...keys].filter(k => k >= a && (!z || k < z)).sort().map(k => [k, lookup(eng.base, k)]).filter(x => x[1] !== null);
  }
  function out(html, cls) { const d = document.createElement('div'); if (cls) d.className = cls; d.innerHTML = html; $('t-log').appendChild(d); $('t-log').scrollTop = 1e9; }
  const evMsg = ev => ev === 'compact' ? ' <span class="ok">→ memtable flushed, 4 sstables compacted (baseline rewrote every value, WiscKey only pointers)</span>' : ev === 'flush' ? ' <span class="ok">→ memtable full: flushed to an sstable, WAL truncated</span>' : '';
  function run(line) {
    line = line.trim(); if (!line) return;
    out(esc(line), 'c');
    const [cmd, ...r] = line.split(/\s+/);
    try {
      switch ((cmd || '').toLowerCase()) {
        case 'put': {
          if (r.length < 2) throw 'usage: put KEY VALUE';
          const ev = put(r[0], r.slice(1).join(' '));
          out('OK  <span class="ok">baseline: value → WAL + memtable · WiscKey: value → vlog@' + (eng.wisc.vlog.length - 1) + ', 13-byte pointer → memtable</span>' + evMsg(ev)); break;
        }
        case 'get': { if (!r[0]) throw 'usage: get KEY'; const v = lookup(eng.base, r[0]), w = lookup(eng.wisc, r[0]); out(v === null ? '(not found)' : esc(v) + (w === v ? '  <span class="ok">(WiscKey followed the pointer into the vlog)</span>' : '')); break; }
        case 'del': { if (!r[0]) throw 'usage: del KEY'; const ev = del(r[0]); out('OK  <span class="ok">tombstone written; it hides older versions until compaction drops it</span>' + evMsg(ev)); break; }
        case 'scan': { const rows = scan(r[0] || '', r[1]); out(rows.length ? rows.map(([k, v]) => esc(k) + '\t' + esc(short(v))).join('\n') + `\n(${rows.length} keys, in order)` : '(no keys in range)'); break; }
        case 'fill': {
          const n = Math.min(64, Math.max(1, +r[0] || 8)); let ev = '';
          for (let i = 0; i < n; i++) { const e = put('k' + String(Math.floor(Math.random() * 900) + 100), 'x'.repeat(1024)); if (e) ev = e === 'compact' || !ev ? e : ev; }
          out(`OK  put ${n} random keys with 1 KB values` + evMsg(ev)); break;
        }
        case 'crash': {
          const nb = eng.base.wal.length, nw = eng.wisc.wal.length;
          eng.base.mem = new Map(); eng.wisc.mem = new Map(); render();
          out('<span class="er">⚡ power lost: both memtables are gone.</span>');
          setTimeout(() => {
            [eng.base, eng.wisc].forEach(E => E.wal.forEach(([k, e]) => { E.mem.delete(k); E.mem.set(k, e); }));
            out(`<span class="ok">recovered: replayed ${nb} WAL records (baseline) and ${nw} (WiscKey); the memtables are rebuilt and no acknowledged write was lost.</span>`);
            render();
          }, 700);
          return;
        }
        case 'reset': reset(); out('fresh stores'); return;
        case 'help': out('put KEY VALUE · get KEY · del KEY · scan [START] [END] · fill N (1 KB values) · crash · reset'); break;
        default: throw 'unknown command: ' + cmd + '. Try help';
      }
    } catch (e) { out(esc(e), 'er'); }
    render();
  }
  const cell = (txt, cls) => `<span class="cell ${cls || ''}">${esc(txt)}</span>`;
  const short = v => v.length > 10 ? v.slice(0, 8) + '…' + (enc(v) >= 1000 ? '(' + kb(enc(v)) + ')' : '') : v;
  function lane(E, name, color) {
    const label = (k, e) => e.t === 'del' ? k : e.t === 'ptr' ? k + '→@' + e.i : k + '=' + short(e.v);
    const phys = E.b.wal + E.b.vlog + E.b.flush + E.b.compact;
    let h = `<h4 style="color:${color}">${name}</h4>`;
    h += `<div class="lane"><span>WAL</span><span class="cells">${cell(E.wal.length + ' records')}</span></div>`;
    h += `<div class="lane"><span>memtable</span><span class="cells">${[...E.mem].map(([k, e]) => cell(label(k, e), (e.t === 'del' ? 'del ' : '') + (k === E.fresh ? 'new' : ''))).join('') || cell('(empty)')}</span></div>`;
    E.ssts.slice().reverse().forEach((m, i) => { h += `<div class="lane"><span>sst ${E.ssts.length - i}</span><span class="cells">${[...m].slice(0, 10).map(([k, e]) => cell(label(k, e), e.t === 'del' ? 'del' : '')).join('')}${m.size > 10 ? cell('+' + (m.size - 10)) : ''}</span></div>`; });
    if (!E.inline) h += `<div class="lane"><span>vlog</span><span class="cells">${E.vlog.slice(-12).map((x, j) => cell('@' + (E.vlog.length - Math.min(12, E.vlog.length) + j) + ' ' + short(x.v))).join('') || cell('(empty)')}${E.vlog.length > 12 ? cell('…' + E.vlog.length + ' values') : ''}</span></div>`;
    h += `<div class="wa">written <b style="color:${color}">${kb(phys)}</b> for ${kb(logical)} of data → write amplification <b style="color:${color}">${logical ? fmt(phys / logical) + '×' : '–'}</b><br>WAL ${kb(E.b.wal)} · ${E.inline ? '' : 'vlog ' + kb(E.b.vlog) + ' · '}flush ${kb(E.b.flush)} · compaction ${kb(E.b.compact)}</div>`;
    return h;
  }
  function render() { $('t-base').innerHTML = lane(eng.base, 'Baseline: values inline', 'var(--contrast)'); $('t-wisc').innerHTML = lane(eng.wisc, 'WiscKey: values in the vlog', 'var(--accent)'); }
  function reset() { logical = 0; eng = { base: mk(true), wisc: mk(false) }; render(); }
  if ($('t-form')) {
    reset();
    out('Type <span class="c" style="display:inline">help</span>, or click a chip. Tip: <b>fill 12</b> writes 1 KB values; tiny values show why WiscKey pays off only for larger values.');
    $('t-form').addEventListener('submit', e => { e.preventDefault(); run($('t-in').value); $('t-in').value = ''; });
    document.querySelectorAll('.tchips button').forEach(b => b.addEventListener('click', () => run(b.dataset.c)));
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
    const V = Math.round(Math.pow(2, +$('e-vs').value)), N = Math.round(Math.pow(10, +$('e-n').value));
    $('e-vs-l').textContent = kb(V); $('e-n-l').textContent = N.toLocaleString();
    const e = est(V, N), tot = x => x.wal + x.vlog + x.flush + x.cmp, tb = tot(e.base), tw = tot(e.wisc), max = Math.max(tb, tw, 1) * 1.08;
    const row = (name, x, c) => `<div class="row"><div class="name">${name}</div><div class="bar">` +
      [[x.wal, 'var(--accent-soft)'], [x.vlog, 'var(--accent)'], [x.flush, 'var(--contrast)'], [x.cmp, 'var(--accent-deep)']].map(([v, col]) => `<i style="width:${100 * v / max}%;background:${col}"></i>`).join('') +
      `<b class="tick" style="left:${100 / max}%"></b></div><div class="val" style="color:${c}">≈ ${fmt(tot(x))}×</div></div>`;
    $('e-bars').innerHTML = row('Baseline', e.base, 'var(--contrast)') + row('WiscKey', e.wisc, 'var(--accent)') +
      '<div class="legend"><span><i style="background:var(--accent-soft)"></i>WAL</span><span><i style="background:var(--accent)"></i>vlog</span><span><i style="background:var(--contrast)"></i>flush</span><span><i style="background:var(--accent-deep)"></i>compaction</span><span>dashed line = 1.0× (the data itself)</span></div>';
    const win = tb / tw;
    $('e-kpis').innerHTML = [[fmt(tb) + '×', 'baseline WA (estimate)'], [fmt(tw) + '×', 'WiscKey WA (estimate)'], [(win >= 1 ? fmt(win) + '× less' : fmt(1 / win) + '× MORE'), 'WiscKey vs baseline'], [kb(e.base.tree) + ' → ' + kb(e.wisc.tree), 'LSM tree size']]
      .map(k => `<div class="kpi"><b>${k[0]}</b><span>${k[1]}</span></div>`).join('');
    $('e-note').textContent = V < 64 ? 'With tiny values the 13-byte pointer and the extra vlog header cost more than they save, so WiscKey only pays off once values are larger than the key and pointer. The paper reports the same effect.'
      : `Compaction rewrites each tree byte ≈ ${fmt(e.base.rew, 1)}× in the baseline vs ${fmt(e.wisc.rew, 1)}× for WiscKey's tiny pointer tree. This is an estimate; see the dashboard for measured runs.`;
  }
  if ($('e-vs')) { $('e-vs').addEventListener('input', draw); $('e-n').addEventListener('input', draw); draw(); }
})();
