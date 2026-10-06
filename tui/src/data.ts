// Data layer: one snapshot from the CLI, live metrics from Docker and /proc.
//
// State comes from `triad runs --json` rather than being re-derived here, so the
// dashboard and the CLI can never disagree about what is running.
import {spawn} from 'node:child_process';
import {readFile, readdir} from 'node:fs/promises';

export type RunProgress = {
  run: string;
  dir: string;
  workdir: string;
  status: string | null;
  start_time: string | null;
  end_time: string | null;
  turns: number | null;
  cost_usd: number | null;
  findings: number;
  findings_by_severity?: Record<string, number>;
  coverage_gaps: number;
  notes: number;
  live: boolean;
  paused: boolean;
  pid: number | null;
  project?: string | null;
  project_fed_at?: string | null;
  agents: {total: number; completed: number; running: string[]; waiting: number; failed: number};
  todos: {total: number; done: number; in_progress: number; pending: number};
  todos_detail?: {agent_id: string; agent_name: string; id: string; title: string | null;
                  status: string | null}[];
  usage: {requests: number | null; input_tokens: number | null; cached_tokens: number | null;
          output_tokens: number | null};
};

export type AgentMessage = {
  id: number;
  session_id: string;
  agent_name: string;
  role: string | null;
  type: string;
  text: string;
  tool: string | null;
  at: string | null;
  truncated: boolean;
};

// `progress --verbose` adds these on top of the summary RunProgress already carries.
export type ProgressDetail = RunProgress & {
  agents_detail?: {id: string; name: string; status: string; pending: number}[];
  todos_detail?: {agent_id: string; agent_name: string; id: string; title: string | null;
                  status: string | null}[];
  findings_detail?: {title: string | null; severity: string | null}[];
  coverage?: {summary?: unknown; gaps?: unknown[]};
  log_tail?: string[];
  notes_detail?: {id: string; title: string | null; agent_name: string | null}[];
  messages?: AgentMessage[];
};

export type Project = {
  id: string;
  title: string;
  status: string;
  fact_count?: number;
  hint_count?: number;
  intent_count?: number;
  unclaimed_intent_count?: number;
  working_intent_count?: number;
};

export type Snapshot = {
  root: string;
  cairn: {base: string; up: boolean; projects: Project[]};
  dispatcher: {pid: number | null; alive: boolean};
  runs: RunProgress[];
};

export type Metrics = {
  containers: Record<string, {cpu: string; mem: string}>;
  procs: Record<number, {cpu: number | null; rss: string}>;
};

const PYTHON = process.env.TRIAD_PYTHON ?? 'python3';
const TRIAD_PY = process.env.TRIAD_PY ?? 'triad.py';

export function runCommand(cmd: string, args: string[], timeoutMs = 20000): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {stdio: ['ignore', 'pipe', 'pipe']});
    let out = '';
    let err = '';
    const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
    child.stdout.on('data', (d: Buffer) => (out += d));
    child.stderr.on('data', (d: Buffer) => (err += d));
    child.on('error', e => {
      clearTimeout(timer);
      reject(e);
    });
    child.on('close', code => {
      clearTimeout(timer);
      if (code === 0) resolve(out);
      else reject(new Error(err.trim() || `${cmd} exited ${code}`));
    });
  });
}

export async function fetchSnapshot(): Promise<Snapshot> {
  return JSON.parse(await runCommand(PYTHON, [TRIAD_PY, 'runs', '--json']));
}

export function triadCommand(args: string[]): Promise<string> {
  return runCommand(PYTHON, [TRIAD_PY, ...args]);
}

// The CLI this app shells out to, so callers that need to spawn it detached (a full
// engagement runs for hours) get the same interpreter and script the polling path uses.
export function triadSpawn(args: string[]): {cmd: string; args: string[]} {
  return {cmd: PYTHON, args: [TRIAD_PY, ...args]};
}

