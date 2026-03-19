use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use tm::grid::{CellContent, Grid, GridLine, LineFlags};
use tm::keys;
use tm::screen::Screen;
use tm::simd::SimdScanner;
use tm::state::{Pane, PaneId};
use tm::tty::TtyWriter;
use tm::vt;

// ---------------------------------------------------------------------------
// Grid benchmarks
// ---------------------------------------------------------------------------

fn bench_scroll_up(c: &mut Criterion) {
    let mut grid = Grid::new(80, 24, 10_000);
    c.bench_function("scroll_up 80x24", |b| {
        b.iter(|| grid.scroll_up(0, 23));
    });
}

fn bench_grid_line_new(c: &mut Criterion) {
    c.bench_function("GridLine::new(80)", |b| {
        b.iter(|| black_box(GridLine::new(80)));
    });
}

fn bench_grid_line_new_200(c: &mut Criterion) {
    c.bench_function("GridLine::new(200)", |b| {
        b.iter(|| black_box(GridLine::new(200)));
    });
}

fn bench_set_cell(c: &mut Criterion) {
    let mut line = GridLine::new(80);
    let content = CellContent::from_ascii(b'X');
    let mut col = 0u32;
    c.bench_function("set_cell ASCII", |b| {
        b.iter(|| {
            line.set_cell(col % 80, &content);
            col = col.wrapping_add(1);
        });
    });
}

fn bench_get_cell(c: &mut Criterion) {
    let mut line = GridLine::new(80);
    for col in 0..80 {
        line.set_cell(col, &CellContent::from_ascii(b'A' + (col % 26) as u8));
    }
    let mut col = 0u32;
    c.bench_function("get_cell", |b| {
        b.iter(|| {
            black_box(line.get_cell(col % 80));
            col = col.wrapping_add(1);
        });
    });
}

fn make_filled_grid(sx: u32, sy: u32, history: u32) -> Grid {
    let mut grid = Grid::new(sx, sy, 10_000);
    for row in 0..sy {
        for col in 0..sx {
            let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
            grid.visible_line_mut(row).unwrap().set_cell(col, &content);
        }
        grid.visible_line_mut(row).unwrap().flags = LineFlags(LineFlags::WRAPPED);
    }
    for _ in 0..history {
        grid.scroll_up(0, sy - 1);
    }
    grid
}

