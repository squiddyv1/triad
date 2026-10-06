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
import {Mascot} from './mascot.js';

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

const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// A stable colour per agent: the hash is by name, so an agent keeps its colour across
// frames and across runs that reuse a name.
const AGENT_PALETTE = ['cyan', 'magenta', 'green', 'yellow', 'blue', 'white'];

const SEVERITY_COLOUR: Record<string, string> = {
  critical: 'magenta', high: 'red', medium: 'yellow', low: 'cyan', info: 'gray',
  informational: 'gray',
};

// What a failed-looking tool result reads like. Deliberately narrow: these are the words
// Strix uses when it reports an error, not every occurrence of "fail" in prose.
const FAILURE_RE = /\b(error|failed|failure|exception|traceback|denied|refused|not found|no such file)\b/i;

function agentColour(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return AGENT_PALETTE[hash % AGENT_PALETTE.length];
}

function severityColour(severity: string | null | undefined): string {
  return SEVERITY_COLOUR[String(severity ?? '').toLowerCase()] ?? 'gray';
}

function Spinner({frame, color}: {frame: number; color?: string}) {
  const index = ((frame % SPINNER_FRAMES.length) + SPINNER_FRAMES.length) % SPINNER_FRAMES.length;
  return <Text color={color}>{SPINNER_FRAMES[index]}</Text>;
}

type Span = {text: string; color?: string; dim?: boolean; bold?: boolean};
type Line = Span[];

// Wrap spans to the pane width, breaking at spaces when possible and hard-breaking any
// token longer than the width. Every rendered line is therefore <= width, so Ink never
// has to wrap (and never clips past the pane border).
function wrapSpans(spans: Span[], width: number): Line[] {
  const limit = Math.max(4, width);
  const lines: Line[] = [];
  // Preserve the leading indent of the logical line on every continuation, so wrapped
  // findings and stream bodies stay under their own heading instead of hitting column 0.
  const head = spans.length ? (/^\s*/.exec(spans[0].text)?.[0] ?? '') : '';
  let current: Line = [];
  let length = 0;
  const flush = () => {
    lines.push(current);
    current = head ? [{text: head}] : [];
    length = head ? head.length : 0;
  };
  const add = (text: string, style: Span) => {
    let rest = text;
    while (rest.length) {
      const room = limit - length;
      if (room <= 0) { flush(); continue; }
      const chunk = rest.slice(0, room);
      current.push({...style, text: chunk});
      length += chunk.length;
      rest = rest.slice(chunk.length);
      if (rest.length) flush();
    }
  };
  for (const span of spans) {
    const parts = span.text.split('\n');
    for (let p = 0; p < parts.length; p++) {
      if (p > 0) flush();
      const words = parts[p].split(' ');
      for (let w = 0; w < words.length; w++) {
        const word = words[w];
        const sep = w > 0 ? ' ' : '';
        if (!word) { if (sep) add(sep, span); continue; }
        if (length + sep.length + word.length <= limit) {
          if (sep) add(sep, span);
          add(word, span);
        } else {
          if (length > 0) flush();
          add(word, span);
        }
      }
    }
  }
  if (current.length) lines.push(current);
  return lines.length ? lines : [[]];
}

// Status has a hard three-row budget, so it is cut to the pane width rather than wrapped:
// a wrapped status would quietly add rows and start eating the stream again.
function clampLine(spans: Span[], width: number): Line {
  const out: Line = [];
  let used = 0;
  for (const span of spans) {
    if (used >= width) break;
    const room = width - used;
    if (span.text.length <= room) {
      out.push({...span});
      used += span.text.length;
    } else {
      out.push({...span, text: `${span.text.slice(0, Math.max(0, room - 1))}…`});
      break;
    }
  }
  return out.length ? out : [{text: ' '}];
}

function gapText(gap: unknown): string {
  if (typeof gap === 'string') return gap;
  if (gap && typeof gap === 'object') {
    const o = gap as {message?: string; title?: string; rule?: string};
    return o.message || o.title || o.rule || JSON.stringify(gap);
  }
  return String(gap);
}

