// Triad dashboard: every run, its progress, its telemetry, its controls.
//
// Rows are Strix runs; the detail pane follows the selection. State is polled from
// `triad runs --json`; the numbers on the right come from Docker and /proc. The verbose
// view and the new-engagement form are the two modal states over that list.
import React, {useCallback, useEffect, useMemo, useRef, useState} from 'react';
import {Box, Text, render, useApp, useInput} from 'ink';
import {existsSync} from 'node:fs';
import {join} from 'node:path';
import {
  cpuPercent, elapsed, fetchContainerMetrics, fetchProgressDetail, fetchSnapshot, human, humanKb,
  readProc, type ProcSample, type ProgressDetail, type RunProgress, type Snapshot,
} from './data.js';
import {
  feed, pauseOrResume, remove, startEngage, startScan, stop,
  type Focus, type NewEngagement, type ScanMode, type Target,
} from './control.js';

const HELP = [
  ['↑/↓  k/j', 'select a run'],
  ['tab', 'switch the target between the run and its graph'],
  ['enter', 'verbose agent stream for the selected run'],
  ['n', 'new engagement (scan or engage form)'],
  ['p', 'pause / resume the target'],
  ['s', 'stop the target'],
  ['d', 'delete the target (asks first)'],
  ['f', 'feed the selected run into its project again'],
  ['r', 'refresh now'],
  ['?', 'hide this help'],
  ['q', 'quit'],
];

const STATE_COLOUR: Record<string, string> = {
  running: 'green', completed: 'cyan', paused: 'yellow', stopped: 'red',
  failed: 'red', timeout: 'red', quiet: 'cyan', stale: 'yellow',
};

function stateOf(run: RunProgress): string {
  if (run.paused) return 'paused';
  if (run.live) return 'running';
  // run.json keeps saying "running" for a scan whose process is gone (killed, rebooted),
  // which is exactly the state that reads as "it is still going" when it is not.
  if (run.status === 'running' || run.status === 'in_progress') return 'stale';
  return run.status ?? 'unknown';
}

function fit(text: string, width: number): string {
  return text.length > width ? text.slice(0, width - 1) + '…' : text.padEnd(width);
}

function Row({run, selected}: {run: RunProgress; selected: boolean}) {
  const state = stateOf(run);
  const colour = STATE_COLOUR[state] ?? 'gray';
  return (
    <Box>
      <Text color={selected ? 'cyan' : undefined}>{selected ? '▸ ' : '  '}</Text>
      <Text color={colour}>{run.live || run.paused ? '●' : '○'} </Text>
      <Text bold={selected} color={selected ? 'white' : undefined}>
        {fit(run.run, 23)}
      </Text>
      <Text color={colour}> {fit(state, 9)}</Text>
      <Text dimColor>{elapsed(run.start_time, run.end_time).padStart(6)}</Text>
    </Box>
  );
}

function Line({label, children}: {label: string; children: React.ReactNode}) {
  return (
    <Box>
      <Text dimColor>{label.padEnd(11)}</Text>
      {children}
    </Box>
  );
}

