//! The left column's idle cartoon: one lab mascot that comes alive while a scan runs, over a
//! fixed-size speech bubble. A direct port of the Ink dashboard's mascot, so the two draw the
//! same figures from the same frame numbers.
//!
//! Every figure draws exactly [`FIG_ROWS`] lines from a single deterministic frame number, so a
//! given frame always renders the same picture and the column never jumps between labs.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The shared figure height; every lab fills it, so nothing reflows.
pub const FIG_ROWS: usize = 13;

/// Marker-to-glyph table. Markers keep the art readable while letting the eyes and shades take
/// the ink colour and the body take the lab colour. `O` is a body-coloured catch light.
fn ink_glyph(marker: char) -> Option<char> {
    Some(match marker {
        'X' => '█',
        'U' => '▀',
        'L' => '▄', // the Dot's solid lenses
        'E' => '●', // the whale's eye
        'G' => '▶',
        'H' => '◀', // the creature's inward-pointing eyes
        'C' => '▬', // a closed eye or a squinting lens
        'B' => '─', // the bright bridge between the Dot's lenses
        'V' => '·',
        'W' => '°', // the whale's spout
        _ => return None,
    })
}

/// A marker resolved to the glyph it draws and whether it takes the ink colour.
fn resolve(marker: char) -> (char, bool) {
    if let Some(glyph) = ink_glyph(marker) {
        (glyph, true)
    } else if marker == 'O' {
        ('●', false)
    } else {
        (marker, false)
    }
}

/// A figure line with its markers resolved to the glyphs the dashboard draws, for the headless
/// review path. The colour split is deliberately dropped: this is the shape, not the palette.
pub fn resolve_line(line: &str) -> String {
    line.chars().map(|marker| resolve(marker).0).collect()
}

fn blank(width: usize, height: usize) -> Vec<Vec<char>> {
    vec![vec![' '; width]; height]
}

fn lines(grid: &[Vec<char>]) -> Vec<String> {
    grid.iter()
        .map(|row| row.iter().collect::<String>().trim_end().to_string())
        .collect()
}

fn paint(grid: &mut [Vec<char>], rows: (usize, usize), cols: (usize, usize), ch: char) {
    for r in rows.0..=rows.1 {
        let Some(row) = grid.get_mut(r) else { continue };
        for c in cols.0..=cols.1 {
            if c < row.len() {
                row[c] = ch;
            }
        }
    }
}

// Anthropic, the Claude creature: a blocky, symmetric pixel animal. One rectangular body with no
// neck, arm tabs low on both sides, two short legs with a gap, and eyes drawn as filled triangles
// pointing inward at each other. It blinks, walks its legs and lifts its arms on independent
// periods.
const ANTHROPIC_W: usize = 16;

fn anthropic_art(frame: u64, resting: bool) -> Vec<String> {
    let mut g = blank(ANTHROPIC_W, FIG_ROWS);
    let eyes_closed = resting || frame % 20 < 2;
    let step = if resting { 0 } else { (frame / 3) % 3 }; // 0 stand, 1 left up, 2 right up
    let lifted = !resting && (frame / 8) % 2 == 1; // arms ride a row higher
    let arm_rows = if lifted { (2, 4) } else { (3, 5) };

    paint(&mut g, (0, 8), (3, 12), '█'); // the body, taller than it is wide
    paint(&mut g, arm_rows, (0, 15), '█'); // the arm band, the widest row, sticking out both sides
    g[3][4] = if eyes_closed { 'C' } else { 'G' }; // left eye, a triangle pointing right
    g[3][11] = if eyes_closed { 'C' } else { 'H' }; // right eye, a triangle pointing left
    paint(&mut g, (9, 12), (5, 6), '█'); // left leg
    paint(&mut g, (9, 12), (9, 10), '█'); // right leg, a two-column gap between them
    if !resting && step == 1 {
        g[12][5] = ' '; // the left foot lifts
    }
    if !resting && step == 2 {
        g[12][10] = ' '; // the right foot lifts
    }
    lines(&g)
}