// The dashboard no longer renders the log tail, so ask for zero lines: the flag and the
// `log_tail` key stay for other callers, but this view never pays to transfer them.
export async function fetchProgressDetail(workdir: string, run: string): Promise<ProgressDetail> {
  return JSON.parse(await runCommand(PYTHON, [TRIAD_PY, 'progress', '--verbose', '--json',
    '--workdir', workdir, '--run', run, '--messages', '200', '--log-lines', '0']));
}

// Containers: find the ones that matter (the scan sandbox, the Cairn server), then ask
// docker for their stats. `docker stats` with explicit names is quick; without them it
// walks every container on the host.
export async function fetchContainerMetrics(): Promise<Record<string, {cpu: string; mem: string}>> {
  try {
    const ps = await runCommand('docker', ['ps', '--format', '{{.Names}}\t{{.Image}}']);
    const names = ps
      .split('\n')
      .filter(Boolean)
      .map(line => line.split('\t'))
      .filter(([, image]) => /strix-sandbox|cairn/.test(image ?? ''))
      .map(([name]) => name);
    if (names.length === 0) return {};
    const stats = await runCommand('docker', ['stats', '--no-stream', '--format', '{{json .}}', ...names]);
    const out: Record<string, {cpu: string; mem: string}> = {};
    for (const line of stats.split('\n').filter(Boolean)) {
      const row = JSON.parse(line);
      out[row.Name] = {cpu: row.CPUPerc, mem: row.MemUsage};
    }
    return out;
  } catch {
    return {};
  }
}

const CLK_TCK = 100; // Linux default, and what every normal host uses

export type ProcSample = {ticks: number; rssKb: number; at: number};

export async function readProc(pid: number): Promise<ProcSample | null> {
  try {
    const stat = await readFile(`/proc/${pid}/stat`, 'utf8');
    // Field 14/15 are utime/stime, after the comm field which may itself contain spaces
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
    const ticks = Number(fields[11]) + Number(fields[12]);
    const status = await readFile(`/proc/${pid}/status`, 'utf8');
    const rssKb = Number(/VmRSS:\s+(\d+)/.exec(status)?.[1] ?? 0);
    return {ticks, rssKb, at: Date.now()};
  } catch {
    return null;
  }
}

// CPU% needs two samples: ticks are cumulative, so it is a delta over wall time.
export function cpuPercent(now: ProcSample | null, before: ProcSample | null): number | null {
  if (!now || !before) return null;
  const seconds = (now.at - before.at) / 1000;
  if (seconds <= 0) return null;
  return Math.max(0, ((now.ticks - before.ticks) / CLK_TCK / seconds) * 100);
}

export function human(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return '-';
  const units: [string, number][] = [['G', 1e9], ['M', 1e6], ['k', 1e3]];
  for (const [suffix, size] of units) {
    if (bytes >= size) {
      const value = (bytes / size).toFixed(1);
      return `${value.endsWith('.0') ? value.slice(0, -2) : value}${suffix}`;
    }
  }
  return String(bytes);
}

export function humanKb(kb: number): string {
  if (!kb) return '-';
  return human(kb * 1024);
}

export function elapsed(start: string | null, end: string | null): string {
  if (!start) return '-';
  const began = Date.parse(start);
  const stopped = end ? Date.parse(end) : Date.now();
  if (Number.isNaN(began)) return '-';
  const minutes = Math.max(0, Math.floor((stopped - began) / 60000));
  return minutes >= 60 ? `${Math.floor(minutes / 60)}h${String(minutes % 60).padStart(2, '0')}m`
                       : `${minutes}m`;
}

export async function engagementDirs(): Promise<string[]> {
  try {
    const entries = await readdir(process.env.TRIAD_WORKDIR ?? `${process.env.HOME}/engagements`,
                                 {withFileTypes: true});
    return entries.filter(e => e.isDirectory()).map(e => e.name);
  } catch {
    return [];
  }
}
