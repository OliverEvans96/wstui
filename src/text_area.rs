use tracing::{info, instrument};
use tui::{
    layout::Rect,
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Default)]
pub struct CursorPosition {
    pub col: usize,
    pub row: usize,
}

#[derive(Debug)]
pub enum Action {
    NewChar(char),
    Backspace,
    NewLine,
    Clear,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveHome,
    MoveEnd,
}

/// Multi-line text input
#[derive(Debug)]
pub struct TextArea {
    pub cursor: CursorPosition,
    lines: Vec<String>,
    /// Current position of the cursor in the current string (character index)
    /// (may differ from cursor.pos due to differences in unicode_width)
    line_pos: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            cursor: Default::default(),
            // Start with one blank line
            lines: vec![String::new()],
            line_pos: 0,
        }
    }
}

// Accessors
impl TextArea {
    /// Get a reference to the current line
    fn current_line(&self) -> &String {
        &self.lines[self.cursor.row]
    }

    /// Get a mutable reference to the current line
    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor.row]
    }

    /// Get the current number of lines.
    pub fn nlines(&self) -> usize {
        self.lines.len()
    }

    /// Get the content of the text area as a single multi-line string.
    pub fn content(&self) -> String {
        self.lines.join("\n")
    }
}

// Mutators
impl TextArea {
    pub fn new() -> Self {
        Default::default()
    }

    fn insert_char(&mut self, c: char) {
        let line_pos = self.line_pos;
        let line = self.current_line_mut();
        // Calculate the byte position from character index
        let byte_pos = get_byte_index_from_char_index(line, line_pos);
        line.insert(byte_pos, c);
        self.move_right();
    }

    fn insert_string(&mut self, s: String) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    /// Remove the character before the cursor.
    ///
    /// This is accomplished by first moving the cursor left,
    /// then deleting the character under the cursor.
    fn remove_char(&mut self) {
        if self.line_pos > 0 {
            // First, move left.
            self.move_left();

            // Then, delete the character under the curosr.
            let current_char_pos = self.line_pos;
            let line = self.current_line_mut();
            // Calculate the byte position from character index
            let byte_pos = get_byte_index_from_char_index(line, current_char_pos);
            line.remove(byte_pos);
        } else {
            // Delete the "newline character"
            if self.cursor.row > 0 {
                // Remove & store the line
                let line = self.lines.remove(self.cursor.row);

                // Move the cursor to the end of the previous line
                self.move_up();
                self.move_to_end_of_line();

                // Save the cursor position before inserting
                let col = self.cursor.col;
                let line_pos = self.line_pos;

                // Insert the deleted line
                self.insert_string(line);

                // Move the cursor back to the correct position
                // (before the inserted string)
                self.cursor.col = col;
                self.line_pos = line_pos;
            }
        }
    }

    fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor.row = 0;
        self.move_to_start_of_line();
    }

    fn insert_new_line(&mut self) {
        let line_pos = self.line_pos;
        let current_line = self.current_line_mut();

        let byte_pos = get_byte_index_from_char_index(current_line, line_pos);
        let rest_of_line = current_line.drain(byte_pos..).collect();
        self.lines.insert(self.cursor.row + 1, rest_of_line);

        self.move_down();
        self.move_to_start_of_line();
    }
}

/// Cursor movement functions
impl TextArea {
    /// Move the cursor right, accounting for unicode widths.
    /// Return `true` if the operation succeeded, otherwise `false`.
    #[instrument]
    fn move_right(&mut self) -> bool {
        let line = self.current_line();
        // Can't move right from the end of the line.
        if self.line_pos < line.len() {
            let current_char = line.chars().nth(self.line_pos);
            // This should never be None, but we should check
            // that the current character is actually in the line.
            if let Some(c) = current_char {
                let width = c.width().unwrap_or(0);
                self.line_pos += 1;
                self.cursor.col += width;
                return true;
            }
        }
        false
    }

    /// Move the cursor left, accounting for unicode widths.
    /// Return `true` if the operation succeeded, otherwise `false`.
    #[instrument]
    fn move_left(&mut self) -> bool {
        let line = self.current_line();

        // Can't move right from the start of the line.
        if self.line_pos > 0 {
            let prev_char = line.chars().nth(self.line_pos - 1);
            // This should never be None, but we should check
            // that the current character is actually in the line.
            if let Some(c) = prev_char {
                let width = c.width().unwrap_or(0);
                self.line_pos -= 1;
                self.cursor.col -= width;
                return true;
            }
        }
        false
    }