// DeepSeek, the blue whale: the mark is a whale in side profile, drawn as a sweeping arc. Thin at
// the rostrum low on the left, thickest through the middle, back convex and belly concave, tail
// forking into two flukes high on the right. The eye only appears at this size, and the spout
// rises from the blowhole.
const WHALE_W: usize = 26;
const WHALE_BODY: [&str; 12] = [
    "       ▄▄ ▄▄▄▄   ▄█       ",
    "   ▄█████████    ███▄ ▄▄▄█",
    " ▄████████████▄  ▀████████",
    "▄███████████████▄ ▀█████▀ ",
    "██████████████████▄███    ",
    "██████████████████████    ",
    "████████████E████████     ",
    "▀████████████████████     ",
    " ██████████████████▀      ",
    "  █████████████████       ",
    "   ▀████████████████      ",
    "     ▀▀███████▀           ",
];
// (col, row, glyph) per tick: nothing, then a jet climbing, then the spray falling back.
type SpoutTick = &'static [(usize, usize, char)];
const SPOUT: [SpoutTick; 12] = [
    &[],
    &[],
    &[(5, 1, 'V')],
    &[(4, 0, 'W'), (5, 1, 'V')],
    &[(4, 0, 'W'), (6, 0, 'W'), (5, 1, 'V')],
    &[(3, 0, 'W'), (5, 0, 'W'), (4, 1, 'V'), (6, 1, 'V')],
    &[(4, 0, 'W'), (6, 0, 'W'), (3, 1, 'V'), (5, 1, 'V')],
    &[(5, 0, 'W'), (4, 1, 'V'), (6, 1, 'V')],
    &[(5, 1, 'V')],
    &[(4, 1, 'V')],
    &[],
    &[],
];

fn deepseek_art(frame: u64, resting: bool) -> Vec<String> {
    let mut g = blank(WHALE_W, FIG_ROWS);
    let closed = resting || frame % 26 < 2;
    for (r, src) in WHALE_BODY.iter().enumerate() {
        for (c, ch) in src.chars().enumerate() {
            if c >= WHALE_W {
                break;
            }
            if ch == ' ' {
                continue;
            }
            // The body sits one row down.
            g[r + 1][c] = if ch == 'E' {
                if closed {
                    'C'
                } else {
                    'E'
                }
            } else {
                ch
            };
        }
    }
    if !resting {
        for &(c, r, ch) in SPOUT[(frame % SPOUT.len() as u64) as usize] {
            // The spout never overwrites the whale.
            if g.get(r).is_some_and(|row| row.get(c) == Some(&' ')) {
                g[r][c] = ch;
            }
        }
    }
    lines(&g)
}

// OpenAI, a Dot: a rounded blob close to 1:1, two gentle lobes with a shallow crease, sides
// bulging and narrowing to a blunt point. Two solid lenses nearly touch and share a thin bridge,
// with no eyes behind them. The lenses blink, one winks, and a catch light sweeps them.
const DOT_BASE: [&str; 13] = [
    "                        ",
    "    ▄▄▄▄▄▄▄▄ ▄▄▄▄▄▄▄▄   ",
    "  ▄███████████████████▄ ",
    " ▄█████████████████████▄",
    " ██████LLLL███LLLL██████",
    " █████XXXXXXBXXXXXX█████",
    "  ████UXXXXU█UXXXXU████ ",
    "   ███████████████████  ",
    "   ███████████████████  ",
    "    █████████████████   ",
    "     ▀█████████████▀    ",
    "       ▀▀███████▀▀      ",
    "           ▀▀▀          ",
];
const DOT_LENS: [(usize, usize); 2] = [(6, 11), (13, 18)]; // the lens columns, per lens

fn openai_art(frame: u64, resting: bool) -> Vec<String> {
    let mut g: Vec<Vec<char>> = DOT_BASE.iter().map(|row| row.chars().collect()).collect();
    let phase = frame % 24;
    let blink = resting || phase < 2;
    let wink = !resting && (8..11).contains(&phase);
    // A closed lens is a single slit across the body it had covered.
    fn close(g: &mut [Vec<char>], (from, to): (usize, usize)) {
        for (row, glyph) in [(4usize, '█'), (5, 'C'), (6, '█')] {
            for cell in g[row][from..=to].iter_mut() {
                if *cell != ' ' {
                    *cell = glyph;
                }
            }
        }
    }
    if blink {
        for lens in DOT_LENS {
            close(&mut g, lens);
        }
    } else if wink {
        close(&mut g, DOT_LENS[1]);
    } else if (14..17).contains(&phase) {
        g[5][DOT_LENS[0].0 + 1] = 'O'; // glint on the left
    } else if (18..21).contains(&phase) {
        g[5][DOT_LENS[1].0 + 1] = 'O'; // glint on the right
    }
    lines(&g)
}

/// One entry per lab, in the order the column rotates through them.
pub struct Lab {
    pub color: Color,
    pub ink: Color,
    pub words: &'static [&'static str],
    art: fn(u64, bool) -> Vec<String>,
}