function Detail({run, project, focus, metrics}: {
  run: RunProgress | null;
  project: {id: string; title: string; status: string; facts: number; hints: number;
            intents: number; open: number} | null;
  focus: Focus;
  metrics: {sandbox: string; strix: string; dispatcher: string};
}) {
  if (!run) {
    return <Box paddingLeft={2}><Text dimColor>no runs yet — start one with `triad engage`</Text></Box>;
  }
  const runFocus = focus === 'run';
  return (
    <Box flexDirection="column" paddingLeft={1}>
      <Text bold color={runFocus ? 'cyan' : undefined}>
        {runFocus ? '▸ ' : '  '}STRIX
      </Text>
      <Line label="run"><Text>{run.run}</Text></Line>
      <Line label="state">
        <Text color={STATE_COLOUR[stateOf(run)] ?? 'gray'}>{stateOf(run)}</Text>
        <Text dimColor>  {elapsed(run.start_time, run.end_time)}  pid {run.pid ?? '-'}</Text>
      </Line>
      <Line label="findings">
        <Text color={run.findings > 0 ? 'green' : 'gray'}>{run.findings}</Text>
        <Text dimColor>  gaps {run.coverage_gaps}  notes {run.notes}</Text>
      </Line>
      <Line label="agents">
        <Text>{run.agents.completed}/{run.agents.total} done</Text>
        <Text dimColor>
          {run.agents.running.length > 0 ? `  ${run.agents.running.length} working` : ''}
          {run.agents.failed > 0 ? `  ${run.agents.failed} failed` : ''}
        </Text>
      </Line>
      <Line label="todos">
        <Text>{run.todos.done}/{run.todos.total} done</Text>
        <Text dimColor>  {run.todos.in_progress} in progress</Text>
      </Line>
      <Line label="tokens">
        <Text>{human(run.usage.input_tokens)} in / {human(run.usage.output_tokens)} out</Text>
        <Text dimColor>  {run.usage.requests ?? '-'} requests</Text>
      </Line>
      {run.agents.running.length > 0 && (
        <Line label="now"><Text color="cyan">{run.agents.running.slice(0, 2).join(', ')}</Text></Line>
      )}

      <Box marginTop={1}>
        <Text bold color={!runFocus ? 'cyan' : undefined}>
          {!runFocus ? '▸ ' : '  '}CAIRN
        </Text>
      </Box>
      {project ? (
        <>
          <Line label="project">
            <Text>{project.id}</Text>
            <Text color={project.status === 'active' ? 'green' : 'yellow'}>  {project.status}</Text>
          </Line>
          <Line label="graph">
            <Text>{project.facts} facts  {project.hints} hints  {project.intents} intents</Text>
            <Text dimColor>  {project.open} open</Text>
          </Line>
        </>
      ) : (
        <Line label="project"><Text dimColor>none linked to this run</Text></Line>
      )}

      <Box marginTop={1}><Text bold>TELEMETRY</Text></Box>
      <Line label="sandbox"><Text>{metrics.sandbox}</Text></Line>
      <Line label="scan"><Text>{metrics.strix}</Text></Line>
      <Line label="dispatcher"><Text>{metrics.dispatcher}</Text></Line>
    </Box>
  );
}

const BLANK_FORM: NewEngagement = {flow: 'scan', target: '', title: '', goal: '', mode: 'quick'};

function Form({fields, row, error}: {fields: NewEngagement; row: number; error: string | null}) {
  const marker = (on: boolean) => (on ? '◉' : '○');
  const label = (n: number, text: string) => (
    <Text color={row === n ? 'cyan' : undefined} bold={row === n}>
      {row === n ? '▸ ' : '  '}{text.padEnd(7)}
    </Text>
  );
  const cursor = (n: number) => (row === n ? <Text color="cyan">▏</Text> : null);
  return (
    <Box flexDirection="column" paddingLeft={1}>
      <Text bold color="cyan">NEW ENGAGEMENT</Text>
      <Box marginTop={1} flexDirection="column">
        <Box>
          {label(0, 'flow')}
          <Text>{marker(fields.flow === 'scan')} scan   {marker(fields.flow === 'engage')} engage</Text>
        </Box>
        <Box>
          {label(1, 'target')}
          <Text>{fields.target}</Text>{cursor(1)}
          {!fields.target && <Text dimColor>   required (URL or local path)</Text>}
        </Box>
        <Box>
          {label(2, 'title')}
          <Text>{fields.title}</Text>{cursor(2)}
          {!fields.title && <Text dimColor>   required (names the engagement directory)</Text>}
        </Box>
        <Box>
          {label(3, 'goal')}
          <Text dimColor={fields.flow !== 'engage'}>{fields.goal}</Text>{cursor(3)}
          {fields.flow === 'engage' && !fields.goal && <Text dimColor>   required for engage</Text>}
        </Box>
        <Box>
          {label(4, 'mode')}
          <Text>{marker(fields.mode === 'quick')} quick   {marker(fields.mode === 'standard')} standard   {marker(fields.mode === 'deep')} deep</Text>
        </Box>
      </Box>
      <Box marginTop={1}>
        <Text dimColor>↑/↓ or tab move   ←/→ or space toggle   enter submit   esc cancel</Text>
      </Box>
      {error && <Text color="red">✗ {error}</Text>}
    </Box>
  );
}