    /// Move the cursor up, to a valid column in the previous row.
    /// Return `true` if the operation succeeded, otherwise `false`.
    #[instrument]
    fn move_up(&mut self) -> bool {
        info!("A");
        if self.cursor.row > 0 {
            info!("B");
            let prev_line = &self.lines[self.cursor.row - 1];
            self.cursor.row -= 1;
            if let Some(LinePosition { col, char_index }) =
                get_line_pos_of_column(&prev_line, self.cursor.col)
            {
                info!("C");
                self.cursor.col = col;
                self.line_pos = char_index;
            } else {
                info!("D");
                // If the line is not long enough, move the cursor to the last character
                self.cursor.col = prev_line.width();
                self.line_pos = prev_line.len();
            }
            true
        } else {
            info!("E");
            false
        }
    }

    /// Move the cursor down, to a valid column in the next row.
    /// Return `true` if the operation succeeded, otherwise `false`.
    #[instrument]
    fn move_down(&mut self) -> bool {
        if self.cursor.row < self.lines.len() - 1 {
            let next_line = &self.lines[self.cursor.row + 1];
            self.cursor.row += 1;
            if let Some(LinePosition { col, char_index }) =
                get_line_pos_of_column(&next_line, self.cursor.col)
            {
                self.cursor.col = col;
                self.line_pos = char_index;
            } else {
                // If the line is not long enough, move the cursor to the last character
                self.cursor.col = next_line.width();
                self.line_pos = next_line.len();
            }
            true
        } else {
            false
        }
    }

    /// Move the cursor to the end of the row.
    #[instrument]
    fn move_to_end_of_line(&mut self) {
        let line = self.current_line();
        let nchars = line.chars().count();
        self.cursor.col = line.width();
        self.line_pos = nchars;
    }

    /// Move the cursor to the start of the row.
    #[instrument]
    fn move_to_start_of_line(&mut self) {
        self.line_pos = 0;
        self.cursor.col = 0;
    }
}

/// Specify the position of a character in a line,
/// both in terms of index (count chars) and columns (unicode widths).
struct LinePosition {
    /// The column this character occupies (based on unicode-widths)
    col: usize,
    /// The index of the character in the string
    char_index: usize,
}

/// Determine the character index of the string
/// such that the unicode width of the string up
/// to this point matches the desired width.
///
/// If the specified column is not the first column of
/// a character (in case of multi-column characters),
/// the beginning column of that character will be returned.
///
/// If the line is not long enough, `None` is returned.
fn get_line_pos_of_column(s: &str, col: usize) -> Option<LinePosition> {
    let mut col_counter = 0;
    for (i, c) in s.chars().enumerate() {
        let char_width = c.width().unwrap_or(0);
        let next_col = col_counter + char_width;
        if next_col > col {
            return Some(LinePosition {
                col: col_counter,
                char_index: i,
            });
        }
        col_counter += char_width;
    }

    None
}

/// Convert a char index to a byte index for a given string.
fn get_byte_index_from_char_index(s: &str, char_ind: usize) -> usize {
    // TODO: This seems inefficient
    s.chars().take(char_ind).map(char::len_utf8).sum()
}

// Action::NewChar(c) => state.input.push(c),
// Action::Backspace => {
//     state.input.pop();
// }
// Action::Send => {
//     let text = app.input.drain(..).collect();
//     let message = Message::new(text, MessageKind::Outbound);
//     // TODO: How to modify app state outside of component?
//     app.messages.push(message);
// }
// Action::NewLine => todo!(), // TODO
// Action::Clear => todo!(),

impl TextArea {
    pub fn update(&mut self, action: Action) {
        match action {
            Action::NewChar(c) => self.insert_char(c),
            // TODO: Handle delete line / no-op (if no content)
            Action::Backspace => self.remove_char(),
            Action::NewLine => self.insert_new_line(),
            Action::Clear => self.clear(),
            Action::MoveLeft => {
                self.move_left();
            }
            Action::MoveRight => {
                self.move_right();
            }
            Action::MoveUp => {
                self.move_up();
            }
            Action::MoveDown => {
                self.move_down();
            }
            Action::MoveHome => self.move_to_start_of_line(),
            Action::MoveEnd => self.move_to_end_of_line(),
        }
    }

    pub fn render(&self, _area: &Rect) -> impl Widget {
        Paragraph::new(self.content())
            // .style(match app.input_mode {
            //     InputMode::Normal => Style::default(),
            //     InputMode::Editing => Style::default().fg(Color::Yellow),
            // })
            .block(Block::default().borders(Borders::ALL).title("Input"))
    }
}
