// The left column's idle cartoon: one lab mascot that bobs while a scan runs and keeps a
// fixed-size speech bubble. Art and vocabulary live here so the dashboard stays a dashboard.
// No JSX in this file: it is `.ts`, so the component is assembled with createElement.
import React from 'react';
import {Box, Text} from 'ink';

export type Lab = {
  label: string;
  color: string;
  art: string[];
  words: string[];
};

// Three lines per lab, so every figure costs the same rows and the column never jumps.
// Eyes are literal `o`s: the sleeping pose is a single character swap.
export const LABS: Lab[] = [
  {
    label: 'deepseek',
    color: '#4D6BFE',
    art: [
      '   ___',
      '  (o o)___',
      '   \\___/~',
    ],
    words: ['pondering', 'percolating', 'marinating', 'noodling', 'moseying',
            'frolicking', 'simmering', 'drifting', 'bobbing', 'breaching',
            'dawdling', 'meandering', 'vibing'],
  },
  {
    label: 'anthropic',
    color: '#D97757',
    art: [
      '   /\\',
      '  /o o\\',
      ' /_/ \\_\\',
    ],
    words: ['ruminating', 'cogitating', 'tinkering', 'scheming', 'deliberating',
            'mulling', 'weighing', 'brooding', 'pontificating', 'deducing',
            'philosophising', 'contriving', 'pondering'],
  },
  {
    label: 'openai',
    color: '#10A37F',
    art: [
      '  \\_|_/',
      '  (o o)',
      '  /_|_\\',
    ],
    words: ['noodling', 'tinkering', 'scheming', 'vibing', 'synthesising',
            'orchestrating', 'wrangling', 'iterating', 'spinning', 'weaving',
            'knotting', 'conjuring', 'extrapolating'],
  },
];

const LAB_PERIOD_MS = 180_000;  // one lab holds the column for minutes, not per frame
const TICKS_PER_WORD = 40;       // the 120ms ticker lands a new word about every 4.8s
const TICKS_PER_BOB = 5;         // ~0.6s per bob step, so the figure visibly moves
const PAUSED_WORDS = ['napping', 'waiting', 'dozing', 'resting', 'idling', 'snoozing', 'loitering'];
const IDLE_WORD = 'dozing';

export function labIndex(now: number): number {
  const bucket = Math.floor(now / LAB_PERIOD_MS) % LABS.length;
  return (bucket + LABS.length) % LABS.length;
}

export type Pose = {lab: Lab; art: string[]; word: string; bob: number; sleeping: boolean};

// Everything the cartoon shows for a frame: which lab, which pose, which word, and whether
// it is bobbing. The caller supplies the ticker's frame, so there is no second timer.
export function mascotState(opts: {
  animating: boolean; paused: boolean; frame: number; now: number;
}): Pose {
  const {animating, paused, frame, now} = opts;
  const lab = LABS[labIndex(now)];
  if (paused || !animating) {
    return {
      lab,
      art: lab.art.map(closeEyes),
      word: paused ? PAUSED_WORDS[labIndex(now) % PAUSED_WORDS.length] : IDLE_WORD,
      bob: 0,
      sleeping: true,
    };
  }
  return {
    lab,
    art: lab.art,
    word: lab.words[Math.floor(frame / TICKS_PER_WORD) % lab.words.length],
    bob: Math.floor(frame / TICKS_PER_BOB) % 2,
    sleeping: false,
  };
}

function closeEyes(line: string): string {
  return line.replace(/o/g, '-');
}

const BUBBLE_INNER = 20;
const TAIL = 3;
const BUBBLE_INDENT = '  ';
const ART_COL = TAIL + 8;                     // art lines up under the pointer arrow
const ART_INDENT = ' '.repeat(ART_COL);
const POINTER = ' '.repeat(TAIL + 3) + '╰─▸';
const MIN_ROWS = 8;                           // bubble + bobbing slot + label

function fit(text: string, width: number): string {
  return text.length > width ? text.slice(0, width - 1) + '…' : text.padEnd(width);
}

// Bubble rows are padded to a fixed width, so a longer word never reflows the column.
function line(text: string, props: React.ComponentProps<typeof Text>, key: string) {
  return React.createElement(Text, {...props, key}, text);
}

export function Mascot({animating, paused, frame, leftRows, activity, now}: {
  animating: boolean;
  paused: boolean;
  frame: number;
  leftRows: number;
  activity?: string | null;
  now?: number;
}): React.JSX.Element | null {
  if (leftRows < MIN_ROWS) return null;
  const {lab, art, word, bob, sleeping} = mascotState({
    animating, paused, frame, now: now ?? Date.now(),
  });
  // Four rows for a three-line figure: the spare row moves top or bottom, which is the bob.
  const slot = bob ? ['', ...art] : [...art, ''];
  const showActivity = animating && leftRows > MIN_ROWS && Boolean(activity);
  const top = `${BUBBLE_INDENT}╭${'─'.repeat(BUBBLE_INNER)}╮`;
  const mid = `${BUBBLE_INDENT}(${fit(`  ${word}…`, BUBBLE_INNER)})`;
  const bottom = `${BUBBLE_INDENT}╰${'─'.repeat(TAIL)}╮${
    '─'.repeat(BUBBLE_INNER - TAIL - 1)}╯`;
  const figure = [
    `${POINTER}  ${slot[0]}`,
    `${ART_INDENT}${slot[1]}`,
    `${ART_INDENT}${slot[2]}`,
    `${ART_INDENT}${slot[3]}`,
  ];
  const children: React.ReactNode[] = [
    line(top, {dimColor: sleeping}, 'top'),
    line(mid, {dimColor: sleeping}, 'mid'),
    line(bottom, {dimColor: sleeping}, 'bottom'),
    ...figure.map((text, i) => line(text, {color: lab.color, dimColor: sleeping}, `fig${i}`)),
    line(`${ART_INDENT}${lab.label}`, {bold: true, color: lab.color}, 'label'),
  ];
  if (showActivity) {
    children.push(line(`${ART_INDENT}${fit(String(activity), 46 - ART_COL)}`, {dimColor: true}, 'doing'));
  }
  return React.createElement(
    Box,
    {flexDirection: 'column', flexGrow: 1, flexShrink: 0, overflow: 'hidden'},
    ...children,
  );
}