function severityTally(findings: {severity?: string | null}[]): string {
  const order = ['critical', 'high', 'medium', 'low', 'info'];
  const counts = new Map<string, number>();
  for (const f of findings) {
    const key = String(f.severity ?? '?').toLowerCase();
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts.entries()]
    .sort((a, b) => (order.indexOf(a[0]) + 1 || 99) - (order.indexOf(b[0]) + 1 || 99))
    .map(([sev, n]) => `${sev} ${n}`)
    .join(' · ');
}

function LineView({line}: {line: Line}) {
  if (!line.length) return <Text> </Text>;
  return (
    <Text>
      {line.map((s, i) => (
        <Text key={i} color={s.color} dimColor={s.dim} bold={s.bold}>{s.text}</Text>
      ))}
    </Text>
  );
}

// The title rides in the top border, so a pane costs exactly one title row plus a bottom
// border no matter how long its name is.
function borderLine(title: string, width: number): string {
  const label = ` ${fit(title, Math.max(0, width - 4)).trimEnd()} `;
  return `╭${label}${'─'.repeat(Math.max(0, width - 2 - label.length))}╮`;
}

// A pane whose content is taller than its window says where the reader is: the last visible
// line over the total, plus an arrow for hidden content on either side. A pane that fits
// adds nothing, so a title only carries a marker when something is actually out of sight.
function overflowMark(offset: number, total: number, visible: number): string {
  if (visible <= 0 || total <= visible) return '';
  const last = Math.min(total, offset + visible);
  const up = offset > 0 ? '↑ ' : '';
  const down = last < total ? ` ↓ ${total - last} more` : '';
  return `${up}${last}/${total}${down}`;
}

// One scrolling region of the split detail body: a titled top border, a fixed content area
// clipped to its own height, and a bottom border. Content is pre-wrapped to the inner width.
function Pane({title, width, contentHeight, focused, lines}: {
  title: string; width: number; contentHeight: number; focused: boolean; lines: Line[];
}) {
  const colour = focused ? 'cyan' : 'gray';
  return (
    <Box flexDirection="column" flexShrink={0}>
      <Text color={colour}>{borderLine(title, width)}</Text>
      <Box
        flexDirection="column"
        borderStyle="round"
        borderTop={false}
        borderColor={colour}
        height={contentHeight + 1}
        overflow="hidden"
      >
        {lines.map((line, i) => <LineView key={i} line={line} />)}
      </Box>
    </Box>
  );
}

