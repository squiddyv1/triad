// Control actions. Every one of them goes through `triad control`, so the dashboard
// cannot invent a state change the CLI does not support.
import {spawn} from 'node:child_process';
import {closeSync, mkdirSync, openSync} from 'node:fs';
import {join} from 'node:path';
import {triadCommand, triadSpawn} from './data.js';

export type Focus = 'run' | 'graph';

export type Target = {workdir?: string; run?: string; project?: string};

export async function pauseOrResume(focus: Focus, target: Target, currentlyPaused: boolean) {
  const action = currentlyPaused ? 'resume' : 'pause';
  return focus === 'graph'
    ? triadCommand(['control', action, '--project', target.project!])
    : triadCommand(['control', action, '--workdir', target.workdir!]);
}

export async function stop(focus: Focus, target: Target) {
  return focus === 'graph'
    ? triadCommand(['control', 'stop', '--project', target.project!])
    : triadCommand(['control', 'stop', '--workdir', target.workdir!]);
}

export async function remove(focus: Focus, target: Target) {
  return focus === 'graph'
    ? triadCommand(['control', 'delete', '--project', target.project!])
    : triadCommand(['control', 'delete', '--workdir', target.workdir!, '--run', target.run!]);
}

export async function feed(target: Target) {
  if (!target.project) {
    throw new Error('no Cairn project is linked to this run: triad engage links one, or pass one to `triad feed`');
  }
  return triadCommand(['feed', '--project', target.project, '--workdir', target.workdir!]);
}

export type Flow = 'scan' | 'engage';
export type ScanMode = 'quick' | 'standard' | 'deep';

export type NewEngagement = {
  flow: Flow;
  target: string;
  title: string;
  goal: string;
  mode: ScanMode;
};

// The CLI's own slug rule (`_slug` in triad.py): lowercase, non-alphanumerics to '-',
// trim the dashes, cap at 48, and fall back so the directory is never empty.
function slug(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 48)
    || 'engagement';
}

// Mirrors `_engage_workdir`: $TRIAD_WORKDIR (or ~/engagements) joined with the title slug.
export function engagementWorkdir(fields: NewEngagement): string {
  const home = process.env.HOME ?? '';
  const root = (process.env.TRIAD_WORKDIR ?? '~/engagements').replace(/^~(?=\/|$)/, home);
  return join(root, slug(fields.title));
}

export function scanArgs(fields: NewEngagement): string[] {
  return ['scan', '--target', fields.target, '--workdir', engagementWorkdir(fields),
          '--mode', fields.mode];
}

export function engageArgs(fields: NewEngagement): string[] {
  return ['engage', '--title', fields.title, '--target', fields.target, '--goal', fields.goal,
          '--workdir', engagementWorkdir(fields), '--mode', fields.mode];
}

// `triad scan` returns as soon as Strix is launched, so this resolves with its
// `strix started pid=...` line once the directory and pid file are on disk.
export async function startScan(fields: NewEngagement): Promise<string> {
  mkdirSync(engagementWorkdir(fields), {recursive: true});
  const out = await triadCommand(scanArgs(fields));
  const line = out.split('\n').map(l => l.trim()).find(l => l.startsWith('strix started'));
  return line ?? out.trim().split('\n').filter(Boolean).pop() ?? 'scan launched';
}

// The engage flow runs for the whole engagement, so it is spawned detached and its output
// appended to a log; the dashboard reports the path instead of holding the process open.
export async function startEngage(fields: NewEngagement): Promise<string> {
  const dir = engagementWorkdir(fields);
  mkdirSync(dir, {recursive: true});
  const log = join(dir, 'triad-engage.log');
  const fd = openSync(log, 'a');
  const {cmd, args} = triadSpawn(engageArgs(fields));
  const child = spawn(cmd, args, {detached: true, stdio: ['ignore', fd, fd]});
  child.unref();
  closeSync(fd);
  return log;
}
