// The left column's idle cartoon: one lab mascot that comes alive while a scan runs, over a
// fixed-size speech bubble. Art, vocabulary and the frame-to-pose logic live here so the
// dashboard stays a dashboard. No JSX in this file: it is `.ts`, so the component is
// assembled with createElement.
//
// Every figure draws exactly FIG_ROWS lines from a single deterministic frame number, so a
// given frame always renders the same picture and the column never jumps between labs.
import React from 'react';
import {Box, Text} from 'ink';

export type Lab = {
  color: string;
  ink: string;
  words: string[];
  // frame is the shared Ink ticker; resting is the closed, still pose (sleeping or paused).
  art: (frame: number, resting: boolean) => string[];
};

const FIG_ROWS = 13;  // the shared figure height; every lab fills it, so nothing reflows

// Markers keep the art readable while letting the eyes and shades take the ink colour and
// the body take the lab colour. A marker stands for one glyph; anything else is body. The
// renderer only ever maps a character through this table, never through Math.random, so a
// frame is stable.
const INK_GLYPH: Record<string, string> = {
  X: '█', U: '▀', L: '▄',   // the Dot's solid lenses
  E: '●',                    // the whale's eye
  G: '▶', H: '◀',            // the Anthropic creature's inward-pointing eyes
  C: '▬',                    // a closed eye or a squinting lens
  B: '─',                    // the bright bridge between the Dot's lenses
  V: '·', W: '°',            // the whale's spout
};
// A body-coloured glyph: a catch light inside a black lens, the way glass glints.
const GLINT_GLYPH: Record<string, string> = {O: '●'};

function blank(width: number, height: number): string[][] {
  return Array.from({length: height}, () => Array.from({length: width}, () => ' '));
}

function lines(grid: string[][]): string[] {
  return grid.map(row => row.join('').replace(/\s+$/, ''));
}

function paint(grid: string[][], rows: [number, number], cols: [number, number], ch: string): void {
  for (let r = rows[0]; r <= rows[1]; r++) {
    const row = grid[r];
    if (!row) continue;
    for (let c = cols[0]; c <= cols[1]; c++) if (c >= 0 && c < row.length) row[c] = ch;
  }
}

// Anthropic, the Claude creature: a blocky, symmetric pixel animal. One rectangular body
// with no neck, arm tabs low on both sides, two short legs with a gap, and eyes drawn as
// filled triangles pointing inward at each other. It blinks, walks its legs and lifts its
// arms on independent periods.
const ANTHROPIC_W = 16;

function anthropicArt(frame: number, resting: boolean): string[] {
  const g = blank(ANTHROPIC_W, FIG_ROWS);
  const eyesClosed = resting || frame % 20 < 2;
  const step = resting ? 0 : Math.floor(frame / 3) % 3;        // 0 stand, 1 left up, 2 right up
  const lifted = !resting && Math.floor(frame / 8) % 2 === 1;  // arms ride a row higher
  const armRows: [number, number] = lifted ? [2, 4] : [3, 5];

  paint(g, [0, 8], [3, 12], '█');       // the body, taller than it is wide
  paint(g, armRows, [0, 15], '█');      // the arm band, the widest row, sticking out both sides
  g[3][4] = eyesClosed ? 'C' : 'G';     // left eye, a triangle pointing right
  g[3][11] = eyesClosed ? 'C' : 'H';    // right eye, a triangle pointing left
  paint(g, [9, 12], [5, 6], '█');       // left leg
  paint(g, [9, 12], [9, 10], '█');      // right leg, a two-column gap between them
  if (!resting && step === 1) g[12][5] = ' ';   // the left foot lifts
  if (!resting && step === 2) g[12][10] = ' ';  // the right foot lifts
  return lines(g);
}

