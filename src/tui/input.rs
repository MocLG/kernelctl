/*
 * kernelctl — unified kernel and boot configuration management across Linux bootloaders.
 * Copyright (C) 2026 Luka Gejak
 *
 * This program is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License, version 3, as published
 * by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 * more details.
 *
 * You should have received a copy of the GNU General Public License along
 * with this program. If not, see <https://www.gnu.org/licenses/>.
 *
 * Alternatively, this file is available under a commercial licence that lifts
 * the obligations of the GPL. Enquiries: lukagejak5@gmail.com
 */
//! A single-line text input.
//!
//! The cmdline editor and the filter box both need one, and neither needs a
//! full editor widget - just cursor movement, insertion, deletion and
//! word-wise operations. Positions are tracked as byte offsets that always sit
//! on a character boundary, so a multi-byte character cannot be split.

#[derive(Debug, Clone, Default)]
pub struct TextInput {
    value: String,
    /// Byte offset of the cursor, always on a character boundary.
    cursor: usize,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> TextInput {
        let value = value.into();
        let cursor = value.len();
        TextInput { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn into_value(self) -> String {
        self.value
    }

    /// Cursor position measured in characters, for placing the terminal cursor.
    pub fn cursor_column(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

    pub fn insert(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_boundary(self.cursor);
        self.value.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let next = self.next_boundary(self.cursor);
        self.value.replace_range(self.cursor..next, "");
    }

    /// Delete the word before the cursor, as ctrl-w does in a shell.
    pub fn delete_word(&mut self) {
        let start = self.word_start();
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Delete from the cursor to the start of the line (ctrl-u).
    pub fn delete_to_start(&mut self) {
        self.value.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    /// Delete from the cursor to the end of the line (ctrl-k).
    pub fn delete_to_end(&mut self) {
        self.value.truncate(self.cursor);
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    pub fn right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor = self.next_boundary(self.cursor);
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Start of the word before the cursor: skip any run of spaces, then the
    /// word itself.
    fn word_start(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 {
            let prev = self.prev_boundary(at);
            if !self.value[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        while at > 0 {
            let prev = self.prev_boundary(at);
            if self.value[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        at
    }

    fn prev_boundary(&self, from: usize) -> usize {
        let mut at = from.saturating_sub(1);
        while at > 0 && !self.value.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    fn next_boundary(&self, from: usize) -> usize {
        let mut at = (from + 1).min(self.value.len());
        while at < self.value.len() && !self.value.is_char_boundary(at) {
            at += 1;
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_the_cursor_at_the_end() {
        let input = TextInput::new("root=/dev/sda1");
        assert_eq!(input.cursor_column(), 14);
    }

    #[test]
    fn inserts_and_deletes() {
        let mut input = TextInput::new("ro");
        input.insert(' ');
        input.insert('q');
        assert_eq!(input.value(), "ro q");

        input.backspace();
        assert_eq!(input.value(), "ro ");
    }

    #[test]
    fn inserts_at_the_cursor_not_the_end() {
        let mut input = TextInput::new("ab");
        input.home();
        input.insert('X');
        assert_eq!(input.value(), "Xab");
        assert_eq!(input.cursor_column(), 1);
    }

    #[test]
    fn deletes_under_the_cursor() {
        let mut input = TextInput::new("abc");
        input.home();
        input.delete();
        assert_eq!(input.value(), "bc");
    }

    #[test]
    fn deletes_a_word_at_a_time() {
        let mut input = TextInput::new("root=/dev/sda1 ro quiet");
        input.delete_word();
        assert_eq!(input.value(), "root=/dev/sda1 ro ");

        input.delete_word();
        assert_eq!(input.value(), "root=/dev/sda1 ");
    }

    #[test]
    fn deletes_to_the_line_ends() {
        let mut input = TextInput::new("abcdef");
        input.home();
        input.right();
        input.right();
        input.delete_to_end();
        assert_eq!(input.value(), "ab");

        let mut input = TextInput::new("abcdef");
        input.home();
        input.right();
        input.right();
        input.delete_to_start();
        assert_eq!(input.value(), "cdef");
        assert_eq!(input.cursor_column(), 0);
    }

    #[test]
    fn never_splits_a_multibyte_character() {
        let mut input = TextInput::new("héllo→");
        input.backspace();
        assert_eq!(input.value(), "héllo");
        input.home();
        input.right();
        input.delete();
        assert_eq!(input.value(), "hllo", "the two-byte é is removed whole");
    }

    #[test]
    fn cursor_movement_stops_at_the_ends() {
        let mut input = TextInput::new("ab");
        input.home();
        input.left();
        assert_eq!(input.cursor_column(), 0);
        input.end();
        input.right();
        assert_eq!(input.cursor_column(), 2);
    }

    #[test]
    fn backspace_on_an_empty_input_is_a_no_op() {
        let mut input = TextInput::default();
        input.backspace();
        input.delete();
        assert_eq!(input.value(), "");
    }

    #[test]
    fn cursor_column_counts_characters_not_bytes() {
        let input = TextInput::new("日本語");
        // Nine bytes, three characters.
        assert_eq!(input.value().len(), 9);
        assert_eq!(input.cursor_column(), 3);
    }
}