pub const LABS: [Lab; 3] = [
    Lab {
        color: Color::Rgb(0x4D, 0x6B, 0xFE),
        ink: Color::White,
        words: &[
            "pondering",
            "percolating",
            "marinating",
            "noodling",
            "moseying",
            "frolicking",
            "simmering",
            "drifting",
            "bobbing",
            "breaching",
            "dawdling",
            "meandering",
            "vibing",
        ],
        art: deepseek_art,
    },
    Lab {
        color: Color::Rgb(0xC1, 0x7A, 0x5B),
        ink: Color::Black,
        words: &[
            "ruminating",
            "cogitating",
            "tinkering",
            "scheming",
            "deliberating",
            "mulling",
            "weighing",
            "brooding",
            "pontificating",
            "deducing",
            "philosophising",
            "contriving",
            "pondering",
        ],
        art: anthropic_art,
    },
    Lab {
        color: Color::Rgb(0x10, 0xA3, 0x7F),
        ink: Color::Black,
        words: &[
            "noodling",
            "tinkering",
            "scheming",
            "vibing",
            "synthesising",
            "orchestrating",
            "wrangling",
            "iterating",
            "spinning",
            "weaving",
            "knotting",
            "conjuring",
            "extrapolating",
        ],
        art: openai_art,
    },
];

const LAB_PERIOD_MS: i64 = 180_000; // one lab holds the column for minutes, not per frame
const TICKS_PER_WORD: u64 = 40; // a new word about every four seconds at the 100ms ticker
const PAUSED_WORDS: [&str; 7] = [
    "napping",
    "waiting",
    "dozing",
    "resting",
    "idling",
    "snoozing",
    "loitering",
];
const IDLE_WORD: &str = "dozing";

/// Which lab owns the column at a wall-clock instant, on the 180-second rotation.
pub fn lab_index(now_ms: i64) -> usize {
    now_ms
        .div_euclid(LAB_PERIOD_MS)
        .rem_euclid(LABS.len() as i64) as usize
}

