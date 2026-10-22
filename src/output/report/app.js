/* The report never fetches resources. Values and connection flags come from Rust. */
(() => {
  'use strict';
  const data = JSON.parse(document.getElementById('report-data').textContent);
  const ja = data.lang === 'ja';
  const words = {
    report: ['パフォーマンスレポート', 'Performance report'],
    subtitle: ['同じ時間軸で、指標の変化をたどる。', 'Follow the metrics on a shared timeline.'],
    offline: ['オフラインレポート · 外部通信なし', 'Offline report · No external requests'],
    select: ['表示する指標', 'Choose metrics'],
    selectHint: ['チェックした指標を同じ時間範囲で比較できます。', 'Checked metrics share the same time range.'],
    host: ['ホスト / 起動区間', 'Host / boot segment'],
    boot: ['起動区間', 'boot segment'],
    activity: ['アクティビティ', 'Activity'], item: ['要素', 'Resource'],
    search: ['指標を検索', 'Search metrics'], all: ['すべて', 'All'],
    overview: ['主要指標', 'Overview'], clear: ['選択を解除', 'Clear selection'],
    selected: ['選択中', 'selected'], available: ['利用できる系列', 'available series'],
    findings: ['検知された変化', 'Detected changes'],
    evaluated: ['検知を評価できた系列', 'series evaluated for detections'],
    time: ['表示する時間範囲', 'Time range'], from: ['開始', 'From'], to: ['終了', 'To'],
    reset: ['全期間', 'Full range'], zoom: ['拡大', 'Zoom in'], back: ['前へ', 'Earlier'], next: ['次へ', 'Later'],
    zoomHint: ['開始・終了を選ぶか、グラフを横にドラッグすると拡大できます。', 'Choose start and end times, or drag across a chart to zoom.'],
    samples: ['採取値', 'Samples'], band: ['検知を裏付けた範囲', 'Detection support'],
    reference: ['固定条件 / 比較基準', 'Condition / comparison basis'],
    charts: ['指標の推移', 'Metric timelines'],
    noSelection: ['左の一覧から表示する指標を選んでください。', 'Choose metrics from the list to display charts.'],
    noMatch: ['該当する指標はありません。', 'No matching metrics.'],
    noData: ['この範囲に有効な採取値はありません。', 'No valid samples in this range.'],
    noInput: ['表示できる時系列がありません。入力ファイルと activity の指定を確認してください。', 'No time series available. Check the input files and activity selection.'],
    noFindings: ['この範囲に報告された検知はありません。評価できなかった系列は下の内訳を確認してください。', 'No reported detections in this range. See evaluation details for unevaluated series.'],
    priority: ['調査優先度', 'Investigation priority'], evidence: ['判断に使えるデータ', 'Evidence sufficiency'],
    background: ['背景の所見', 'Standing finding'], local: ['局所の所見', 'Local finding'],
    findingHint: ['クリックすると指標を追加し、前後30分を表示します。ほかの選択中の指標も同じ時刻に移動します。', 'Click a finding to add its metric and view 30 minutes of context on each side. Other selected metrics follow.'],
    coverage: ['評価できた範囲・判定の内訳', 'Evaluation coverage and evidence'],
    notes: ['読み取りの前提', 'How to read this report'],
    lineNote: ['点は採取値、線は補助線です。欠測・不連続は結びません。百分率の軸は0〜100を基準に、実測が超えると広げます。', 'Dots are samples; lines are visual aids. Missing samples and discontinuities are not joined. Percent axes start at 0–100 and expand for observations outside that range.'],
    scopeNote: ['時間の選択は表示だけを変えます。検知・比較基準・全期間集計は入力全体に対する結果です。比較基準は正常値を意味しません。', 'Time selection changes the view only. Detections, comparison baselines and full-period summaries remain based on the whole input. A comparison baseline is not a normal value.'],
    partial: ['部分結果です。読めなかった入力があります。', 'Partial report: some input could not be read.'],
    partialDetails: ['入力の診断', 'Input diagnostics'],
    period: ['全期間集計', 'Full-period summary'], mean: ['平均', 'Mean'], maximum: ['最大', 'Max'],
    unavailable: ['値なし', 'Unavailable'], missing: ['欠測', 'Missing'],
    table: ['採取値を表で確認', 'Inspect sampled data'], timestamp: ['採取時刻', 'Sample time'],
    intervalStart: ['区間の始点', 'Interval start'], value: ['値', 'Value'], reason: ['除外理由', 'Exclusion reason'],
    tableHint: ['選択した時間範囲の採取値。1ページ50件。', 'Samples in the selected time range, 50 per page.'],
    previous: ['前のページ', 'Previous page'], following: ['次のページ', 'Next page'],
    print: ['印刷', 'Print'], remove: ['表示を外す', 'Remove'],
    skip: ['グラフへ移動', 'Skip to charts'],
    zero: ['有効な観測はありません', 'No valid observations'],
    raw: ['評価結果を表示', 'Show full assessment'],
    evaluationTable: ['系列ごとの評価結果', 'Evaluation by series'],
    coverageHint: ['検知器の対象系列についての内訳です。表示できるすべての指標を検知対象にしているわけではありません。', 'These counts cover the detection catalog. Other chartable metrics may not be evaluated by the detector.'],
    fixed: ['固定条件', 'Fixed condition'], robust: ['比較基準からの逸脱', 'Departure from comparison basis'], shift: ['水準変化', 'Level shift'],
    minimum: ['最小', 'Min'], intervals: ['有効区間', 'Valid intervals'], excluded: ['除外区間', 'Excluded intervals'],
    aggregation: ['平均の取り方', 'Mean basis'],
    selectAll: ['絞り込み結果を追加', 'Add filtered metrics'],
  };
  const t = key => words[key][ja ? 0 : 1];
  function el(tag, attrs = {}, children = []) {
    const node = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs)) {
      if (name === 'class') node.className = value;
      else if (name.startsWith('on')) node.addEventListener(name.slice(2), value);
      else if (name === 'text') node.textContent = value;
      else if (typeof value === 'boolean') node[name] = value;
      else node.setAttribute(name, value);
    }
    for (const child of children) node.append(child);
    return node;
  }
  const text = (tag, content, cls = '') => el(tag, {text: content, class: cls});
  const button = (content, action, cls = '') => el('button', {type: 'button', text: content, onclick: action, class: cls});
  const labelled = (content, input) => el('label', {}, [text('span', content), input]);
  const num = value => value === null || value === undefined ? '—' : Number.isFinite(value) ? new Intl.NumberFormat(ja ? 'ja' : 'en', {maximumSignificantDigits: 6}).format(value) : t('unavailable');
  const nameOf = s => `${s.activity} / ${s.item} / ${s.label}${s.label !== s.metric ? ` [${s.metric}]` : ''}`;
  const unitValue = (s, value) => `${num(value)}${s.unit ? ` ${s.unit}` : ''}`;
  const segment = () => data.segments[state.segment];
  let state = {segment: 0, selected: [], a: 0, b: 0};
  let cursor = null;
  let charts = [];
  let activeFinding = null;
  let tablePage = 0;
  let pendingDraw = false;

  document.title = `reSARch · ${t('report')}`;
  document.body.append(el('a', {href: '#charts', class: 'skip-link', text: t('skip')}));
  document.body.append(el('header', {class: 'app-header'}, [el('div', {class: 'brand'}, [document.createTextNode('re'), text('span', 'SAR'), document.createTextNode('ch')]), text('div', t('offline'), 'header-note')]));
  const app = el('div', {class: 'app'});
  const sidebar = el('aside', {class: 'sidebar', 'aria-label': t('select')});
  const inner = el('div', {class: 'sidebar-inner'});
  inner.append(text('h2', t('select')), text('p', t('selectHint'), 'muted intro'));
  const hostSelect = el('select', {id: 'host-select', onchange: () => changeSegment(Number(hostSelect.value))});
  data.segments.forEach((s, i) => hostSelect.append(el('option', {value: i, disabled: !s.times.length, text: `${s.host} / ${t('boot')} ${s.boot}`})));
  const activitySelect = el('select', {id: 'activity-select', onchange: () => { updateItems(); renderSeriesList(); }});
  const itemSelect = el('select', {id: 'item-select', onchange: renderSeriesList});
  const search = el('input', {type: 'search', id: 'metric-search', oninput: renderSeriesList});
  inner.append(labelled(t('host'), hostSelect), labelled(t('activity'), activitySelect), labelled(t('item'), itemSelect), labelled(t('search'), search));
  const seriesList = el('div', {class: 'series-list', id: 'series-list'});
  inner.append(el('div', {class: 'sidebar-actions'}, [button(t('overview'), () => { state.selected = defaults(); render(); }), button(t('clear'), () => { state.selected = []; render(); })]), seriesList);
  sidebar.append(inner);
  const main = el('main', {class: 'main'});
  main.append(el('div', {class: 'page-title'}, [el('div', {}, [text('div', 'SYSTEM PERFORMANCE', 'eyebrow'), text('h1', t('report')), text('p', t('subtitle'))]), button(t('print'), () => window.print())]));
  if (data.partial) {
    main.append(el('div', {class: 'notice', role: 'status'}, [text('strong', t('partial')), el('details', {}, [text('summary', t('partialDetails')), text('pre', JSON.stringify({skipped: data.skipped, incomplete: data.incomplete}, null, 2))])]));
  }
  const stats = el('div', {class: 'stats'});
  main.append(stats);
  const rangePanel = el('section', {class: 'panel time-panel', 'aria-label': t('time')});
  const fromSelect = el('select', {id: 'from-time', onchange: () => setRange(Number(fromSelect.value), state.b, 'from')});
  const toSelect = el('select', {id: 'to-time', onchange: () => setRange(state.a, Number(toSelect.value), 'to')});
  const fromRange = el('input', {type: 'range', id: 'from-range', min: 0, step: 1, 'aria-label': t('from'), oninput: () => setRange(Number(fromRange.value), state.b, 'from')});
  const toRange = el('input', {type: 'range', id: 'to-range', min: 0, step: 1, 'aria-label': t('to'), oninput: () => setRange(state.a, Number(toRange.value), 'to')});
  const timeStatus = text('p', '', 'time-status mono');
  rangePanel.append(el('div', {class: 'section-head'}, [text('h2', t('time')), el('div', {class: 'button-row'}, [button(t('back'), () => pan(-1)), button(t('next'), () => pan(1)), button(t('zoom'), zoomIn), button(t('reset'), () => setRange(0, segment().times.length - 1), 'primary')])]), el('div', {class: 'time-inputs'}, [el('div', {}, [labelled(t('from'), fromSelect), fromRange]), el('div', {}, [labelled(t('to'), toSelect), toRange])]), timeStatus, text('p', t('zoomHint'), 'hint'));
  main.append(rangePanel);
  main.append(el('div', {class: 'section-head'}, [text('h2', t('charts')), text('span', '', 'badge')]));
  const chartStatus = main.lastChild.lastChild;
  main.append(el('div', {class: 'legend'}, [text('span', t('samples'), 'sample-key'), text('span', t('band'), 'band-key'), text('span', t('reference'), 'guide-key')]));
  const chartArea = el('section', {id: 'charts', tabindex: -1, 'aria-label': t('charts')});
  main.append(chartArea);
  const findingSection = el('section', {class: 'panel', 'aria-label': t('findings')});
  const findingCount = text('span', '', 'badge');
  const findingDetail = el('div', {class: 'finding-details', hidden: true});
  const findingList = el('div', {class: 'finding-list', id: 'finding-list'});
  findingSection.append(el('div', {class: 'section-head'}, [text('h2', t('findings')), findingCount]), text('p', t('findingHint'), 'hint'), findingDetail, findingList);
  main.append(findingSection);
  const tableSection = el('details', {class: 'panel', ontoggle: () => { if (tableSection.open) renderTable(); }});
  const tableSelect = el('select', {id: 'table-series', 'aria-label': t('select'), onchange: () => { tablePage = 0; renderTable(); }});
  const tableBody = el('div', {class: 'table-scroll'});
  const pageLabel = text('span', '', 'muted');
  const prevPage = button(t('previous'), () => { tablePage--; renderTable(); });
  const nextPage = button(t('following'), () => { tablePage++; renderTable(); });
  tableSection.append(text('summary', t('table')), el('div', {class: 'data-tools'}, [tableSelect]), tableBody, el('div', {class: 'data-tools'}, [prevPage, pageLabel, nextPage]));
  main.append(tableSection);
  const coverage = el('section', {class: 'panel coverage'});
  main.append(coverage, el('footer', {class: 'footer'}, [text('p', t('lineNote')), text('p', t('scopeNote'))]));
  app.append(sidebar, main);
  document.body.append(app);

  function defaults() {
    const all = segment().series;
    const wanted = [['A_CPU', 'all', 'idle'], ['A_CPU', 'all', 'user'], ['A_CPU', 'all', 'iowait'], ['A_QUEUE', '-', 'runq_sz'], ['A_MEMORY', '-', 'memused_pct'], ['A_DISK', null, 'util_pct']];
    const selected = [];
    for (const [activity, item, metric] of wanted) {
      const i = all.findIndex(s => s.activity === activity && (item === null || s.item === item) && s.metric.replaceAll('-', '_') === metric);
      if (i >= 0) selected.push(i);
    }
    for (const f of segment().findings) if (selected.length < 6 && f.series !== null && !selected.includes(f.series)) selected.push(f.series);
    if (!selected.length) return all.slice(0, 6).map((_, i) => i);
    return selected;
  }
  function saveState() {
    try { history.replaceState(null, '', `#${encodeURIComponent(JSON.stringify(state))}`); } catch (_) { /* Some file viewers restrict history. */ }
  }
  function restoreState() {
    try {
      const saved = JSON.parse(decodeURIComponent(location.hash.slice(1)));
      if (!Number.isInteger(saved.segment) || !data.segments[saved.segment] || !Array.isArray(saved.selected)) return false;
      const s = data.segments[saved.segment];
      if (!s.times.length || ![saved.a, saved.b].every(Number.isInteger) || saved.a < 0 || saved.b < saved.a || saved.b >= s.times.length) return false;
      state = {segment: saved.segment, selected: [...new Set(saved.selected.filter(i => Number.isInteger(i) && i >= 0 && i < s.series.length))], a: saved.a, b: saved.b};
      return true;
    } catch (_) { return false; }
  }
  function populateControls() {
    hostSelect.value = state.segment;
    activitySelect.replaceChildren(el('option', {value: '', text: t('all')}));
    for (const activity of new Set(segment().series.map(s => s.activity))) activitySelect.append(el('option', {value: activity, text: activity}));
    updateItems();
    for (const select of [fromSelect, toSelect]) {
      select.replaceChildren();
      const options = document.createDocumentFragment();
      segment().times.forEach((time, i) => options.append(el('option', {value: i, text: time.label})));
      select.append(options);
    }
    fromRange.max = toRange.max = Math.max(0, segment().times.length - 1);
    search.value = '';
  }
  function changeSegment(index) {
    state = {segment: index, selected: [], a: 0, b: data.segments[index].times.length - 1};
    state.selected = defaults();
    cursor = null; activeFinding = null; tablePage = 0;
    populateControls(); render();
  }
  function updateItems() {
    itemSelect.replaceChildren(el('option', {value: '', text: t('all')}));
    const items = new Set(segment().series.filter(s => !activitySelect.value || s.activity === activitySelect.value).map(s => s.item));
    for (const item of items) itemSelect.append(el('option', {value: item, text: item}));
  }
  function renderSeriesList() {
    seriesList.replaceChildren();
    const query = search.value.toLocaleLowerCase();
    const fragment = document.createDocumentFragment();
    let count = 0;
    segment().series.forEach((s, i) => {
      if (activitySelect.value && s.activity !== activitySelect.value || itemSelect.value && s.item !== itemSelect.value || !`${nameOf(s)} ${s.metric}`.toLocaleLowerCase().includes(query)) return;
      count++;
      const checkbox = el('input', {type: 'checkbox', checked: state.selected.includes(i), 'data-series': i, onchange: () => {
        state.selected = checkbox.checked ? [...state.selected, i] : state.selected.filter(v => v !== i);
        render(false);
      }});
      const hasFinding = segment().findings.some(f => f.series === i);
      fragment.append(el('label', {class: 'series-option'}, [checkbox, el('span', {}, [text('strong', `${s.label} ${s.unit && s.unit !== s.label ? `· ${s.unit}` : ''}`), text('small', `${s.activity} / ${s.item}${hasFinding ? ` · ${t('findings')}` : ''}`, hasFinding ? 'finding-dot' : '')])]));
    });
    seriesList.append(count ? fragment : text('p', t('noMatch'), 'empty'));
  }
  const epochAt = i => BigInt(segment().times[i].epoch);
  const lowerIndex = target => {
    const times = segment().times;
    let lo = 0, hi = times.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (BigInt(times[mid].epoch) < target) lo = mid + 1; else hi = mid; }
    return Math.min(lo, times.length - 1);
  };
  function setRange(a, b, changed) {
    const max = segment().times.length - 1;
    if (max < 0) return;
    a = Math.max(0, Math.min(max, a)); b = Math.max(0, Math.min(max, b));
    if (a > b) { if (changed === 'to') a = b; else b = a; }
    state.a = a; state.b = b; cursor = null; tablePage = 0;
    render();
  }
  function pan(direction) {
    const width = Math.max(1, state.b - state.a);
    const max = segment().times.length - 1;
    const a = Math.max(0, Math.min(max - width, state.a + direction * Math.ceil(width / 2)));
    setRange(a, Math.min(max, a + width));
  }
  function zoomIn() {
    const inset = Math.floor((state.b - state.a) / 4);
    setRange(state.a + inset, state.b - inset);
  }
  function visibleFinding(f) {
    return BigInt(f.start) <= epochAt(state.b) && BigInt(f.end) >= epochAt(state.a);
  }
  function focusFinding(f) {
    activeFinding = f;
    if (f.series !== null && !state.selected.includes(f.series)) state.selected.push(f.series);
    const a = BigInt(f.start) - 1800n;
    const b = BigInt(f.end) + 1800n;
    setRange(Math.max(0, lowerIndex(a) - 1), lowerIndex(b));
    chartArea.scrollIntoView({block: 'start'});
  }
  function render(updateList = true) {
    const s = segment();
    if (!s || !s.times.length) return;
    saveState();
    fromSelect.value = fromRange.value = state.a;
    toSelect.value = toRange.value = state.b;
    timeStatus.textContent = `${s.times[state.a].label} → ${s.times[state.b].label} · ${s.timezone}`;
    chartStatus.textContent = `${state.selected.length} ${t('selected')} / ${s.series.length}`;
    stats.replaceChildren(...[[s.series.length, t('available')], [s.findings.length, t('findings')], [`${s.assessment.coverage.series_evaluated} / ${s.assessment.coverage.series_present}`, t('evaluated')]].map(([value, label]) => el('div', {class: 'stat'}, [text('strong', String(value), 'mono'), text('span', label)])));
    if (updateList) renderSeriesList();
    chartArea.replaceChildren(); charts = [];
    if (!state.selected.length) chartArea.append(text('div', t('noSelection'), 'panel empty'));
    for (const index of state.selected) addChart(index);
    renderFindings();
    const selectedTable = tableSelect.value;
    tableSelect.replaceChildren(...state.selected.map(i => el('option', {value: i, text: nameOf(s.series[i])})));
    if (state.selected.includes(Number(selectedTable))) tableSelect.value = selectedTable;
    if (tableSection.open) renderTable();
    coverage.replaceChildren(text('h2', t('coverage')));
    const c = s.assessment.coverage;
    coverage.append(text('p', `${t('evaluated')}: ${c.series_evaluated} / ${c.series_present}`, 'hint'));
    coverage.append(text('p', t('coverageHint'), 'hint'));
    const evaluationTable = el('table', {}, [text('caption', t('evaluationTable')), el('thead', {}, [el('tr', {}, [t('select'), t('samples'), t('missing'), t('fixed'), t('robust'), t('shift')].map(label => el('th', {scope: 'col', text: label})))])]);
    evaluationTable.append(el('tbody', {}, s.evaluations.map(row => el('tr', {}, row.map(value => text('td', value))))));
    coverage.append(el('details', {}, [text('summary', t('evaluationTable')), el('div', {class: 'table-scroll'}, [evaluationTable])]));
    coverage.append(el('details', {}, [text('summary', t('notes')), ...s.assessment.notes.map(note => text('p', note, 'hint'))]));
    coverage.append(el('details', {}, [text('summary', t('raw')), text('pre', JSON.stringify(s.assessment, null, 2))]));
    requestDraw();
  }
  function renderFindings() {
    const s = segment();
    const findings = s.findings.filter(visibleFinding);
    findingCount.textContent = `${findings.length} / ${s.findings.length}`;
    findingList.replaceChildren();
    if (!findings.length) findingList.append(text('p', t('noFindings'), 'hint'));
    for (const f of findings) {
      const btn = button('', () => focusFinding(f), 'finding-card');
      btn.append(text('strong', f.headline), text('small', `${f.from_label} → ${f.to_label}`, 'mono'), text('small', `${f.background ? t('background') : t('local')} · ${t('priority')}: ${f.priority} · ${t('evidence')}: ${f.evidence}`));
      findingList.append(btn);
    }
    findingDetail.hidden = !activeFinding;
    findingDetail.replaceChildren();
    if (activeFinding) {
      const f = activeFinding;
      findingDetail.append(text('strong', f.headline), text('p', `${f.from_label} → ${f.to_label}`, 'mono'), text('p', `${t('priority')}: ${f.priority} / ${t('evidence')}: ${f.evidence}`), text('p', f.detail));
      for (const ref of f.references) findingDetail.append(text('p', `${ref.label}: ${num(ref.value)}`));
    }
  }
  function addChart(index) {
    const s = segment().series[index];
    const card = el('article', {class: 'panel chart', 'data-chart': index});
    const value = text('output', '', 'chart-value');
    const header = el('div', {class: 'chart-head'}, [el('div', {}, [text('h3', nameOf(s), 'chart-title'), text('p', `${s.origin} · ${s.unit || '—'}`)]), value]);
    const canvas = el('canvas', {'aria-label': `${nameOf(s)}. ${t('table')}`, role: 'img'});
    const note = text('p', '', 'plot-description');
    const referenceLabels = text('p', '', 'plot-description');
    const remove = button(`${t('remove')} · ${s.label}`, () => { state.selected = state.selected.filter(i => i !== index); render(); });
    const summary = s.summary;
    const fullSummary = summary ? `${t('mean')}: ${unitValue(s, summary.mean)} · ${t('minimum')}: ${unitValue(s, summary.min?.value)} · ${t('maximum')}: ${unitValue(s, summary.max?.value)} · p95: ${unitValue(s, summary.p95?.value)} · ${t('intervals')}: ${summary.intervals} · ${t('excluded')}: ${summary.excluded_intervals}` : '';
    const methods = {
      rate_over_valid_intervals: ['有効区間の差分合計と分母合計から計算', 'From total deltas and denominators over valid intervals'],
      sample_mean: ['採取値の標本平均', 'Sample mean'], time_weighted_mean: ['区間長で重みを付けた平均', 'Time-weighted mean'],
      derived_interval_sample_mean: ['計算済み区間値の標本平均', 'Sample mean of derived interval values'],
      last_valid: ['最後の有効観測値', 'Last valid observation'], delta_sum: ['有効区間の差分合計', 'Sum of valid deltas'],
      not_aggregated: ['集計対象外', 'Not aggregated'],
    };
    const method = summary ? methods[summary.method]?.[ja ? 0 : 1] || summary.method : '—';
    card.append(header, el('div', {class: 'chart-wrap'}, [canvas]), note, referenceLabels, el('details', {}, [text('summary', t('period')), text('p', fullSummary, 'hint'), text('p', `${t('aggregation')}: ${method}`, 'hint'), remove]));
    chartArea.append(card);
    const chart = {index, series: s, canvas, value, note, referenceLabels, visible: [], x: null, drag: null};
    charts.push(chart);
    canvas.addEventListener('pointermove', e => {
      if (!chart.x) return;
      cursor = chart.x(e.clientX);
      if (chart.drag) chart.drag.end = cursor;
      requestDraw();
    });
    canvas.addEventListener('pointerleave', () => { if (!chart.drag) { cursor = null; requestDraw(); } });
    canvas.addEventListener('pointerdown', e => {
      if (!chart.x || e.button !== 0) return;
      chart.drag = {start: chart.x(e.clientX), end: chart.x(e.clientX), client: e.clientX};
      canvas.setPointerCapture(e.pointerId);
    });
    canvas.addEventListener('pointerup', e => {
      const drag = chart.drag; chart.drag = null;
      if (drag && Math.abs(e.clientX - drag.client) > 8) {
        const end = chart.x(e.clientX);
        setRange(lowerIndex(drag.start < end ? drag.start : end), lowerIndex(drag.start < end ? end : drag.start));
      } else requestDraw();
    });
    canvas.addEventListener('pointercancel', () => { chart.drag = null; cursor = null; requestDraw(); });
  }
  function requestDraw() {
    if (pendingDraw) return;
    pendingDraw = true;
    requestAnimationFrame(() => { pendingDraw = false; for (const chart of charts) draw(chart); });
  }
  function draw(chart) {
    const {canvas, series: s} = chart;
    const rect = canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    const width = rect.width, height = rect.height, ratio = window.devicePixelRatio || 1;
    if (canvas.width !== Math.round(width * ratio) || canvas.height !== Math.round(height * ratio)) { canvas.width = Math.round(width * ratio); canvas.height = Math.round(height * ratio); }
    const ctx = canvas.getContext('2d');
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0); ctx.clearRect(0, 0, width, height);
    const left = Math.min(78, width * .22), right = width - 14, top = 15, bottom = height - 32;
    const start = epochAt(state.a), end = epochAt(state.b), span = end - start || 1n;
    const x = time => start === end ? (left + right) / 2 : left + Number(time - start) / Number(span) * (right - left);
    chart.x = client => start + BigInt(Math.round(Math.max(0, Math.min(1, (client - rect.left - left) / (right - left))) * Number(end - start)));
    chart.visible = s.points.filter(p => p[1] >= state.a && p[1] <= state.b);
    const findings = segment().findings.filter(f => f.series === chart.index && visibleFinding(f));
    const refs = findings.flatMap(f => f.references);
    chart.referenceLabels.textContent = [...new Set(refs.map(ref => `${ref.label}: ${unitValue(s, ref.value)}`))].join(' · ');
    let low = 0, high = s.percent ? 100 : 0, observed = false;
    for (const p of chart.visible) if (p[2] !== null && p[3] === null) { low = Math.min(low, p[2]); high = Math.max(high, p[2]); observed = true; }
    for (const ref of refs) { low = Math.min(low, ref.value); high = Math.max(high, ref.value); }
    const scale = Math.max(1, Math.abs(low), Math.abs(high));
    low /= scale; high /= scale;
    if (low === high) high = low + 1;
    if (!s.percent) high += (high - low) * .08;
    const y = value => bottom - (value / scale - low) / (high - low) * (bottom - top);
    ctx.font = '11px system-ui, sans-serif'; ctx.lineWidth = 1;
    for (let tick = 0; tick <= 4; tick++) {
      const py = top + (bottom - top) * tick / 4;
      ctx.strokeStyle = '#e2e9e7'; ctx.beginPath(); ctx.moveTo(left, py); ctx.lineTo(right, py); ctx.stroke();
      ctx.fillStyle = '#52646b'; ctx.textAlign = 'right'; ctx.fillText(num((high + (low - high) * tick / 4) * scale), left - 8, py + 4);
    }
    let previousTick = -1;
    for (let tick = 0; tick <= 4; tick++) {
      const time = start + (end - start) * BigInt(tick) / 4n;
      const index = tick === 0 ? state.a : tick === 4 ? state.b : lowerIndex(time);
      if (index === previousTick) continue;
      previousTick = index;
      const px = x(epochAt(index));
      ctx.strokeStyle = '#e8eeec'; ctx.beginPath(); ctx.moveTo(px, top); ctx.lineTo(px, bottom); ctx.stroke();
      ctx.fillStyle = '#52646b'; ctx.textAlign = tick === 0 ? 'left' : tick === 4 ? 'right' : 'center';
      // Labels refer to real samples at their actual positions, rather than fabricated clock times.
      const label = segment().times[index][end - start >= 86400n ? 'short' : 'clock'];
      if (tick === 0 || tick === 4 || width > 500) ctx.fillText(label, px, height - 9);
    }
    ctx.save(); ctx.beginPath(); ctx.rect(left, top, right - left, bottom - top); ctx.clip();
    for (const f of findings) {
      const x1 = x(BigInt(f.start)), x2 = x(BigInt(f.end));
      ctx.fillStyle = f.background ? '#dbeafe88' : '#ffedd5bb'; ctx.fillRect(x1, top, Math.max(2, x2 - x1), bottom - top);
      ctx.strokeStyle = f.background ? '#5584a3' : '#bd7637'; ctx.setLineDash([3, 4]); ctx.strokeRect(x1, top, Math.max(2, x2 - x1), bottom - top); ctx.setLineDash([]);
    }
    if (activeFinding && activeFinding.series !== chart.index && visibleFinding(activeFinding)) {
      ctx.fillStyle = '#00786b12';
      const x1 = x(BigInt(activeFinding.start)), x2 = x(BigInt(activeFinding.end));
      ctx.fillRect(x1, top, Math.max(2, x2 - x1), bottom - top);
      ctx.strokeStyle = '#00786b'; ctx.setLineDash([2, 4]); ctx.strokeRect(x1, top, Math.max(2, x2 - x1), bottom - top); ctx.setLineDash([]);
    }
    for (const ref of refs) {
      const py = y(ref.value);
      ctx.strokeStyle = '#00786b'; ctx.setLineDash([6, 4]); ctx.beginPath(); ctx.moveTo(x(BigInt(ref.start)), py); ctx.lineTo(x(BigInt(ref.end)), py); ctx.stroke(); ctx.setLineDash([]);
    }
    let previous = null;
    ctx.strokeStyle = '#176d9c'; ctx.fillStyle = '#176d9c'; ctx.lineWidth = 1.7;
    for (const p of chart.visible) {
      const px = x(epochAt(p[1]));
      if (p[2] === null || p[3] !== null) {
        previous = null;
        ctx.strokeStyle = '#a14e0a'; ctx.beginPath(); ctx.moveTo(px - 3, bottom - 7); ctx.lineTo(px + 3, bottom - 1); ctx.moveTo(px + 3, bottom - 7); ctx.lineTo(px - 3, bottom - 1); ctx.stroke(); ctx.strokeStyle = '#176d9c';
        continue;
      }
      const py = y(p[2]);
      if (previous && p[4]) { ctx.beginPath(); ctx.moveTo(previous[0], previous[1]); ctx.lineTo(px, py); ctx.stroke(); }
      ctx.beginPath(); ctx.arc(px, py, 2.3, 0, Math.PI * 2); ctx.fill(); previous = [px, py];
    }
    if (chart.drag) { ctx.fillStyle = '#00786b22'; const a = x(chart.drag.start), b = x(chart.drag.end); ctx.fillRect(Math.min(a, b), top, Math.abs(b - a), bottom - top); }
    chart.value.textContent = '';
    chart.note.textContent = observed ? '' : t('noData');
    if (cursor !== null) {
      ctx.strokeStyle = '#172b3277'; ctx.setLineDash([3, 3]); ctx.beginPath(); ctx.moveTo(x(cursor), top); ctx.lineTo(x(cursor), bottom); ctx.stroke(); ctx.setLineDash([]);
      let nearest = null, distance = Infinity;
      for (const p of chart.visible) { const d = Math.abs(Number(epochAt(p[1]) - cursor)); if (d < distance) { distance = d; nearest = p; } }
      if (nearest) {
        chart.value.textContent = nearest[2] === null ? t('missing') : unitValue(s, nearest[2]);
        chart.note.textContent = `${segment().times[nearest[1]].label} · ${nearest[2] === null ? `${t('missing')}: ${nearest[3] || t('unavailable')}` : `${t('value')}: ${nearest[2]} ${s.unit}`} · ${t('intervalStart')}: ${segment().times[nearest[0]].label}`;
      }
    }
    ctx.restore();
  }
  function renderTable() {
    tableBody.replaceChildren();
    const s = segment().series[Number(tableSelect.value)];
    if (!state.selected.length || !s) { tableBody.append(text('p', t('noSelection'), 'hint')); return; }
    const points = s.points.filter(p => p[1] >= state.a && p[1] <= state.b);
    const pages = Math.max(1, Math.ceil(points.length / 50));
    tablePage = Math.max(0, Math.min(pages - 1, tablePage));
    const table = el('table', {}, [text('caption', `${nameOf(s)} · ${t('tableHint')}`), el('thead', {}, [el('tr', {}, [t('timestamp'), t('intervalStart'), `${t('value')} (${s.unit || '—'})`, t('reason')].map(label => el('th', {scope: 'col', text: label})))])]);
    const body = el('tbody');
    for (const p of points.slice(tablePage * 50, tablePage * 50 + 50)) body.append(el('tr', {}, [segment().times[p[1]].label, segment().times[p[0]].label, p[2] === null ? '—' : String(p[2]), p[3] || '—'].map(value => text('td', value))));
    table.append(body); tableBody.append(table);
    pageLabel.textContent = `${tablePage + 1} / ${pages} · ${points.length}`;
    prevPage.disabled = tablePage === 0; nextPage.disabled = tablePage === pages - 1;
  }
  window.addEventListener('resize', requestDraw);
  window.addEventListener('hashchange', () => { if (restoreState()) { activeFinding = null; populateControls(); render(); } });
  if (!data.segments.length || !data.segments.some(s => s.times.length)) {
    main.prepend(text('p', t('noInput'), 'notice'));
    sidebar.hidden = true; rangePanel.hidden = true;
  } else {
    if (!restoreState()) {
      state.segment = data.segments.findIndex(s => s.times.length);
      state.b = segment().times.length - 1;
      state.selected = defaults();
    }
    populateControls(); render();
  }
})();
