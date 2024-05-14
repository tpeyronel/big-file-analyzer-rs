use std::{
    cmp::Ordering,
    fs::File,
    io::{self, BufReader, Cursor, Read, Seek, SeekFrom},
    path::Path,
};

use crate::file::ReadRetry;

const TAB_SIZE: usize = 8;

const ASCII_HT: u8 = 9;
const ASCII_LF: u8 = 10;
const ASCII_CR: u8 = 13;

const CHUNK_SIZE: usize = 4096;

#[derive(Debug, Clone)]
pub struct FileWindowFrame {
    pub first_line: usize,
    pub first_column: usize,
    pub lines: usize,
    pub columns: usize,
}

#[derive(Debug)]
pub struct FileWindow {
    pub lines: Vec<String>,
    pub frame: FileWindowFrame,
}

#[derive(Debug)]
struct WalkState {
    curr_line: usize,
    curr_column: usize,
    prev_cr: bool,
}

#[derive(Debug)]
pub struct BigFileEditor<T: Read + Seek> {
    chunks: Vec<FileChunkIndex>,
    reader: T,
}

impl BigFileEditor<BufReader<File>> {
    pub fn from_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self::from_file(file))
    }
}

impl BigFileEditor<BufReader<File>> {
    pub fn from_file(file: File) -> Self {
        Self::from_reader(std::io::BufReader::new(file))
    }
}

impl<T: Read + Seek> BigFileEditor<T> {
    pub fn from_reader(mut reader: T) -> Self {
        // TODO: seek 0 (?
        // TODO: handle potential UTF-8 BOM.
        let mut buf = vec![0; 64 * 1024 * 1024];

        let mut chunks = vec![];

        let mut first_line = 0;
        let mut first_column = 0;
        let mut chunk_first_byte = 0;
        let mut chunk_bytes = 0;
        let mut walk_state = WalkState {
            curr_line: 0,
            curr_column: 0,
            prev_cr: false,
        };

        loop {
            let bytes_read = reader.read(&mut buf).expect("TODO");
            if bytes_read == 0 {
                break;
            }

            for i in 0..bytes_read as u32 {
                let c = buf[i as usize];

                // We want to make sure that we dont split a \r\n between two chunks.
                // That's why we check for (!prev_cr || c != ASCII_LF).
                // That's also the reason why we push *before* processing c.
                // Explanation: if !prev_cr, then we can push as there is no risk of \r\n,
                // but if prev_cr, then there are two cases: c == \n or c != \n.
                // If c == \n, then we *don't* push, because we want that character inside the
                // chunk (it will be pushed in the next iteration, as prev_cr will be false).
                // If c != \n, then we can safely push.
                if chunk_bytes >= CHUNK_SIZE && (!walk_state.prev_cr || c != ASCII_LF) {
                    chunks.push(FileChunkIndex {
                        first_byte: chunk_first_byte,
                        bytes_len: chunk_bytes,
                        first_line,
                        first_line_offset: first_column,
                        last_line: walk_state.curr_line,
                        last_line_length: walk_state.curr_column - first_column,
                    });

                    chunk_first_byte += chunk_bytes;
                    chunk_bytes = 0;

                    first_line = walk_state.curr_line;
                    first_column = walk_state.curr_column;
                }

                Self::advance_char(c, &mut walk_state);

                chunk_bytes += 1;
            }
        }

        if chunk_bytes > 0 {
            chunks.push(FileChunkIndex {
                first_byte: chunk_first_byte,
                bytes_len: chunk_bytes,
                first_line,
                first_line_offset: first_column,
                last_line: walk_state.curr_line,
                last_line_length: walk_state.curr_column - first_column,
            });
        }

        Self { chunks, reader }
    }

    pub fn read_window(&mut self, frame: &FileWindowFrame) -> FileWindow {
        let mut lines = vec![];

        let first_column = frame.first_column - (frame.first_column % TAB_SIZE);

        for l in frame.first_line..frame.first_line + frame.lines {
            let line = self.read_line(l, first_column, frame.columns);
            lines.push(line);
        }

        return FileWindow {
            lines,
            frame: FileWindowFrame {
                first_column,
                ..*frame
            },
        };
    }