/// The wall clock in milliseconds since the epoch, the `now` the rotation reads. A
/// `TRIAD_MASCOT_NOW_MS` override pins the clock for the headless per-lab render check; the
/// dashboard never sets it.
pub fn now_millis() -> i64 {
    if let Ok(value) = std::env::var("TRIAD_MASCOT_NOW_MS") {
        if let Ok(ms) = value.parse::<i64>() {
            return ms;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

/// Everything the cartoon shows for a frame: which lab, which pose, which word, and whether it
/// is still. The caller supplies the shared ticker's frame, so there is no second timer.
pub struct Pose {
    pub lab: usize,
    pub art: Vec<String>,
    pub word: String,
    pub sleeping: bool,
}

pub fn mascot_state(animating: bool, paused: bool, frame: u64, now_ms: i64) -> Pose {
    let index = lab_index(now_ms);
    let lab = &LABS[index];
    let resting = paused || !animating;
    let word = if paused {
        PAUSED_WORDS[index % PAUSED_WORDS.len()].to_string()
    } else if !animating {
        IDLE_WORD.to_string()
    } else {
        let step = (frame / TICKS_PER_WORD) as usize;
        lab.words[step % lab.words.len()].to_string()
    };
    Pose {
        lab: index,
        art: (lab.art)(if resting { 0 } else { frame }, resting),
        word,
        sleeping: resting,
    }
}

const BUBBLE_INNER: usize = 25;
const TAIL: usize = 3;
const ART_COL: usize = TAIL + 8; // art lines up under the pointer arrow
const MIN_ROWS: usize = 3 + FIG_ROWS; // bubble plus the figure's own rows

/// The mascot block, split into runs that keep the body colour or switch to the ink colour.
fn art_lines(lab: &Lab, art: &[String], sleeping: bool) -> Vec<Line<'static>> {
    let pointer = format!("{}╰─▸", " ".repeat(TAIL + 3));
    let indent = " ".repeat(ART_COL);
    art.iter()
        .enumerate()
        .map(|(i, row)| {
            let text = if i == 0 {
                format!("{pointer}  {row}")
            } else {
                format!("{indent}{row}")
            };
            let mut spans: Vec<Span<'static>> = Vec::new();
            let chars: Vec<char> = text.chars().collect();
            let mut at = 0;
            while at < chars.len() {
                let (_, ink) = resolve(chars[at]);
                let mut run = String::new();
                while at < chars.len() && resolve(chars[at]).1 == ink {
                    run.push(resolve(chars[at]).0);
                    at += 1;
                }
                let mut style = Style::new().fg(if ink { lab.ink } else { lab.color });
                if sleeping {
                    style = style.add_modifier(Modifier::DIM);
                }
                spans.push(Span::styled(run, style));
            }
            Line::from(spans)
        })
        .collect()
}

/// The whole mascot column, centred in `left_rows` leftover rows, or nothing when there is not
/// room for the full figure. The caller owns the area, so an empty vec means "draw nothing".
pub fn render(
    left_rows: usize,
    animating: bool,
    paused: bool,
    frame: u64,
    now_ms: i64,
) -> Vec<Line<'static>> {
    if left_rows < MIN_ROWS {
        return Vec::new();
    }
    let pose = mascot_state(animating, paused, frame, now_ms);
    let lab = &LABS[pose.lab];
    let sleeping = pose.sleeping;

    let bubble = if sleeping {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    let top = format!("  ╭{}╮", "─".repeat(BUBBLE_INNER));
    let mid = format!(
        "  ({})",
        crate::ui::fit(&format!("  {}…", pose.word), BUBBLE_INNER)
    );
    let bottom = format!(
        "  ╰{}╮{}╯",
        "─".repeat(TAIL),
        "─".repeat(BUBBLE_INNER - TAIL - 1)
    );

    let mut out: Vec<Line<'static>> = Vec::new();
    // The column takes every row the lists leave and centres the figure in them.
    for _ in 0..(left_rows - MIN_ROWS) / 2 {
        out.push(Line::default());
    }
    out.push(Line::styled(top, bubble));
    out.push(Line::styled(mid, bubble));
    out.push(Line::styled(bottom, bubble));
    out.extend(art_lines(lab, &pose.art, sleeping));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_rotates_every_180_seconds() {
        assert_eq!(lab_index(0), 0);
        assert_eq!(lab_index(LAB_PERIOD_MS - 1), 0);
        assert_eq!(lab_index(LAB_PERIOD_MS), 1);
        assert_eq!(lab_index(2 * LAB_PERIOD_MS), 2);
        assert_eq!(lab_index(3 * LAB_PERIOD_MS), 0);
        // A clock before the epoch still yields a valid lab.
        assert_eq!(lab_index(-1), 2);
    }

    #[test]
    fn word_cycles_on_the_tick_and_rests_when_paused_or_idle() {
        let first = mascot_state(true, false, 0, 0);
        assert_eq!(first.word, LABS[0].words[0]);
        assert_eq!(
            mascot_state(true, false, TICKS_PER_WORD, 0).word,
            LABS[0].words[1]
        );
        // Wraps back to the first word after a full pass.
        let pass = TICKS_PER_WORD * LABS[0].words.len() as u64;
        assert_eq!(mascot_state(true, false, pass, 0).word, LABS[0].words[0]);
        // Nothing live: the idle word. A paused run: the paused list, keyed by the lab.
        assert_eq!(mascot_state(false, false, 99, 0).word, IDLE_WORD);
        assert_eq!(
            mascot_state(false, true, 99, 0).word,
            PAUSED_WORDS[lab_index(0) % PAUSED_WORDS.len()]
        );
    }

    #[test]
    fn resting_pose_is_still_and_eyes_closed() {
        let rest = mascot_state(false, false, 0, 0);
        assert!(rest.sleeping);
        // Any frame renders the same closed pose, so nothing moves without a live run.
        for frame in [3u64, 8, 21, 99] {
            let later = mascot_state(false, false, frame, 0);
            assert_eq!(later.art, rest.art);
        }
        let paused = mascot_state(false, true, 7, 0);
        assert!(paused.sleeping);
        assert_eq!(paused.art, rest.art);
        // The resting creature shows closed eyes: no open G/H markers survive the glyph table.
        let creature = &rest.art;
        assert!(creature.iter().all(|line| !line.contains(['G', 'H'])));
    }

    #[test]
    fn hides_below_min_rows_and_draws_the_full_block_above() {
        assert!(render(MIN_ROWS - 1, true, false, 0, 0).is_empty());
        let drawn = render(MIN_ROWS, true, false, 0, 0);
        assert_eq!(drawn.len(), MIN_ROWS);
        // Extra rows are padding above, so the block stays centred.
        let roomy = render(MIN_ROWS + 4, true, false, 0, 0);
        assert_eq!(roomy.len(), MIN_ROWS + 2);
        assert!(roomy[0].spans.is_empty());
    }
}