// DeepSeek, the blue whale: the mark is a whale in side profile, drawn as a sweeping arc.
// Thin at the rostrum low on the left, thickest through the middle, back convex and belly
// concave, tail forking into two flukes high on the right. The eye only appears at this
// size, and the spout rises from the blowhole.
const WHALE_W = 26;
const WHALE_BODY = [
  '       ▄▄ ▄▄▄▄   ▄█       ',
  '   ▄█████████    ███▄ ▄▄▄█',
  ' ▄████████████▄  ▀████████',
  '▄███████████████▄ ▀█████▀ ',
  '██████████████████▄███    ',
  '██████████████████████    ',
  '████████████E████████     ',
  '▀████████████████████     ',
  ' ██████████████████▀      ',
  '  █████████████████       ',
  '   ▀████████████████      ',
  '     ▀▀███████▀           ',
];
// (col, row, glyph) per tick: nothing, then a jet climbing, then the spray falling back.
const SPOUT: [number, number, string][][] = [
  [],
  [],
  [[5, 1, 'V']],
  [[4, 0, 'W'], [5, 1, 'V']],
  [[4, 0, 'W'], [6, 0, 'W'], [5, 1, 'V']],
  [[3, 0, 'W'], [5, 0, 'W'], [4, 1, 'V'], [6, 1, 'V']],
  [[4, 0, 'W'], [6, 0, 'W'], [3, 1, 'V'], [5, 1, 'V']],
  [[5, 0, 'W'], [4, 1, 'V'], [6, 1, 'V']],
  [[5, 1, 'V']],
  [[4, 1, 'V']],
  [],
  [],
];

function deepseekArt(frame: number, resting: boolean): string[] {
  const g = blank(WHALE_W, FIG_ROWS);
  const closed = resting || frame % 26 < 2;
  WHALE_BODY.forEach((src, r) => {
    for (let c = 0; c < src.length && c < WHALE_W; c++) {
      if (src[c] === ' ') continue;
      g[r + 1][c] = src[c] === 'E' ? (closed ? 'C' : 'E') : src[c];  // the body sits one row down
    }
  });
  if (!resting) {
    for (const [c, r, ch] of SPOUT[frame % SPOUT.length]) {
      if (g[r] && g[r][c] === ' ') g[r][c] = ch;  // the spout never overwrites the whale
    }
  }
  return lines(g);
}

// OpenAI, a Dot: a rounded blob close to 1:1, two gentle lobes with a shallow crease, sides
// bulging and narrowing to a blunt point. Two solid lenses nearly touch and share a thin
// bridge, with no eyes behind them. The lenses blink, one winks, and a catch light sweeps
// them. Todd green, one of the named Dot colours.
const DOT_W = 24;
const DOT_BASE = [
  '                        ',
  '    ▄▄▄▄▄▄▄▄ ▄▄▄▄▄▄▄▄   ',
  '  ▄███████████████████▄ ',
  ' ▄█████████████████████▄',
  ' ██████LLLL███LLLL██████',
  ' █████XXXXXXBXXXXXX█████',
  '  ████UXXXXU█UXXXXU████ ',
  '   ███████████████████  ',
  '   ███████████████████  ',
  '    █████████████████   ',
  '     ▀█████████████▀    ',
  '       ▀▀███████▀▀      ',
  '           ▀▀▀          ',
];
const DOT_LENS: [number, number][] = [[6, 11], [13, 18]];  // the lens columns, per lens

function openaiArt(frame: number, resting: boolean): string[] {
  const g = DOT_BASE.map(row => row.split(''));
  const phase = frame % 24;
  const blink = resting || phase < 2;
  const wink = !resting && phase >= 8 && phase < 11;
  // A closed lens is a single slit across the body it had covered.
  const close = ([from, to]: [number, number]) => {
    for (let c = from; c <= to; c++) {
      if (g[4][c] !== ' ') g[4][c] = '█';
      if (g[5][c] !== ' ') g[5][c] = 'C';
      if (g[6][c] !== ' ') g[6][c] = '█';
    }
  };
  if (blink) DOT_LENS.forEach(close);
  else if (wink) close(DOT_LENS[1]);
  else if (phase >= 14 && phase < 17) g[5][DOT_LENS[0][0] + 1] = 'O';  // glint on the left
  else if (phase >= 18 && phase < 21) g[5][DOT_LENS[1][0] + 1] = 'O';  // glint on the right
  return lines(g);
}

// One entry per lab, in the order the column rotates through them.
export const LABS: Lab[] = [
  {
    color: '#4D6BFE',
    ink: 'white',
    words: ['pondering', 'percolating', 'marinating', 'noodling', 'moseying',
            'frolicking', 'simmering', 'drifting', 'bobbing', 'breaching',
            'dawdling', 'meandering', 'vibing'],
    art: deepseekArt,
  },
  {
    color: '#C17A5B',
    ink: 'black',
    words: ['ruminating', 'cogitating', 'tinkering', 'scheming', 'deliberating',
            'mulling', 'weighing', 'brooding', 'pontificating', 'deducing',
            'philosophising', 'contriving', 'pondering'],
    art: anthropicArt,
  },
  {
    color: '#10A37F',
    ink: 'black',
    words: ['noodling', 'tinkering', 'scheming', 'vibing', 'synthesising',
            'orchestrating', 'wrangling', 'iterating', 'spinning', 'weaving',
            'knotting', 'conjuring', 'extrapolating'],
    art: openaiArt,
  },
];

