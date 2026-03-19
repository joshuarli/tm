use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use tm::grid::{CellContent, Grid, GridLine, LineFlags};
use tm::keys;
use tm::screen::Screen;
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
    // Simulate a program printing lots of ASCII text
    let data: Vec<u8> = (0..4096).map(|i| b'A' + (i % 26) as u8).collect();
    c.bench_function("vt parse 4KB ASCII", |b| {
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
        CellContent { fg: Color::Palette(1), ..CellContent::default() },
        CellContent { fg: Color::Palette(2), bg: Color::Palette(4), ..CellContent::default() },
        CellContent { fg: Color::Rgb(100, 200, 50), ..CellContent::default() },
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

criterion_main!(grid_benches, vt_benches, screen_benches, input_benches, tty_benches);