function Row({run, selected, frame}: {run: RunProgress; selected: boolean; frame: number}) {
  const state = stateOf(run);
  const colour = STATE_COLOUR[state] ?? 'gray';
  return (
    <Box>
      <Text color={selected ? 'cyan' : undefined}>{selected ? '▸ ' : '  '}</Text>
      {run.live && !run.paused
        ? <><Spinner frame={frame} color={colour} /><Text color={colour}> </Text></>
        : run.paused
          ? <Text color={colour}>‖ </Text>
          : <Text color={colour}>○ </Text>}
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

function coverageBits(d: ProgressDetail) {
  const summary = (d.coverage?.summary && typeof d.coverage.summary === 'object')
    ? d.coverage.summary as {surfaces_reviewed?: number; findings_filed?: number; gaps?: number}
    : {};
  const gaps = Array.isArray(d.coverage?.gaps) ? d.coverage.gaps : [];
  // The count follows the list the pane renders: when the run supplied gaps, use their
  // length, so a heading can never say more gaps than it shows.
  const gapCount = gaps.length || (typeof summary.gaps === 'number' ? summary.gaps : 0);
  return {summary, gaps, gapCount};
}

// The status block is three clamped rows by design: it used to grow a line per field and
// squeeze the stream down to a few rows. The state is handed in from the snapshot, because
// the detail payload does not carry `live`/`paused` and deriving it here would read "stale"
// for a run the list row still animates.
function buildStatus(d: ProgressDetail, width: number, state: string): Line[] {
  const findings = d.findings_detail ?? [];
  const {summary, gapCount} = coverageBits(d);
  return [
    clampLine([
      {text: ' '},
      {text: d.run, bold: true, color: 'cyan'},
      {text: '  '},
      {text: state, color: STATE_COLOUR[state] ?? 'gray'},
      {text: `  ${elapsed(d.start_time, d.end_time)}  pid ${d.pid ?? '-'}`, dim: true},
    ], width),
    clampLine([
      {text: ' '},
      {text: `agents ${d.agents.completed}/${d.agents.total} done · todos ${d.todos.done}/${d.todos.total}`},
      ...(d.agents.failed ? [{text: ` · ${d.agents.failed} failed`, color: 'red'}] : []),
    ], width),
    clampLine([
      {text: ' coverage '},
      {text: `${summary.surfaces_reviewed ?? 0} surfaces · ${summary.findings_filed ?? findings.length} filed · `},
      {text: `${gapCount} gap${gapCount === 1 ? '' : 's'}`, color: gapCount ? 'yellow' : 'green'},
      {text: ' · '},
      {text: `usage ${human(d.usage.input_tokens)} in / ${human(d.usage.output_tokens)} out`, dim: true},
      {text: ` · $${d.cost_usd ?? '-'}`, dim: true},
    ], width),
  ];
}

// The findings pane is status, then findings, then coverage gaps, with no cap on either list:
// it scrolls, so a run with a hundred findings shows all of them without hiding the stream.
// The pane border already names the list and counts it, so the content line carries only the
// severity breakdown.
function buildFindings(d: ProgressDetail, width: number, state: string): Line[] {
  const lines = buildStatus(d, width, state);
  const push = (spans: Span[]) => { for (const line of wrapSpans(spans, width)) lines.push(line); };
  const findings = d.findings_detail ?? [];

  if (findings.length) push([{text: ` severity  ${severityTally(findings)}`, dim: true}]);
  for (const f of findings) {
    const colour = severityColour(f.severity);
    push([
      {text: '  '},
      {text: `[${String(f.severity ?? '?').toLowerCase()}] `, color: colour},
      {text: f.title ?? '(untitled)', color: colour},
    ]);
  }

  const {gaps, gapCount} = coverageBits(d);
  push([{text: ` COVERAGE GAPS (${gapCount})`, bold: true, color: 'white'}]);
  for (const gap of gaps) push([{text: '  · '}, {text: gapText(gap), color: 'yellow'}]);
  return lines;
}

function buildStream(d: ProgressDetail, width: number): Line[] {
  const lines: Line[] = [];
  const push = (spans: Span[]) => { for (const line of wrapSpans(spans, width)) lines.push(line); };
  const messages = d.messages ?? [];
  if (!messages.length) {
    push([{text: '  (no agent messages yet)', dim: true}]);
    return lines;
  }
  for (const m of messages) {
    const failed = m.type === 'function_call_output' && FAILURE_RE.test(m.text ?? '');
    const name = m.agent_name || '?';
    const stamp = (m.at ?? '').slice(11, 19);
    const head: Span[] = [{text: '  '}];
    if (stamp) head.push({text: `${stamp} `, dim: true});
    head.push({text: name, color: agentColour(name)});
    head.push({text: ' · ', dim: true});
    if (m.type === 'function_call') {
      head.push({text: 'call ', color: 'cyan'});
      head.push({text: m.tool ?? '?', color: 'cyan', bold: true});
    } else if (m.type === 'function_call_output') {
      head.push({text: 'result', color: failed ? 'red' : undefined, dim: !failed});
    } else if (m.type === 'reasoning') {
      head.push({text: 'reasoning', dim: true});
    } else {
      head.push({text: m.type});
    }
    if (m.truncated) head.push({text: ' [truncated]', dim: true});
    push(head);

    let dim = false;
    let colour: string | undefined;
    if (m.type === 'reasoning' || m.type === 'function_call') dim = true;
    else if (m.type === 'function_call_output') {
      if (failed) colour = 'red';
      else dim = true;
    }
    const body = (m.text ?? '').replace(/\r/g, '');
    for (const raw of (body.length ? body.split('\n') : ['(empty)'])) {
      push([{text: '    '}, {text: raw.replace(/\t/g, '  '), color: colour, dim}]);
    }
    push([]);
  }
  return lines;
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
  const [detailNew, setDetailNew] = useState(0);
  const [detailFollow, setDetailFollow] = useState(true);
  const [detailPane, setDetailPane] = useState<'findings' | 'stream'>('stream');
  const [findingsOffset, setFindingsOffset] = useState(0);
  const [frame, setFrame] = useState(0);
  const scrollReset = useRef(true);

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
  const liveCount = runs.filter(r => r.live || r.paused).length;
  // Only a scan that is actually running animates. A paused one is still "live" on disk,
  // and ticking it would read as work that is not happening.
  const animatingCount = runs.filter(r => r.live && !r.paused).length;

  // The ticker only exists while something is animating: nothing animating means no
  // timer, no re-renders, and a dashboard that costs nothing when idle.
  useEffect(() => {
    if (animatingCount === 0) return undefined;
    const timer = setInterval(() => setFrame(f => f + 1), 120);
    return () => clearInterval(timer);
  }, [animatingCount]);

  const activeRun = runs.find(r => r.live || r.paused) ?? null;

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
  // -50 leaves the RUNS column (46) plus the outer pane border and padding, so this is the
  // width the two panes share. Each pane's border eats two more columns for its inner text.
  const bodyWidth = Math.max(24, cols - 50);
  const paneInner = Math.max(20, bodyWidth - 2);
  // The cartoon only gets the rows the RUNS and PROJECTS lists leave free.
  const projectCount = snapshot?.cairn.projects.length ?? 0;
  const leftRows = Math.max(0, (rows - 4) - (runs.length + projectCount + 3));

  const hasDb = detail?.dir ? existsSync(join(detail.dir, '.state', 'agents.db')) : true;

  const detailRun = detailTarget
    ? runs.find(r => r.workdir === detailTarget.workdir && r.run === detailTarget.run) ?? null
    : null;
  const detailLive = Boolean(detailRun?.live || detailRun?.paused);
  const detailPaused = Boolean(detailRun?.paused);
  // The pane's own state line reads from the same snapshot run as the list row, so a live
  // scan can never show `scanning` up top and `stale` in the pane.
  const detailState = detailRun ? stateOf(detailRun) : detail ? stateOf(detail) : 'unknown';
  const currentText = detail?.agents.running?.[0]
    ?? detail?.todos_detail?.find(t => t.status === 'in_progress')?.title
    ?? 'working';
  const liveRow = detailLive ? 1 : 0;
  const warnRow = detail && !hasDb ? 1 : 0;

  // The upper pane takes max(6, 40%) of the body; the stream keeps the rest. On a 42-row
  // terminal that is 14 rows of findings and 22 of stream, so the stream stays the majority.
  const paneRows = Math.max(8, viewHeight - liveRow - warnRow);
  const upperRows = Math.max(6, Math.floor(paneRows * 0.4));
  const streamRows = paneRows - upperRows;
  const findingsHeight = Math.max(1, upperRows - 2);
  const streamHeight = Math.max(1, streamRows - 2);

  const findings = useMemo(
    () => (detail ? buildFindings(detail, paneInner, detailState) : []),
    [detail, paneInner, detailState],
  );
  const stream = useMemo(() => (detail ? buildStream(detail, paneInner) : []), [detail, paneInner]);

  const findingsMax = Math.max(0, findings.length - findingsHeight);
  const clampedFindingsOffset = Math.min(findingsOffset, findingsMax);
  const maxOffset = Math.max(0, stream.length - streamHeight);
  const clampedOffset = Math.min(detailOffset, maxOffset);
  const findingsMark = overflowMark(clampedFindingsOffset, findings.length, findingsHeight);
  const streamMark = overflowMark(clampedOffset, stream.length, streamHeight);
  const findingsCount = detail?.findings_detail?.length ?? detail?.findings ?? 0;
  const messageCount = (detail?.messages ?? []).length;
  const topMessageId = detail?.messages?.length ? detail.messages[detail.messages.length - 1].id : 0;

  // Follow the tail only when the follow intent and the live offset agree. Judging "at the
  // bottom" from the offset and the previously rendered length means a stale flag cannot
  // pin the view; counting new messages by id survives the 200-message window sliding.
  const prevMaxRef = useRef(0);
  const prevLenRef = useRef(0);
  const baselineIdRef = useRef(0);
  useEffect(() => {
    if (!detail) return;
    const newMax = Math.max(0, stream.length - streamHeight);
    if (scrollReset.current) {
      scrollReset.current = false;
      prevMaxRef.current = newMax;
      prevLenRef.current = stream.length;
      baselineIdRef.current = topMessageId;
      setDetailOffset(newMax);
      setDetailFollow(true);
      setDetailNew(0);
      return;
    }
    const wasAtBottom = detailOffset >= prevMaxRef.current;
    const grew = stream.length > prevLenRef.current;
    const shift = stream.length - prevLenRef.current;
    prevMaxRef.current = newMax;
    prevLenRef.current = stream.length;
    // Follow only while the stream is focused. Tabbing to findings freezes the stream: a
    // new arrival counts as paused instead of moving either pane.
    if (detailFollow && wasAtBottom && (detailPane === 'stream' || !grew)) {
      if (detailPane === 'stream') setDetailOffset(newMax);
      setDetailNew(0);
      if (grew) baselineIdRef.current = topMessageId;
      return;
    }
    if (detailFollow) setDetailFollow(false);
    if (baselineIdRef.current === 0) baselineIdRef.current = topMessageId;
    setDetailNew((detail.messages ?? []).filter(m => m.id > baselineIdRef.current).length);
    // When the 200-message window slides, lines leave the top: move the paused offset by
    // the same amount so the messages on screen do not jump under the reader.
    const target = Math.max(0, Math.min(newMax, detailOffset + Math.min(0, shift)));
    if (target !== detailOffset) setDetailOffset(target);
  }, [detail, stream.length, streamHeight, detailOffset, detailFollow, detailPane, topMessageId]);

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
    scrollReset.current = true;
    setDetailOffset(0);
    setDetailNew(0);
    setDetailFollow(true);
    setDetailPane('stream');
    setFindingsOffset(0);
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
      // tab moves between the two panes; the list view keeps its own tab meaning.
      if (key.tab) {
        setDetailPane(p => (p === 'findings' ? 'stream' : 'findings'));
        return;
      }
      // Scrolling up pauses the stream; reaching the bottom again (or End) resumes. Home
      // stops at the top. Findings scrolling never touches the stream's follow state.
      const step = (delta: number) => {
        const next = Math.min(maxOffset, Math.max(0, detailOffset + delta));
        if (next >= maxOffset) {
          setDetailFollow(true);
          setDetailNew(0);
        } else {
          setDetailFollow(false);
          baselineIdRef.current = topMessageId;
        }
        setDetailOffset(next);
      };
      const stepFindings = (delta: number) => {
        setFindingsOffset(o => Math.min(findingsMax, Math.max(0, o + delta)));
      };
      const scrolling = detailPane === 'findings'
        ? {step: stepFindings, page: findingsHeight}
        : {step, page: streamHeight};
      if (key.upArrow || input === 'k') return scrolling.step(-1);
      if (key.downArrow || input === 'j') return scrolling.step(1);
      if (key.pageUp) return scrolling.step(-scrolling.page);
      if (key.pageDown) return scrolling.step(scrolling.page);
      if (key.home || input === 'g') {
        if (detailPane === 'findings') return setFindingsOffset(0);
        setDetailFollow(false);
        baselineIdRef.current = topMessageId;
        setDetailNew(0);
        return setDetailOffset(0);
      }
      if (key.end || input === 'G') {
        if (detailPane === 'findings') return setFindingsOffset(findingsMax);
        setDetailFollow(true);
        setDetailNew(0);
        return setDetailOffset(maxOffset);
      }
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

  const findingsWindow = findings.slice(clampedFindingsOffset, clampedFindingsOffset + findingsHeight);
  const streamWindow = stream.slice(clampedOffset, clampedOffset + streamHeight);

  return (
    <Box flexDirection="column" height={rows - 1}>
      <Box justifyContent="space-between">
        <Box>
          {animatingCount > 0 ? (
            <>
              <Spinner frame={frame} color="cyan" />
              <Text bold color="cyan"> TRIAD</Text>
              <Text dimColor> scanning · {liveCount} live</Text>
            </>
          ) : (
            <Text bold color="cyan">TRIAD</Text>
          )}
        </Box>
        <Text dimColor>
          {snapshot ? `${snapshot.cairn.base} ${snapshot.cairn.up ? 'up' : 'DOWN'}` : 'loading…'}
          {cairnLine}  dispatcher {snapshot?.dispatcher.alive ? 'up' : 'down'}
        </Text>
      </Box>

      <Box flexGrow={1} marginTop={1}>
        <Box flexDirection="column" width={46}>
          <Box flexDirection="column" flexShrink={0}>
            <Text bold>RUNS ({runs.length})</Text>
            {runs.length === 0 && <Text dimColor>  none</Text>}
            {runs.map((r, i) => <Row key={r.dir} run={r} selected={i === index} frame={frame} />)}
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
          <Mascot
            animating={animatingCount > 0}
            paused={animatingCount === 0 && liveCount > 0}
            frame={frame}
            leftRows={leftRows}
            activity={activeRun?.agents.running[0] ?? null}
          />
        </Box>

        <Box flexDirection="column" flexGrow={1} borderStyle="round" borderColor="gray" paddingX={1}>
          {mode === 'form' ? (
            <Form fields={form} row={formRow} error={formError} />
          ) : mode === 'verbose' ? (
            <Box flexDirection="column">
              {detailError && <Text color="red">{detailError}</Text>}
              {!detail && !detailError && <Text dimColor>{detailLoading ? 'loading…' : 'no detail'}</Text>}
              {detail && !hasDb && <Text color="yellow">no agents.db yet for this run</Text>}
              {detail && (
                <>
                  {detailLive && (
                    <Box>
                      {detailPaused
                        ? <Text color={STATE_COLOUR.paused}>‖ </Text>
                        : <Spinner frame={frame} color="cyan" />}
                      <Text dimColor> {detailPaused ? 'paused' : 'scanning'}  </Text>
                      <Text color={detailPaused ? STATE_COLOUR.paused : 'cyan'}>{currentText}</Text>
                    </Box>
                  )}
                  <Pane
                    title={`FINDINGS (${findingsCount})${findingsMark ? `  ${findingsMark}` : ''}`}
                    width={bodyWidth}
                    contentHeight={findingsHeight}
                    focused={detailPane === 'findings'}
                    lines={findingsWindow}
                  />
                  <Pane
                    title={`AGENT STREAM (${messageCount})${streamMark ? `  ${streamMark}` : ''}`}
                    width={bodyWidth}
                    contentHeight={streamHeight}
                    focused={detailPane === 'stream'}
                    lines={streamWindow}
                  />
                </>
              )}
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
          <Text dimColor>
            esc back  tab pane  ↑/↓ scroll  g/G top/end  r refresh  pane: {detailPane}  {'  '}
            {detailFollow
              ? 'follow: on (tail -f)'
              : detailNew > 0
                ? `paused · ↓ ${detailNew} new`
                : 'paused · ↑ scrolled'}  q quit
          </Text>
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
