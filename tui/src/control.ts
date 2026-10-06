// Control actions. Every one of them goes through `triad control`, so the dashboard
// cannot invent a state change the CLI does not support.
import {triadCommand} from './data.js';

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