    // first_column should be a multiple of TAB_SIZE
    fn read_line(&mut self, l: usize, first_column: usize, columns: usize) -> String {
        assert_eq!(first_column % TAB_SIZE, 0);

        // Find the chunk in which l:window.first_column is.
        let Some(first_chunk_index) = self.find_chunk_by_coordinates(l, first_column) else {
            // The line does not exist or window.first_column is past the line's end.
            return String::new();
        };

        let first_chunk = &self.chunks[first_chunk_index];
        assert!(first_chunk.first_line <= l && l <= first_chunk.last_line);
        assert!(first_chunk.first_line != l || first_chunk.first_line_offset <= first_column);
        assert!(first_chunk.last_line != l || first_column <= first_chunk.last_line_length);

        // We know that l:window.first_column is somewhere inside first_chunk.
        // Now we need to find exactly where it is.

        // Read first chunk.
        let mut buf = vec![0u8; first_chunk.bytes_len];
        self.reader
            .seek(SeekFrom::Start(first_chunk.first_byte as u64))
            .expect("TODO");
        let mut bytes_read = self.reader.read_with_retry(&mut buf).unwrap();
        assert_eq!(bytes_read, first_chunk.bytes_len); // TODO: handle

        let mut buf_idx = 0;

        let mut walk_state = WalkState {
            curr_line: first_chunk.first_line,
            curr_column: first_chunk.first_line_offset,
            prev_cr: false,
        };

        while walk_state.curr_line < l
            || (walk_state.curr_line == l && walk_state.curr_column < first_column)
        {
            if buf_idx == bytes_read {
                // We ran out of bytes without reaching (l, first_column).
                break;
            }

            let c = buf[buf_idx];

            Self::advance_char(c, &mut walk_state);

            buf_idx += 1;
        }

        if walk_state.curr_line < l {
            // We didn't reach line l in this chunk, which means that either
            // the file changed or that this line is past the EOF.
            return String::new();
        }

        if walk_state.curr_line > l {
            // We skipped past line l before reaching the first column of the line l.
            // This simply means that line l does not reach the window.
            // Therefore, we simply go to the next line.
            return String::new();
        }

        if walk_state.curr_column < first_column {
            // We didn't reach first_column column of the line l.
            return String::new();
        }

        // Because first_column is tab aligned, (at this point) this should always be true.
        assert_eq!(walk_state.curr_column, first_column);

        // Make sure to not include the \n of a \r\n of the previous line
        if walk_state.prev_cr && buf_idx < bytes_read && buf[buf_idx] == ASCII_LF {
            buf_idx += 1;
        }

        // We found the byte for coord (l, first_column) (stored in buf_idx).
        // Now we will read bytes until reaching (at least) `columns` chars.
        let mut line = String::new();
        let last_column = first_column + columns;
        let mut chunk_index = first_chunk_index;
        loop {
            while buf_idx < bytes_read && walk_state.curr_column < last_column {
                let c = buf[buf_idx];

                if walk_state.curr_line > l {
                    if c == ASCII_LF && walk_state.prev_cr {
                        line.push(ASCII_LF as char);
                    }
                    break;
                }

                // We push before checking as we do want the EOL to be part of the line.
                line.push(c as char);

                Self::advance_char(c, &mut walk_state);

                buf_idx += 1;
            }

            chunk_index += 1;

            // If we read all the requested columns,
            // or if we reached EOF (indicated by having no further chunks),
            // then break and return what we have read.
            if walk_state.curr_column >= last_column || chunk_index >= self.chunks.len() {
                break;
            }

            // Prepare for next iteration by reading the next chunk.
            let chunk = &self.chunks[chunk_index];
            buf_idx = 0;
            buf.resize(chunk.bytes_len, 0);
            bytes_read = self.reader.read_with_retry(&mut buf).expect("TODO");

            assert!(bytes_read > 0);
        }

        line
    }