fn bench_reflow_shrink(c: &mut Criterion) {
    c.bench_function("reflow 80→40 (1K history)", |b| {
        b.iter_batched(
            || make_filled_grid(80, 24, 1000),
            |mut grid| grid.resize(40, 24),
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_reflow_expand(c: &mut Criterion) {
    c.bench_function("reflow 40→80 (1K history)", |b| {
        b.iter_batched(
            || make_filled_grid(40, 24, 1000),
            |mut grid| grid.resize(80, 24),
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_reflow_large(c: &mut Criterion) {
    c.bench_function("reflow 80→40 (10K history)", |b| {
        b.iter_batched(
            || make_filled_grid(80, 24, 10_000),
            |mut grid| grid.resize(40, 24),
            criterion::BatchSize::LargeInput,
        );
    });
}

// ---------------------------------------------------------------------------
// VT parser benchmarks
// ---------------------------------------------------------------------------

fn make_test_pane(sx: u32, sy: u32) -> Pane {
    Pane::new(PaneId(0), -1, 0, sx, sy)
}

fn bench_vt_ascii(c: &mut Criterion) {
    let data: Vec<u8> = (0..4096).map(|i| b'A' + (i % 26) as u8).collect();
    c.bench_function("vt parse 4KB ASCII", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_ascii_64k(c: &mut Criterion) {
    let data: Vec<u8> = (0..65536).map(|i| b'A' + (i % 26) as u8).collect();
    c.bench_function("vt parse 64KB ASCII", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_sgr_colors(c: &mut Criterion) {
    // Colored output: SGR + text, repeated
    let mut data = Vec::new();
    for i in 0..500 {
        data.extend_from_slice(format!("\x1b[38;5;{}m", i % 256).as_bytes());
        data.extend_from_slice(b"colored text ");
    }
    c.bench_function("vt parse SGR colors (500 seqs)", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_cursor_movement(c: &mut Criterion) {
    // Lots of cursor movement (like a TUI app redrawing)
    let mut data = Vec::new();
    for row in 1..=24 {
        for col in 1..=80 {
            data.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
            data.push(b'X');
        }
    }
    c.bench_function("vt full screen redraw 80x24", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_scroll(c: &mut Criterion) {
    // Simulate `yes` output — newlines that cause scrolling
    let data: Vec<u8> = "y\n".repeat(1000).into_bytes();
    c.bench_function("vt 1000 newlines (scroll)", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

// ---------------------------------------------------------------------------
// Screen benchmarks
// ---------------------------------------------------------------------------

fn bench_screen_put_char(c: &mut Criterion) {
    let mut screen = Screen::new(80, 24);
    c.bench_function("screen put_char ASCII", |b| {
        b.iter(|| {
            screen.put_char(b"X\0\0\0\0\0\0\0", 1, 1);
        });
    });
}

fn bench_screen_linefeed(c: &mut Criterion) {
    let mut screen = Screen::new(80, 24);
    screen.cy = 23; // bottom of screen
    c.bench_function("screen linefeed (scroll)", |b| {
        b.iter(|| {
            screen.linefeed();
        });
    });
}

fn bench_screen_erase_display(c: &mut Criterion) {
    let mut screen = Screen::new(80, 24);
    c.bench_function("screen erase_display(2)", |b| {
        b.iter(|| {
            screen.erase_display(2);
        });
    });
}

// ---------------------------------------------------------------------------
// Key parsing benchmarks
// ---------------------------------------------------------------------------

fn bench_parse_input_ascii(c: &mut Criterion) {
    let data: Vec<u8> = (0..1000).map(|i| b'a' + (i % 26) as u8).collect();
    c.bench_function("parse_input 1000 ASCII chars", |b| {
        b.iter(|| {
            keys::parse_input(black_box(&data));
        });
    });
}

fn bench_parse_input_escape_seqs(c: &mut Criterion) {
    let mut data = Vec::new();
    for _ in 0..100 {
        data.extend_from_slice(b"\x1b[A\x1b[B\x1b[C\x1b[D"); // arrow keys
        data.extend_from_slice(b"\x1b[<0;10;20M"); // mouse click
    }
    c.bench_function("parse_input 500 escape seqs", |b| {
        b.iter(|| {
            keys::parse_input(black_box(&data));
        });
    });
}

// ---------------------------------------------------------------------------
// TTY writer benchmarks
// ---------------------------------------------------------------------------

fn bench_tty_cursor_goto(c: &mut Criterion) {
    let mut tty = TtyWriter::new();
    let mut row = 0u32;
    c.bench_function("tty cursor_goto", |b| {
        b.iter(|| {
            tty.cursor_goto(row % 24, row % 80);
            row = row.wrapping_add(1);
        });
    });
}

fn bench_tty_set_cell_attrs(c: &mut Criterion) {
    use tm::grid::Color;
    let mut tty = TtyWriter::new();
    let cells = [
        CellContent {
            fg: Color::Palette(1),
            ..CellContent::default()
        },
        CellContent {
            fg: Color::Palette(2),
            bg: Color::Palette(4),
            ..CellContent::default()
        },
        CellContent {
            fg: Color::Rgb(100, 200, 50),
            ..CellContent::default()
        },
        CellContent::default(),
    ];
    let mut i = 0usize;
    c.bench_function("tty set_cell_attrs (cycling)", |b| {
        b.iter(|| {
            tty.set_cell_attrs(&cells[i % cells.len()]);
            i += 1;
        });
    });
}

// ---------------------------------------------------------------------------
// Slow path benchmarks
// ---------------------------------------------------------------------------

fn bench_vt_utf8_cjk(c: &mut Criterion) {
    // CJK characters (3-byte UTF-8, width 2) — extended cell path
    let mut data = Vec::new();
    for _ in 0..500 {
        data.extend_from_slice("中文".as_bytes()); // 3 bytes × 2 chars
    }
    c.bench_function("vt parse 500 CJK chars", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_mixed_ascii_utf8(c: &mut Criterion) {
    // Realistic mixed content: ASCII with occasional UTF-8
    let mut data = Vec::new();
    for i in 0..200 {
        data.extend_from_slice(b"filename_");
        if i % 5 == 0 {
            data.extend_from_slice("→".as_bytes());
        }
        data.extend_from_slice(b".txt ");
    }
    c.bench_function("vt mixed ASCII+UTF8 (200 entries)", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_scroll_region(c: &mut Criterion) {
    // Partial scroll — used by vim, less, htop for scrolling content areas
    let mut grid = Grid::new(80, 24, 10_000);
    c.bench_function("scroll_up region (rows 1-22)", |b| {
        b.iter(|| grid.scroll_up(1, 22));
    });
}

fn bench_scroll_down(c: &mut Criterion) {
    let mut grid = Grid::new(80, 24, 10_000);
    c.bench_function("scroll_down 80x24", |b| {
        b.iter(|| grid.scroll_down(0, 23));
    });
}

fn bench_vt_htop_frame(c: &mut Criterion) {
    // Simulate htop: cursor position + colored text for each cell
    let mut data = Vec::new();
    for row in 1..=24 {
        data.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        for col in 0..80 {
            let color = 31 + (col % 7);
            data.extend_from_slice(format!("\x1b[{color}m").as_bytes());
            data.push(b'0' + (col % 10) as u8);
        }
    }
    data.extend_from_slice(b"\x1b[0m");
    c.bench_function("vt htop-like frame (24 rows)", |b| {
        let mut pane = make_test_pane(80, 24);
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_vt_htop_unchanged(c: &mut Criterion) {
    // Second htop frame — same content, tests skip-if-unchanged
    let mut data = Vec::new();
    for row in 1..=24 {
        data.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        for col in 0..80 {
            let color = 31 + (col % 7);
            data.extend_from_slice(format!("\x1b[{color}m").as_bytes());
            data.push(b'0' + (col % 10) as u8);
        }
    }
    data.extend_from_slice(b"\x1b[0m");
    let mut pane = make_test_pane(80, 24);
    // Prime with first frame
    vt::process_pane_output(&mut pane, &data);
    // Clear dirty so we can measure
    for row in 0..24 {
        if let Some(line) = pane.screen.grid.visible_line_mut(row) {
            for c in &mut line.compact {
                c.clear_dirty();
            }
        }
    }
    c.bench_function("vt htop unchanged (skip-if-same)", |b| {
        b.iter(|| {
            vt::process_pane_output(&mut pane, black_box(&data));
        });
    });
}

fn bench_erase_large(c: &mut Criterion) {
    let mut screen = Screen::new(200, 50);
    c.bench_function("erase_display(2) 200x50", |b| {
        b.iter(|| screen.erase_display(2));
    });
}

fn bench_delete_insert_lines(c: &mut Criterion) {
    let mut screen = Screen::new(80, 24);
    screen.set_scroll_region(2, 20);
    screen.cy = 5;
    c.bench_function("insert+delete 10 lines", |b| {
        b.iter(|| {
            screen.insert_lines(10);
            screen.delete_lines(10);
        });
    });
}

fn bench_selection_extract(c: &mut Criterion) {
    use tm::state::{Pane, Selection, State};
    // Fill a pane with lots of text, then extract a large selection
    let mut state = State::new();
    let pid = state.alloc_pane_id();
    let mut pane = Pane::new(pid, -1, 0, 80, 24);
    // Fill 1000 history lines
    for _ in 0..1000 {
        for col in 0..80 {
            let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
            pane.screen
                .grid
                .visible_line_mut(0)
                .unwrap()
                .set_cell(col, &content);
        }
        pane.screen.grid.scroll_up(0, 23);
    }
    state.panes.insert(pid, pane);
    let hsize = state.panes[&pid].screen.grid.hsize();

    let sel = Selection {
        pane: pid,
        start_col: 0,
        start_row: hsize.saturating_sub(500),
        end_col: 79,
        end_row: hsize.saturating_sub(1),
    };
    c.bench_function("extract_selection 500 lines", |b| {
        b.iter(|| {
            black_box(tm::key_bind::extract_selection(&state, pid, &sel));
        });
    });
}

fn bench_render_full_screen(c: &mut Criterion) {
    use tm::config::Config;
    use tm::state::{Client, Pane, State};
    // Build a state with a filled pane and render it
    let mut state = State::new();
    let config = Config::default_config();
    let pid = state.alloc_pane_id();
    let mut pane = Pane::new(pid, -1, 0, 80, 24);
    // Fill visible lines
    for row in 0..24 {
        for col in 0..80 {
            let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
            pane.screen
                .grid
                .visible_line_mut(row)
                .unwrap()
                .set_cell(col, &content);
        }
    }
    state.panes.insert(pid, pane);
    let sid = state.create_session("bench", pid, 80, 25);
    let cid = state.alloc_client_id();
    state
        .clients
        .insert(cid, Client::new(cid, -1, -1, 80, 25, sid));
    // Mark all dirty
    for row in 0..24u32 {
        if let Some(pane) = state.panes.get_mut(&pid)
            && let Some(line) = pane.screen.grid.visible_line_mut(row)
        {
            line.mark_dirty();
        }
    }
    c.bench_function("render full 80x24 screen", |b| {
        let mut tty = TtyWriter::new();
        b.iter(|| {
            tty.buf.clear();
            tty.reset_state();
            tm::render::render_client(&state, &config, cid, &mut tty);
            black_box(tty.buf.len());
        });
    });
}

// ---------------------------------------------------------------------------
// Render-after-scroll benchmarks — measure scroll optimization
// ---------------------------------------------------------------------------

/// Helper: set up a state with a filled pane, do an initial render + clear_dirty.
fn make_scroll_bench_state(
    sx: u32,
    sy: u32,
) -> (
    tm::state::State,
    tm::config::Config,
    tm::state::ClientId,
    tm::state::PaneId,
) {
    use tm::config::Config;
    use tm::state::{Client, Pane, State};

    let mut state = State::new();
    let config = Config::default_config();
    let pid = state.alloc_pane_id();
    let mut pane = Pane::new(pid, -1, 0, sx, sy.saturating_sub(1));
    // Fill visible lines with content
    let pane_sy = sy.saturating_sub(1);
    for row in 0..pane_sy {
        for col in 0..sx {
            let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
            pane.screen
                .grid
                .visible_line_mut(row)
                .unwrap()
                .set_cell(col, &content);
        }
    }
    state.panes.insert(pid, pane);
    let sid = state.create_session("bench", pid, sx, sy);
    let cid = state.alloc_client_id();
    state
        .clients
        .insert(cid, Client::new(cid, -1, -1, sx, sy, sid));

    // Initial render + clear to establish baseline terminal state
    let mut tty = TtyWriter::new();
    tm::render::render_client(&state, &config, cid, &mut tty);
    tm::render::clear_dirty(&mut state, cid);

    (state, config, cid, pid)
}

fn bench_render_after_scroll_1(c: &mut Criterion) {
    let (mut state, config, cid, pid) = make_scroll_bench_state(80, 25);
    c.bench_function("render after 1 scroll 80x24", |b| {
        let mut tty = TtyWriter::new();
        b.iter(|| {
            // Simulate one line of output: scroll + write new bottom line
            let pane = state.panes.get_mut(&pid).unwrap();
            let sy = pane.screen.grid.sy;
            pane.screen.grid.scroll_up(0, sy - 1);
            for col in 0..80u32 {
                let content = CellContent::from_ascii(b'a' + (col % 26) as u8);
                pane.screen
                    .grid
                    .visible_line_mut(sy - 1)
                    .unwrap()
                    .set_cell(col, &content);
            }

            tty.buf.clear();
            tty.reset_state();
            tm::render::render_client(&state, &config, cid, &mut tty);
            black_box(tty.buf.len());
            tm::render::clear_dirty(&mut state, cid);
        });
    });
}

fn bench_render_after_scroll_5(c: &mut Criterion) {
    let (mut state, config, cid, pid) = make_scroll_bench_state(80, 25);
    c.bench_function("render after 5 scrolls 80x24", |b| {
        let mut tty = TtyWriter::new();
        b.iter(|| {
            let pane = state.panes.get_mut(&pid).unwrap();
            let sy = pane.screen.grid.sy;
            for i in 0..5u32 {
                pane.screen.grid.scroll_up(0, sy - 1);
                for col in 0..80u32 {
                    let content = CellContent::from_ascii(b'a' + ((col + i) % 26) as u8);
                    pane.screen
                        .grid
                        .visible_line_mut(sy - 1)
                        .unwrap()
                        .set_cell(col, &content);
                }
            }

            tty.buf.clear();
            tty.reset_state();
            tm::render::render_client(&state, &config, cid, &mut tty);
            black_box(tty.buf.len());
            tm::render::clear_dirty(&mut state, cid);
        });
    });
}

fn bench_render_after_scroll_200x50(c: &mut Criterion) {
    let (mut state, config, cid, pid) = make_scroll_bench_state(200, 51);
    c.bench_function("render after 1 scroll 200x50", |b| {
        let mut tty = TtyWriter::new();
        b.iter(|| {
            let pane = state.panes.get_mut(&pid).unwrap();
            let sy = pane.screen.grid.sy;
            pane.screen.grid.scroll_up(0, sy - 1);
            for col in 0..200u32 {
                let content = CellContent::from_ascii(b'a' + (col % 26) as u8);
                pane.screen
                    .grid
                    .visible_line_mut(sy - 1)
                    .unwrap()
                    .set_cell(col, &content);
            }

            tty.buf.clear();
            tty.reset_state();
            tm::render::render_client(&state, &config, cid, &mut tty);
            black_box(tty.buf.len());
            tm::render::clear_dirty(&mut state, cid);
        });
    });
}

fn bench_layout_calculate(c: &mut Criterion) {
    use tm::layout::{LayoutNode, SplitDir};
    use tm::state::PaneId;
    // Build a complex layout: 4 panes in a nested split
    let layout = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        children: vec![
            LayoutNode::Split {
                dir: SplitDir::Vertical,
                children: vec![
                    LayoutNode::Pane(PaneId(0)),
                    LayoutNode::Pane(PaneId(1)),
                    LayoutNode::Pane(PaneId(2)),
                ],
            },
            LayoutNode::Split {
                dir: SplitDir::Vertical,
                children: vec![LayoutNode::Pane(PaneId(3)), LayoutNode::Pane(PaneId(4))],
            },
        ],
    };
    c.bench_function("layout calculate 5 panes", |b| {
        b.iter(|| black_box(layout.calculate(0, 0, 200, 50)));
    });
}

fn bench_protocol_encode_decode(c: &mut Criterion) {
    use tm::protocol::{MSG_INPUT, Message};
    let payload = vec![0x41u8; 256];
    let msg = Message::new(MSG_INPUT, payload);
    let _encoded = msg.encode();
    c.bench_function("protocol encode+decode 256B", |b| {
        b.iter(|| {
            let enc = msg.encode();
            let (dec, _) = Message::decode(black_box(&enc)).unwrap();
            black_box(dec.payload.len());
        });
    });
}

fn bench_mouse_drag_input(c: &mut Criterion) {
    // 200 mouse drag events — realistic for click-drag selection
    let mut data = Vec::new();
    for i in 0..200u32 {
        let x = 10 + (i % 60);
        let y = 5 + (i / 60);
        data.extend_from_slice(format!("\x1b[<32;{x};{y}M").as_bytes());
    }
    c.bench_function("parse_input 200 mouse drags", |b| {
        b.iter(|| {
            keys::parse_input(black_box(&data));
        });
    });
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(50)
}

criterion_group!(
    name = grid_benches;
    config = fast();
    targets =
    bench_scroll_up,
    bench_grid_line_new,
    bench_grid_line_new_200,
    bench_set_cell,
    bench_get_cell,
    bench_reflow_shrink,
    bench_reflow_expand,
    bench_reflow_large,
);

criterion_group!(
    name = vt_benches;
    config = fast();
    targets =
    bench_vt_ascii,
    bench_vt_ascii_64k,
    bench_vt_sgr_colors,
    bench_vt_cursor_movement,
    bench_vt_scroll,
);

criterion_group!(
    name = screen_benches;
    config = fast();
    targets =
    bench_screen_put_char,
    bench_screen_linefeed,
    bench_screen_erase_display,
);

criterion_group!(
    name = input_benches;
    config = fast();
    targets =
    bench_parse_input_ascii,
    bench_parse_input_escape_seqs,
);

criterion_group!(
    name = tty_benches;
    config = fast();
    targets =
    bench_tty_cursor_goto,
    bench_tty_set_cell_attrs,
);

fn bench_simd_scan_4k(c: &mut Criterion) {
    let data = vec![b'A'; 4096];
    c.bench_function("SIMD scan 4KB ASCII", |b| {
        b.iter(|| black_box(SimdScanner::scan(black_box(&data))));
    });
}

fn bench_simd_scan_64k(c: &mut Criterion) {
    let data = vec![b'A'; 65536];
    c.bench_function("SIMD scan 64KB ASCII", |b| {
        b.iter(|| black_box(SimdScanner::scan(black_box(&data))));
    });
}

criterion_group!(
    name = simd_benches;
    config = fast();
    targets =
    bench_simd_scan_4k,
    bench_simd_scan_64k,
);

criterion_group!(
    name = slow_path_benches;
    config = fast();
    targets =
    bench_vt_utf8_cjk,
    bench_vt_mixed_ascii_utf8,
    bench_scroll_region,
    bench_scroll_down,
    bench_vt_htop_frame,
    bench_vt_htop_unchanged,
    bench_erase_large,
    bench_delete_insert_lines,
    bench_selection_extract,
    bench_render_full_screen,
    bench_layout_calculate,
    bench_protocol_encode_decode,
    bench_mouse_drag_input,
);

criterion_group!(
    name = scroll_render_benches;
    config = fast();
    targets =
    bench_render_after_scroll_1,
    bench_render_after_scroll_5,
    bench_render_after_scroll_200x50,
);

criterion_main!(
    grid_benches,
    vt_benches,
    screen_benches,
    input_benches,
    tty_benches,
    simd_benches,
    slow_path_benches,
    scroll_render_benches
);
