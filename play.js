// WiscLite landing page: live KPIs from bench.json + a toy "play a put" simulation.
(function () {
  const $ = id => document.getElementById(id);
  const f = (x, d = 2) => Number(+x + 1e-9).toLocaleString(undefined, { maximumFractionDigits: d, minimumFractionDigits: d });
  const css = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();

  // ---------- real numbers ----------
  function kpis(B) {
    const b = B.results.find(r => r.mode === 'Baseline'), w = B.results.find(r => r.mode === 'WiscKey'), L = r => r.stats.logical_bytes;
    if (!$('kpis')) return;
    $('h-wa').textContent = f(w.stats.write_amplification) + '×';
    $('h-wab').textContent = f(b.stats.write_amplification) + '×';
    $('h-tree').textContent = f(b.stats.sstable_disk_bytes / w.stats.sstable_disk_bytes, 1) + '×';
    const S = B.sweep || [], big = Math.max(0, ...S.map(p => p.ops));
    const pb = S.find(p => p.ops === big && p.mode === 'Baseline'), pw = S.find(p => p.ops === big && p.mode === 'WiscKey');
    if (pb && pw) {
      $('h-big').textContent = f(pb.write_amplification) + '× → ' + f(pw.write_amplification) + '×';
      $('h-big').nextElementSibling.textContent = 'WA at ' + (big / 1000) + 'k puts';
      $('real-k').textContent = `Measured, not modelled: at ${big.toLocaleString()} puts the baseline ran ${pb.compactions} compactions and rewrote ${f(pb.compaction_bytes / 1e6, 0)} MB; WiscKey ran ${pw.compactions} and rewrote ${f(pw.compaction_bytes / 1e6, 0)} MB.`;
    }
    $('kpis').innerHTML = [
      [f(w.stats.write_amplification, 3) + '×', 'WiscKey write amplification'],
      [f(b.stats.write_amplification, 3) + '×', 'baseline write amplification'],
      [b.stats.compactions + ' → ' + w.stats.compactions, 'compactions'],
      [f(b.puts_per_sec / 1000, 0) + 'k → ' + f(w.puts_per_sec / 1000, 0) + 'k', 'puts per second']]
      .map(k => `<div class="kpi"><b>${k[0]}</b><span>${k[1]}</span></div>`).join('');
    const max = Math.max(b.stats.write_amplification, w.stats.write_amplification) * 1.08;
    $('wa').innerHTML = [b, w].map((r, ri) => {
      const parts = [[r.stats.wal_bytes, '--accent-soft'], [r.stats.vlog_bytes, '--accent'], [r.stats.sst_bytes, '--contrast'], [r.stats.compaction_bytes, '--accent-deep']];
      return `<div class="row"><div class="name">${r.mode}</div><div class="bar">${parts.map(([v, c]) => `<i style="width:${100 * v / L(r) / max}%;background:var(${c})"></i>`).join('')}<b class="tick" style="left:${100 / max}%">${ri === 0 ? '<span>1.0× data</span>' : ''}</b></div><div class="val">${f(r.stats.write_amplification, 3)}×</div></div>`;
    }).join('');
    $('method').textContent = `${b.ops.toLocaleString()} random puts of ${b.value_size} B per mode · ${B.memtable_bytes / 1048576} MiB memtable · size-tiered compaction, fan-in ${B.compaction_fanin} · measured on a laptop-class machine; byte counts are hardware-independent.`;
  }
  fetch('bench.json').then(r => { if (!r.ok) throw 0; return r.json(); }).then(kpis).catch(() => { if (window.EMBEDDED || typeof EMBEDDED !== 'undefined') kpis(EMBEDDED); });

  // ---------- simulation ----------
  const KEY = 16, VAL = 1024, PTR = 21, HDR = 8, MEM = 8, FANIN = 4, NKEYS = 200;
  let st, timer = null;
  function lane(inline) {
    return { inline, mem: [], tables: [], wal: 0, vlog: 0, flush: 0, compact: 0, compactions: 0, last: null };
  }
  function reset() {
    st = { i: 0, base: lane(true), wisc: lane(false), keys: [] };
    let x = 7; for (let i = 0; i < NKEYS; i++) { x = (x * 1103515245 + 12345) % 2147483648; st.keys.push('k' + (x % 1000).toString().padStart(3, '0')); }
    $('caption').textContent = 'Press Step to put the first key.'; draw();
  }
  const entry = l => KEY + 4 + (l.inline ? 5 + VAL : 13);
  function putLane(l, k) {
    let ev = [];
    if (!l.inline) { l.vlog += HDR + VAL; }
    l.wal += HDR + 4 + entry(l);
    const ix = l.mem.indexOf(k); if (ix >= 0) l.mem.splice(ix, 1); l.mem.push(k);
    l.last = 'put';
    if (l.mem.length >= MEM) {
      const bytes = l.mem.length * entry(l); l.flush += bytes; l.tables.push({ n: l.mem.length, bytes, tier: 0, keys: new Set(l.mem) }); l.mem = []; l.wal = l.wal; ev.push('flush'); l.last = 'flush';
      for (;;) { // size-tiered: merge the FANIN newest tables when they share a tier
        const n = l.tables.length; if (n < FANIN) break;
        const run = l.tables.slice(n - FANIN); if (!run.every(t => t.tier === run[0].tier)) break;
        const keys = new Set(); run.forEach(t => t.keys.forEach(x => keys.add(x)));
        const bytes = keys.size * entry(l); l.compact += bytes; l.compactions++;
        l.tables.splice(n - FANIN, FANIN, { n: keys.size, bytes, tier: run[0].tier + 1, keys, fresh: true }); ev.push('compact'); l.last = 'compact';
      }
    }
    return ev;
  }
  function step() {
    if (st.i >= NKEYS) { stop(); $('caption').textContent = `Done: ${NKEYS} puts. Baseline rewrote ${f(st.base.compact / 1e3, 0)} KB in compaction; WiscKey ${f(st.wisc.compact / 1e3, 1)} KB.`; return; }
    st.base.tables.forEach(t => t.fresh = false); st.wisc.tables.forEach(t => t.fresh = false);
    const k = st.keys[st.i++], eb = putLane(st.base, k), ew = putLane(st.wisc, k);
    let c = `put ${k}: baseline logs the 1 KB value and keeps it in the memtable. WiscKey appends it to the vlog and keeps a 21-byte pointer.`;
    if (eb.includes('compact')) c = `Compaction! Baseline merges ${FANIN} sstables and rewrites every 1 KB value inside them. WiscKey's merge moves only pointers.`;
    else if (eb.includes('flush')) c = `Memtable full (8 entries) → flush. Baseline writes ${f(MEM * entry(st.base) / 1e3, 1)} KB to a new sstable; WiscKey writes ${f(MEM * entry(st.wisc), 0)} B.`;
    $('caption').textContent = c; draw();
  }
  function stop() { clearInterval(timer); timer = null; $('b-play').textContent = 'Play'; $('b-play').classList.remove('on'); }
  function play() { if (timer) return stop(); $('b-play').textContent = 'Pause'; $('b-play').classList.add('on'); timer = setInterval(step, 1000 / +$('b-speed').value); }

  function draw() {
    const svg = $('sim-svg'); if (!svg) return;
    const logical = st.i * (KEY + VAL), cols = [[st.base, 20, 'Baseline: values inline', css('--contrast')], [st.wisc, 510, 'WiscKey: values in the vlog', css('--accent')]];
    let g = '';
    const maxBytes = Math.max(1, ...[st.base, st.wisc].flatMap(l => l.tables.map(t => t.bytes)));
    cols.forEach(([l, x, title, c]) => {
      g += `<text x="${x}" y="18" style="font-weight:600;fill:${css('--text')}">${title}</text>`;
      g += `<text x="${x}" y="46">memtable</text>`;
      for (let s = 0; s < MEM; s++) {
        const filled = s < l.mem.length, w = l.inline ? 40 : 10;
        g += `<rect x="${x + 70 + s * 46}" y="32" width="${w}" height="20" fill="${filled ? c : 'none'}" stroke="${css('--rule')}" opacity="${filled ? (s === l.mem.length - 1 && l.last === 'put' ? 1 : .55) : 1}"/>`;
      }
      g += `<text x="${x}" y="80">sstables (${l.tables.length})</text>`;
      l.tables.slice(-9).forEach((t, j) => {
        const w = Math.max(3, 290 * Math.sqrt(t.bytes / maxBytes));
        g += `<rect x="${x + 70}" y="${90 + j * 22}" width="${w}" height="16" fill="${c}" opacity="${t.fresh ? 1 : .6 + .1 * t.tier}" ${t.fresh ? `stroke="${css('--text')}"` : ''}/><text x="${x + 76 + w}" y="${102 + j * 22}">tier ${t.tier} · ${t.bytes >= 1e3 ? f(t.bytes / 1e3, 1) + ' KB' : t.bytes + ' B'}</text>`;
      });
      if (!l.inline) {
        const w = Math.min(390, 390 * st.wisc.vlog / ((HDR + VAL) * NKEYS));
        g += `<text x="${x}" y="306">value log</text><rect x="${x + 70}" y="292" width="390" height="18" fill="none" stroke="${css('--rule')}"/><rect x="${x + 70}" y="292" width="${w}" height="18" fill="${css('--accent-deep')}"/>`;
        g += `<text x="${x + 70}" y="328">append-only: ${f(st.wisc.vlog / 1e3, 0)} KB, written once, never compacted</text>`;
      } else {
        g += `<text x="${x}" y="306">value log</text><text x="${x + 70}" y="306">(none: values ride along in every flush and merge)</text>`;
      }
      const phys = l.wal + l.vlog + l.flush + l.compact;
      g += `<text x="${x}" y="364" style="font-size:14px;fill:${css('--text')}">written ${f(phys / 1e3, 0)} KB for ${f(logical / 1e3, 0)} KB of data → WA ${logical ? f(phys / logical, 2) : '0.00'}×</text>`;
    });
    g += `<line x1="490" x2="490" y1="0" y2="370" stroke="${css('--rule')}"/>`;
    svg.innerHTML = g;
    const ctr = (l, el) => { $(el).innerHTML = [['puts', st.i], ['WAL', f(l.wal / 1e3, 0) + ' KB'], ['tree (flush+compact)', f((l.flush + l.compact) / 1e3, 1) + ' KB'], ['compactions', l.compactions]].map(([a, b]) => `<div><b>${b}</b>${a}</div>`).join(''); };
    ctr(st.base, 'c-base'); ctr(st.wisc, 'c-wisc');
  }
  if ($('sim-svg')) {
    $('b-step').onclick = () => { stop(); step(); };
    $('b-play').onclick = play;
    $('b-reset').onclick = () => { stop(); reset(); };
    $('b-speed').oninput = () => { if (timer) { stop(); play(); } };
    reset();
    // ?steps=N fast-forwards (handy for screenshots and the viva)
    const n = +new URLSearchParams(location.search).get('steps') || 0;
    for (let i = 0; i < Math.min(n, NKEYS); i++) step();
  }
})();