    fn find_chunk_by_coordinates(&self, line: usize, column: usize) -> Option<usize> {
        self.chunks
            .binary_search_by(|chunk| {
                if line < chunk.first_line {
                    Ordering::Greater
                } else if line > chunk.last_line {
                    Ordering::Less
                } else if chunk.first_line == chunk.last_line {
                    // (by sandwich, chunk.first_line == l == chunk.last_line)

                    if column < chunk.first_line_offset {
                        Ordering::Greater
                    } else if column >= chunk.first_line_offset + chunk.last_line_length {
                        Ordering::Less
                    } else {
                        Ordering::Equal
                    }
                } else if line == chunk.first_line && column < chunk.first_line_offset {
                    Ordering::Greater
                } else if line == chunk.last_line && column >= chunk.last_line_length {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()
    }

    fn advance_char(c: u8, state: &mut WalkState) {
        // TODO: handle UTF-8

        if c == ASCII_CR {
            state.curr_line += 1;
            state.curr_column = 0;
        } else if c == ASCII_LF {
            if !state.prev_cr {
                state.curr_line += 1;
                state.curr_column = 0;
            }
        } else if c == ASCII_HT {
            state.curr_column += TAB_SIZE - (state.curr_column % TAB_SIZE);
        } else if c <= 127 {
            state.curr_column += 1;
        }

        state.prev_cr = c == ASCII_CR;
    }
}

impl BigFileEditor<Cursor<String>> {
    fn from_str(s: &str) -> Self {
        Self::from_reader(Cursor::new(s.to_owned()))
    }
}

// TODO: we probably don't need to store last_line and last_line_length
// This is because we could perform binary search based on just first_line
// and first_line_offset, and (most of the time), we would get returned
// Err(idx), where idx is actually correct.
#[derive(Debug, PartialEq, Eq)]
struct FileChunkIndex {
    // The first byte of this chunk (relative to the beginning of the file).
    first_byte: usize,
    // The length in bytes of this chunk.
    bytes_len: usize,
    // // The "screen space" length of this chunk.
    // display_len: usize,

    // The first line in this chunk (relative to the first line of the file).
    first_line: usize,
    first_line_offset: usize,

    // The last line in this chunk (relative to the first line of the file).
    last_line: usize,
    // If first_line = last_line, then this length is relative to first_line_offset.
    last_line_length: usize,
}

#[cfg(test)]
mod indexing_tests {
    use super::*;

    fn single_chunk_test(s: &str, expected: FileChunkIndex) {
        let editor = BigFileEditor::from_str(s);
        assert!(editor.chunks.len() == 1);
        let chunk = editor.chunks.first().unwrap();
        assert_eq!(*chunk, expected);
    }

    #[test]
    fn empty_file() {
        let editor = BigFileEditor::from_str("");

        assert!(editor.chunks.is_empty());
    }

    #[test]
    fn one_char() {
        single_chunk_test(
            "a",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 1,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 1,
            },
        );
    }

    #[test]
    fn two_chars() {
        single_chunk_test(
            "aa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 2,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 2,
            },
        );
    }

    #[test]
    fn aligned_tab() {
        single_chunk_test(
            "\taa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 3,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: TAB_SIZE + 2,
            },
        );
    }

    #[test]
    fn non_aligned_tab() {
        single_chunk_test(
            "aa\taa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 5,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: TAB_SIZE + 2,
            },
        );
    }

    #[test]
    fn far_aligned_tab() {
        single_chunk_test(
            "aaaabbbbccccdddd\taa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 16 + 1 + 2,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 16 + TAB_SIZE + 2,
            },
        );
    }

    #[test]
    fn far_non_aligned_tab() {
        single_chunk_test(
            "aaaabbbbccccddddaa\taa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 16 + 2 + 1 + 2,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 16 + TAB_SIZE + 2,
            },
        );
        single_chunk_test(
            "aaaabbbbccccddddaaaabb\taa",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 16 + 6 + 1 + 2,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 16 + TAB_SIZE + 2,
            },
        );
    }

    #[test]
    fn control_chars() {
        single_chunk_test(
            // Special characters are \x09 (\t), \x0A (\n), and \x0D (\r)
            // Both the \x0A and the \x0D should be treated as a new line.
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 16,
                first_line: 0,
                first_line_offset: 0,
                last_line: 2,
                last_line_length: 2,
            },
        );
        single_chunk_test(
            // All of these characters are considered garbage,
            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E\x1F",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 16,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 16,
            },
        );
    }

    #[test]
    fn control_chars_of_width_1() {
        single_chunk_test(
            // Every control char except \t, \r, and \n.
            // Therefore, we have 29 control chars, all of width 1.
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\x0B\x0C\x0E\x0F\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E\x1F",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 29,
                first_line: 0,
                first_line_offset: 0,
                last_line: 0,
                last_line_length: 29,
            },
        );
    }

    #[test]
    fn newlines_lf() {
        single_chunk_test(
            "\n\n\n\n",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 4,
                first_line: 0,
                first_line_offset: 0,
                last_line: 4,
                last_line_length: 0,
            },
        );
    }

    #[test]
    fn newlines_crlf() {
        single_chunk_test(
            "\r\n\r\n\r\n\r\n",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 8,
                first_line: 0,
                first_line_offset: 0,
                last_line: 4,
                last_line_length: 0,
            },
        );
    }

    #[test]
    fn newlines_cr() {
        single_chunk_test(
            "\r\r\r\r",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 4,
                first_line: 0,
                first_line_offset: 0,
                last_line: 4,
                last_line_length: 0,
            },
        );
    }

    #[test]
    fn newlines_all_mixed() {
        single_chunk_test(
            "\n\r\n\r\r\n\n",
            FileChunkIndex {
                first_byte: 0,
                bytes_len: 7,
                first_line: 0,
                first_line_offset: 0,
                last_line: 5,
                last_line_length: 0,
            },
        );
    }
}