const LAB_PERIOD_MS = 180_000;  // one lab holds the column for minutes, not per frame
const TICKS_PER_WORD = 40;       // the 120ms ticker lands a new word about every 4.8s
const PAUSED_WORDS = ['napping', 'waiting', 'dozing', 'resting', 'idling', 'snoozing', 'loitering'];
const IDLE_WORD = 'dozing';

export function labIndex(now: number): number {
  const bucket = Math.floor(now / LAB_PERIOD_MS) % LABS.length;
  return (bucket + LABS.length) % LABS.length;
}

export type Pose = {lab: Lab; art: string[]; word: string; sleeping: boolean};

// Everything the cartoon shows for a frame: which lab, which pose, which word, and whether
// it is still. The caller supplies the shared ticker's frame, so there is no second timer.
export function mascotState(opts: {
  animating: boolean; paused: boolean; frame: number; now: number;
}): Pose {
  const {animating, paused, frame, now} = opts;
  const lab = LABS[labIndex(now)];
  const resting = paused || !animating;
  const word = paused
    ? PAUSED_WORDS[labIndex(now) % PAUSED_WORDS.length]
    : !animating
      ? IDLE_WORD
      : lab.words[Math.floor(frame / TICKS_PER_WORD) % lab.words.length];
  return {lab, art: lab.art(resting ? 0 : frame, resting), word, sleeping: resting};
}

// A figure line, split into runs that keep the body colour or switch to the ink colour.
function figSpans(line: string): {ch: string; ink: boolean}[] {
  const spans: {ch: string; ink: boolean}[] = [];
  for (const raw of line) {
    const ink = raw in INK_GLYPH;
    const ch = ink ? INK_GLYPH[raw] : raw in GLINT_GLYPH ? GLINT_GLYPH[raw] : raw;
    const last = spans[spans.length - 1];
    if (last && last.ink === ink) last.ch += ch;
    else spans.push({ch, ink});
  }
  return spans;
}

const BUBBLE_INNER = 25;
const TAIL = 3;
const BUBBLE_INDENT = '  ';
const ART_COL = TAIL + 8;                     // art lines up under the pointer arrow
const ART_INDENT = ' '.repeat(ART_COL);
const POINTER = ' '.repeat(TAIL + 3) + '╰─▸';
const MIN_ROWS = 3 + FIG_ROWS;                // bubble plus the figure's own rows

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
  const {lab, art, word, sleeping} = mascotState({
    animating, paused, frame, now: now ?? Date.now(),
  });
  const showActivity = animating && leftRows > MIN_ROWS && Boolean(activity);
  const top = `${BUBBLE_INDENT}╭${'─'.repeat(BUBBLE_INNER)}╮`;
  const mid = `${BUBBLE_INDENT}(${fit(`  ${word}…`, BUBBLE_INNER)})`;
  const bottom = `${BUBBLE_INDENT}╰${'─'.repeat(TAIL)}╮${
    '─'.repeat(BUBBLE_INNER - TAIL - 1)}╯`;
  // The first figure row rides the bubble pointer; the rest indent to the same column.
  const figure = art.map(
    (row, i) => (i === 0 ? `${POINTER}  ${row}` : `${ART_INDENT}${row}`),
  );
  const children: React.ReactNode[] = [
    line(top, {dimColor: sleeping}, 'top'),
    line(mid, {dimColor: sleeping}, 'mid'),
    line(bottom, {dimColor: sleeping}, 'bottom'),
    ...figure.map((text, i) => React.createElement(
      Text,
      {key: `fig${i}`, dimColor: sleeping},
      ...figSpans(text).map((s, j) => React.createElement(
        Text,
        {key: `s${j}`, color: s.ink ? lab.ink : lab.color},
        s.ch,
      )),
    )),
  ];
  if (showActivity) {
    children.push(line(`${ART_INDENT}${fit(String(activity), 46 - ART_COL)}`, {dimColor: true}, 'doing'));
  }
  // The column takes every row the lists leave and centres the figure in them; overflow hidden
  // keeps a short terminal from drawing over the footer.
  return React.createElement(
    Box,
    {
      flexDirection: 'column', flexGrow: 1, flexShrink: 0,
      justifyContent: 'center', overflow: 'hidden',
    },
    ...children,
  );
}
