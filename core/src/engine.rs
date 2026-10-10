use crate::Result;

/// An RGB colour, already resolved against the palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// How a cell takes part in the grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Width {
    /// One column.
    #[default]
    Narrow,
    /// The first of two columns (CJK, emoji). The next cell is a [`Width::Spacer`].
    Wide,
    /// The second column of a wide character, or the filler at the end of a
    /// soft-wrapped line. Not drawn.
    Spacer,
}

/// One cell of the visible screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster shown in the cell; empty for a blank cell.
    pub text: String,
    pub width: Width,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// An owned, engine-independent copy of the visible screen.
///
/// The renderer never sees an engine's own types, so an engine can be swapped
/// without touching the UI.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Option<Cursor>,
    pub default_fg: Color,
    pub default_bg: Color,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<Cell>,
}

impl Screen {
    /// The screen as plain text, one line per row, trailing blanks trimmed.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for row in self.cells.chunks(self.cols.max(1) as usize) {
            let line: String = row
                .iter()
                .filter(|c| c.width != Width::Spacer)
                .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

/// The boundary between holzWave and a terminal emulation library.
///
/// `libghostty-vt` is young and its API moves, so nothing outside the engine
/// implementation may name its types. An engine is **not** required to be
/// `Send`: `libghostty-vt` objects are not, so an engine lives on one thread
/// and is driven over channels.
pub trait TerminalEngine {
    /// Feed bytes that came from the PTY (or any VT source).
    fn feed(&mut self, bytes: &[u8]);

    /// Change the grid size. `cell_px` is the cell size in pixels, used for
    /// image protocols and size reports; pass `(0, 0)` if unknown.
    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<()>;

    /// The current grid size as `(cols, rows)`.
    fn size(&self) -> Result<(u16, u16)>;

    /// Copy the visible screen out of the engine.
    fn screen(&mut self) -> Result<Screen>;
}