type DocLine = {text: string; dim?: boolean; color?: string; bold?: boolean};

function buildDoc(d: ProgressDetail, width: number): {lines: DocLine[]; streamEnd: number} {
  const lines: DocLine[] = [];
  const push = (text: string, opts: Omit<DocLine, 'text'> = {}) => lines.push({text, ...opts});
  const wrapPush = (text: string, opts: Omit<DocLine, 'text'> = {}, indent = 0) => {
    const pad = ' '.repeat(indent);
    const room = Math.max(8, width - indent);
    for (const raw of (text ?? '').split('\n')) {
      if (!raw) { push(pad, opts); continue; }
      for (let i = 0; i < raw.length; i += room) push(pad + raw.slice(i, i + room), opts);
    }
  };

  push(d.run, {bold: true, color: 'cyan'});
  push(`state ${stateOf(d)}  ${elapsed(d.start_time, d.end_time)}  pid ${d.pid ?? '-'}`, {dim: true});
  push('');

  const messages = d.messages ?? [];
  push(`AGENT STREAM (${messages.length})`, {bold: true});
  if (!messages.length) push('  (no agent messages yet)', {dim: true});
  for (const m of messages) {
    const marker = m.type === 'function_call' ? `call ${m.tool ?? '?'}` : m.type;
    const output = m.type === 'function_call_output';
    push(`  ${m.agent_name} · ${marker}${m.truncated ? ' [truncated]' : ''}`,
         {dim: true, color: output ? 'gray' : undefined});
    wrapPush(m.text || '(empty)',
             {dim: output || m.type === 'reasoning', color: output ? 'gray' : undefined}, 4);
    push('');
  }
  const streamEnd = lines.length - 1;

  push('AGENTS', {bold: true});
  if (d.agents_detail?.length) {
    for (const a of d.agents_detail) push(`  ${a.status.padEnd(10)} ${a.name}`);
  } else {
    push('  (none)', {dim: true});
  }

  push('');
  push('FINDINGS', {bold: true});
  if (d.findings_detail?.length) {
    for (const f of d.findings_detail) wrapPush(`  [${f.severity ?? '?'}] ${f.title ?? ''}`);
  } else {
    push('  (none)', {dim: true});
  }

  push('');
  push('COVERAGE', {bold: true});
  if (d.coverage?.summary !== undefined) {
    wrapPush(`  summary: ${JSON.stringify(d.coverage.summary).slice(0, 600)}`, {dim: true});
  }
  if (d.coverage?.gaps) push(`  gaps: ${d.coverage.gaps.length}`, {dim: true});
  if (d.coverage?.summary === undefined && !d.coverage?.gaps) push('  (none)', {dim: true});

  push('');
  push(`USAGE  tokens ${human(d.usage.input_tokens)} in / ${human(d.usage.output_tokens)} out  ` +
       `requests ${d.usage.requests ?? '-'}  cost $${d.cost_usd ?? '-'}`, {dim: true});

  push('');
  const tail = d.log_tail ?? [];
  push(`STRIX.LOG (last ${tail.length} lines)`, {bold: true});
  if (!tail.length) push('  (no log)', {dim: true});
  for (const line of tail) wrapPush('  ' + line.slice(0, 500), {dim: true, color: 'gray'});

  return {lines, streamEnd};
}

