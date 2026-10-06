// Triad dashboard: every run, its progress, its telemetry, its controls.
//
// Rows are Strix runs; the detail pane follows the selection. State is polled from
// `triad runs --json`; the numbers on the right come from Docker and /proc.
import React, {useCallback, useEffect, useRef, useState} from 'react';
import {Box, Text, render, useApp, useInput} from 'ink';
import {
  cpuPercent, elapsed, fetchContainerMetrics, fetchSnapshot, human, humanKb, readProc,
  type ProcSample, type RunProgress, type Snapshot,
} from './data.js';
import {feed, pauseOrResume, remove, stop, type Focus, type Target} from './control.js';

const HELP = [
  ['↑/↓  k/j', 'select a run'],
  ['tab', 'switch the target between the run and its graph'],
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

  useInput((input, key) => {
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
    if (key.upArrow || input === 'k') return setSelected(i => Math.max(0, i - 1));
    if (key.downArrow || input === 'j') return setSelected(i => Math.min(runs.length - 1, i + 1));
    if (key.tab) return setFocus(f => (f === 'run' ? 'graph' : 'run'));
    if (input === 'r') {
      setMessage({text: 'refreshed', kind: 'info'});
      return void refresh();
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

  const rows = process.stdout.rows ?? 40;
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
          {error
            ? <Text color="red">{error}</Text>
            : <Detail run={run} project={project} focus={focus} metrics={metrics} />}
        </Box>
      </Box>

      <Box flexDirection="column">
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
        <Text dimColor>p pause  s stop  d delete  f feed  tab target: {focus}  r refresh  ? help  q quit</Text>
      </Box>
    </Box>
  );
}

const intervalArg = process.argv.indexOf('--interval');
const interval = intervalArg > -1 ? Number(process.argv[intervalArg + 1]) : 3;

const app = render(<App interval={Number.isFinite(interval) ? interval : 3} />);
await app.waitUntilExit();