function App({interval}: {interval: number}) {
  const {exit} = useApp();
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);
  const [focus, setFocus] = useState<Focus>('run');
  const [message, setMessage] = useState<{text: string; kind: 'ok' | 'err' | 'info'} | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Target | null>(null);
  const [help, setHelp] = useState(false);
  const [containers, setContainers] = useState<Record<string, {cpu: string; mem: string}>>({});
  const [procs, setProcs] = useState<Record<number, {cpu: number | null; rss: string}>>({});
  const samples = useRef<Record<number, ProcSample>>({});
  const busy = useRef(false);

  const [mode, setMode] = useState<'list' | 'form' | 'verbose'>('list');
  const [form, setForm] = useState<NewEngagement>(BLANK_FORM);
  const [formRow, setFormRow] = useState(0);
  const [formError, setFormError] = useState<string | null>(null);
  const [detail, setDetail] = useState<ProgressDetail | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailTarget, setDetailTarget] = useState<{workdir: string; run: string} | null>(null);
  const [detailOffset, setDetailOffset] = useState(0);
  const [detailPinned, setDetailPinned] = useState(true);

  const refresh = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    try {
      const [snap, cont] = await Promise.all([fetchSnapshot(), fetchContainerMetrics()]);
      setSnapshot(snap);
      setContainers(cont);
      setError(null);

      const pids = new Set<number>();
      for (const run of snap.runs) if (run.pid) pids.add(run.pid);
      if (snap.dispatcher.pid) pids.add(snap.dispatcher.pid);
      const next: Record<number, {cpu: number | null; rss: string}> = {};
      for (const pid of pids) {
        const sample = await readProc(pid);
        const cpu = cpuPercent(sample, samples.current[pid] ?? null);
        if (sample) samples.current[pid] = sample;
        next[pid] = {cpu, rss: sample ? humanKb(sample.rssKb) : '-'};
      }
      setProcs(next);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      busy.current = false;
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), Math.max(1, interval) * 1000);
    return () => clearInterval(timer);
  }, [refresh, interval]);

  const loadDetail = useCallback(async (workdir: string, run: string) => {
    setDetailLoading(true);
    try {
      setDetail(await fetchProgressDetail(workdir, run));
      setDetailError(null);
    } catch (e) {
      setDetailError(e instanceof Error ? e.message : String(e));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  // Refetch the verbose view on the same interval, and only while it is open: the ordinary
  // poll stays on `runs --json` so the dashboard never pays for the message stream.
  useEffect(() => {
    if (mode !== 'verbose' || !detailTarget) return;
    const timer = setInterval(
      () => void loadDetail(detailTarget.workdir, detailTarget.run),
      Math.max(1, interval) * 1000,
    );
    return () => clearInterval(timer);
  }, [mode, detailTarget, loadDetail, interval]);

  const runs = snapshot?.runs ?? [];
  const index = Math.min(selected, Math.max(0, runs.length - 1));
  const run = runs[index] ?? null;
  const project = (() => {
    if (!run) return null;
    const found = snapshot?.cairn.projects.find(p => p.id === run.project);
    if (!found) return null;
    return {
      id: found.id, title: found.title, status: found.status,
      facts: found.fact_count ?? 0, hints: found.hint_count ?? 0,
      intents: found.intent_count ?? 0,
      open: (found.unclaimed_intent_count ?? 0) + (found.working_intent_count ?? 0),
    };
  })();

  const target: Target = {workdir: run?.workdir, run: run?.run, project: run?.project ?? undefined};

  const rows = process.stdout.rows ?? 40;
  const cols = process.stdout.columns ?? 132;
  const viewHeight = Math.max(5, rows - 6);
  const paneWidth = Math.max(24, cols - 50);
  const doc = useMemo(() => (detail ? buildDoc(detail, paneWidth) : null), [detail, paneWidth]);
  const maxOffset = doc ? Math.max(0, doc.lines.length - viewHeight) : 0;

  // Opening the view lands on the newest stream entry, which is what a live run is about.
  useEffect(() => {
    if (mode !== 'verbose' || !detailPinned || !doc) return;
    setDetailOffset(Math.max(0, doc.streamEnd - viewHeight + 1));
  }, [mode, detailPinned, doc, viewHeight]);

  const act = useCallback(async (label: string, fn: () => Promise<string>) => {
    setMessage({text: `${label}…`, kind: 'info'});
    try {
      const out = await fn();
      const line = out.trim().split('\n').filter(Boolean).pop() ?? `${label} done`;
      setMessage({text: line.replace(/^\s*[✓!✗]\s*/, ''), kind: 'ok'});
    } catch (e) {
      setMessage({text: e instanceof Error ? e.message : String(e), kind: 'err'});
    }
    void refresh();
  }, [refresh]);

  const openDetail = useCallback((r: RunProgress) => {
    setMode('verbose');
    setDetail(null);
    setDetailError(null);
    setDetailLoading(false);
    setDetailPinned(true);
    setDetailOffset(0);
    if (!r.workdir) {
      setDetailTarget(null);
      setDetailError('this run has no directory on disk');
      return;
    }
    setDetailTarget({workdir: r.workdir, run: r.run});
    void loadDetail(r.workdir, r.run);
  }, [loadDetail]);

  const submitForm = useCallback(async () => {
    const fields = form;
    const missing: string[] = [];
    if (!fields.target.trim()) missing.push('target');
    if (!fields.title.trim()) missing.push('title');
    if (fields.flow === 'engage' && !fields.goal.trim()) missing.push('goal');
    if (missing.length) {
      setFormError(`missing ${missing.join(', ')}`);
      return;
    }
    setFormError(null);
    setMode('list');
    setMessage({text: fields.flow === 'scan' ? 'starting scan…' : 'starting engagement…', kind: 'info'});
    try {
      if (fields.flow === 'scan') {
        setMessage({text: await startScan(fields), kind: 'ok'});
      } else {
        const log = await startEngage(fields);
        setMessage({text: `engagement started (full flow); log: ${log}`, kind: 'ok'});
      }
    } catch (e) {
      setMessage({text: e instanceof Error ? e.message : String(e), kind: 'err'});
    }
    void refresh();
  }, [form, refresh]);

  useInput((input, key) => {
    // A modal view owns every key while it is open: `q` must not quit and `d` must not
    // delete from inside the form.
    if (mode === 'form') {
      const last = 4;
      if (key.upArrow) return setFormRow(r => Math.max(0, r - 1));
      if (key.downArrow) return setFormRow(r => Math.min(last, r + 1));
      if (key.tab) return setFormRow(r => (r + 1) % (last + 1));
      if (key.escape) { setMode('list'); setFormError(null); return; }
      if (key.return) return void submitForm();
      if (formRow === 0) {
        if (key.leftArrow || key.rightArrow || input === ' ') {
          setForm(f => ({...f, flow: f.flow === 'scan' ? 'engage' : 'scan'}));
        }
        return;
      }
      if (formRow === 4) {
        const order: ScanMode[] = ['quick', 'standard', 'deep'];
        if (key.rightArrow || input === ' ') {
          setForm(f => ({...f, mode: order[(order.indexOf(f.mode) + 1) % order.length]}));
        } else if (key.leftArrow) {
          setForm(f => ({...f, mode: order[(order.indexOf(f.mode) + order.length - 1) % order.length]}));
        }
        return;
      }
      const edit = (change: (value: string) => string) => setForm(f => {
        if (formRow === 1) return {...f, target: change(f.target)};
        if (formRow === 2) return {...f, title: change(f.title)};
        return {...f, goal: change(f.goal)};
      });
      if (key.backspace || key.delete) return edit(v => v.slice(0, -1));
      // Accept a whole pasted run of printable characters, not just one keystroke.
      if (input && !/[\x00-\x1f\x7f]/.test(input)) return edit(v => v + input);
      return;
    }

    if (mode === 'verbose') {
      if (input === 'q' || (key.ctrl && input === 'c')) return exit();
      if (key.escape || key.return) { setMode('list'); return; }
      if (input === 'r') {
        if (detailTarget) void loadDetail(detailTarget.workdir, detailTarget.run);
        return;
      }
      const step = (delta: number) => {
        setDetailPinned(false);
        setDetailOffset(o => Math.min(maxOffset, Math.max(0, o + delta)));
      };
      if (key.upArrow || input === 'k') return step(-1);
      if (key.downArrow || input === 'j') return step(1);
      if (key.pageUp) return step(-viewHeight);
      if (key.pageDown) return step(viewHeight);
      if (input === 'g') { setDetailPinned(false); setDetailOffset(0); return; }
      if (input === 'G') { setDetailPinned(false); setDetailOffset(maxOffset); return; }
      return;
    }

    if (pendingDelete) {
      if (input === 'y') {
        const where = focus === 'graph' ? 'project' : 'run';
        const t = pendingDelete;
        setPendingDelete(null);
        void act(`deleting ${where}`, () => remove(focus, t));
      } else if (input === 'n' || key.escape) {
        setPendingDelete(null);
        setMessage({text: 'delete cancelled', kind: 'info'});
      }
      return;
    }
    if (input === 'q' || (key.ctrl && input === 'c')) return exit();
    if (input === '?') return setHelp(h => !h);
    if (input === 'n') {
      setForm(BLANK_FORM);
      setFormRow(1);
      setFormError(null);
      setMode('form');
      return;
    }
    if (key.upArrow || input === 'k') return setSelected(i => Math.max(0, i - 1));
    if (key.downArrow || input === 'j') return setSelected(i => Math.min(runs.length - 1, i + 1));
    if (key.tab) return setFocus(f => (f === 'run' ? 'graph' : 'run'));
    if (input === 'r') {
      setMessage({text: 'refreshed', kind: 'info'});
      return void refresh();
    }
    if (key.return) {
      if (run && focus === 'run') openDetail(run);
      return;
    }
    if (!run) return;
    if (input === 'p') {
      const paused = focus === 'graph' ? project?.status === 'stopped' : run.paused;
      if (focus === 'graph' && !project) return setMessage({text: 'no project linked to this run', kind: 'err'});
      return void act(paused ? 'resuming' : 'pausing', () => pauseOrResume(focus, target, paused));
    }
    if (input === 's') {
      if (focus === 'graph' && !project) return setMessage({text: 'no project linked to this run', kind: 'err'});
      if (focus === 'run' && !run.live) return setMessage({text: 'this run is not live', kind: 'err'});
      return void act('stopping', () => stop(focus, target));
    }
    if (input === 'd') {
      if (focus === 'graph' && !project) return setMessage({text: 'no project linked to this run', kind: 'err'});
      return setPendingDelete(target);
    }
    if (input === 'f') return void act('feeding', () => feed(target));
  });

  const sandbox = Object.entries(containers).find(([name]) => name !== 'triad-cairn-server');
  const cairn = containers['triad-cairn-server'];
  const scanPid = run?.pid ?? null;
  const dispatcherPid = snapshot?.dispatcher.pid ?? null;
  const metrics = {
    sandbox: sandbox ? `${sandbox[1].cpu.padStart(5)} cpu  ${sandbox[1].mem}` : 'no scan container',
    strix: scanPid && procs[scanPid]
      ? `${String(procs[scanPid].cpu?.toFixed(0) ?? '-').padStart(4)}% cpu  ${procs[scanPid].rss}`
      : 'no live process',
    dispatcher: dispatcherPid && procs[dispatcherPid]
      ? `${String(procs[dispatcherPid].cpu?.toFixed(0) ?? '-').padStart(4)}% cpu  ${procs[dispatcherPid].rss}`
      : 'stopped',
  };
  const cairnLine = cairn ? `  cairn ${cairn.cpu} cpu  ${cairn.mem}` : '';

  const clampedOffset = doc ? Math.min(detailOffset, maxOffset) : 0;
  const window = doc ? doc.lines.slice(clampedOffset, clampedOffset + viewHeight) : [];
  const hasDb = detail?.dir ? existsSync(join(detail.dir, '.state', 'agents.db')) : true;

  return (
    <Box flexDirection="column" height={rows - 1}>
      <Box justifyContent="space-between">
        <Text bold color="cyan">TRIAD</Text>
        <Text dimColor>
          {snapshot ? `${snapshot.cairn.base} ${snapshot.cairn.up ? 'up' : 'DOWN'}` : 'loading…'}
          {cairnLine}  dispatcher {snapshot?.dispatcher.alive ? 'up' : 'down'}
        </Text>
      </Box>

      <Box flexGrow={1} marginTop={1}>
        <Box flexDirection="column" width={46}>
          <Text bold>RUNS ({runs.length})</Text>
          {runs.length === 0 && <Text dimColor>  none</Text>}
          {runs.map((r, i) => <Row key={r.dir} run={r} selected={i === index} />)}
          <Box marginTop={1} flexDirection="column">
            <Text bold>PROJECTS ({snapshot?.cairn.projects.length ?? 0})</Text>
            {(snapshot?.cairn.projects ?? []).map(p => (
              <Text key={p.id} dimColor={p.id !== run?.project}>
                {'  '}{p.id} <Text color={p.status === 'active' ? 'green' : 'yellow'}>{p.status}</Text>
                {' '}{p.hint_count ?? 0}h {p.intent_count ?? 0}i
              </Text>
            ))}
          </Box>
        </Box>

        <Box flexDirection="column" flexGrow={1} borderStyle="round" borderColor="gray" paddingX={1}>
          {mode === 'form' ? (
            <Form fields={form} row={formRow} error={formError} />
          ) : mode === 'verbose' ? (
            <Box flexDirection="column">
              {detailError && <Text color="red">{detailError}</Text>}
              {!detail && !detailError && <Text dimColor>{detailLoading ? 'loading…' : 'no detail'}</Text>}
              {detail && !hasDb && <Text color="yellow">no agents.db yet for this run</Text>}
              {window.map((line, i) => (
                <Text key={clampedOffset + i} dimColor={line.dim} color={line.color} bold={line.bold}>
                  {line.text || ' '}
                </Text>
              ))}
            </Box>
          ) : error ? (
            <Text color="red">{error}</Text>
          ) : (
            <Detail run={run} project={project} focus={focus} metrics={metrics} />
          )}
        </Box>
      </Box>

      <Box flexDirection="column">
        {mode === 'form' ? (
          <Text dimColor>form open: esc cancels, no other key acts</Text>
        ) : mode === 'verbose' ? (
          <Text dimColor>esc back  ↑/↓ scroll  r refresh  q quit</Text>
        ) : (
          <>
            {help && (
              <Box flexDirection="column">
                {HELP.map(([key, what]) => (
                  <Text key={key} dimColor>{key.padEnd(12)}{what}</Text>
                ))}
              </Box>
            )}
            {pendingDelete && (
              <Text color="yellow">
                delete the {focus === 'graph' ? 'project' : 'run'} {focus === 'graph' ? run?.project : run?.run}? (y/n)
              </Text>
            )}
            {message && !pendingDelete && (
              <Text color={message.kind === 'err' ? 'red' : message.kind === 'ok' ? 'green' : 'gray'}>
                {message.kind === 'err' ? '✗ ' : message.kind === 'ok' ? '✓ ' : '  '}{message.text}
              </Text>
            )}
            <Text dimColor>n new  p pause  s stop  d delete  f feed  enter detail  tab target: {focus}  r refresh  ? help  q quit</Text>
          </>
        )}
      </Box>
    </Box>
  );
}

const intervalArg = process.argv.indexOf('--interval');
const interval = intervalArg > -1 ? Number(process.argv[intervalArg + 1]) : 3;

const app = render(<App interval={Number.isFinite(interval) ? interval : 3} />);
await app.waitUntilExit();
