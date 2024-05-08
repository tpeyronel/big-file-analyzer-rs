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

#[derive(Debug)]
pub struct FileWindow {
    pub first_line: usize,
    pub first_column: usize,
    pub lines: usize,
    pub columns: usize,
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
        let mut buf = vec![0; 64 * 1024 * 1024];

        let mut chunks = vec![];

        let mut first_line = 0;
        let mut first_column = 0;
        let mut lines = 0;
        let mut columns = 0;
        let mut chunk_first_byte = 0;
        let mut chunk_bytes = 0;
        let mut prev_cr = false;
        loop {
            let bytes_read = reader.read(&mut buf).expect("TODO");
            if bytes_read == 0 {
                break;
            }

            for i in 0..bytes_read as u32 {
                let c = buf[i as usize];

                if c == ASCII_CR {
                    lines += 1;
                    columns = 0;
                } else if c == ASCII_LF && !prev_cr {
                    lines += 1;
                    columns = 0;
                } else if c == ASCII_HT {
                    columns += TAB_SIZE - (columns % TAB_SIZE);
                } else if c <= 127 {
                    columns += 1;
                }

                chunk_bytes += 1;
                prev_cr = c == ASCII_CR;

                if chunk_bytes >= CHUNK_SIZE {
                    chunks.push(FileChunkIndex {
                        first_byte: chunk_first_byte,
                        bytes_len: chunk_bytes,
                        first_line,
                        first_line_offset: first_column,
                        last_line: first_line + lines,
                        last_line_length: columns,
                    });

                    chunk_first_byte += chunk_bytes;
                    chunk_bytes = 0;

                    first_line += lines;
                    first_column = columns;

                    lines = 0;
                }
            }
        }

        if chunk_bytes > 0 {
            chunks.push(FileChunkIndex {
                first_byte: chunk_first_byte,
                bytes_len: chunk_bytes,
                first_line,
                first_line_offset: first_column,
                last_line: first_line + lines,
                last_line_length: columns,
            });
        }

        Self { chunks, reader }
    }

    pub fn read_window(&mut self, window: &FileWindow) -> String {
        let mut s = String::new();

        'lines: for l in window.first_line..window.first_line + window.lines {
            if l != window.first_line {
                s.push('\n');
            }

            // Find the chunk in which l:window.first_column is.
            let Some(first_chunk_index) = self.find_chunk_by_coordinates(l, window.first_column)
            else {
                // The line does not exist or window.first_column is past the line's end.
                s.push('\n');
                continue;
            };

            let first_chunk = &self.chunks[first_chunk_index];
            assert!(first_chunk.first_line <= l && l <= first_chunk.last_line);
            assert!(
                first_chunk.first_line != l || first_chunk.first_line_offset <= window.first_column
            );
            assert!(
                first_chunk.last_line != l || window.first_column <= first_chunk.last_line_length
            );

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

            let mut curr_line = first_chunk.first_line;
            while curr_line != l {
                assert!(buf_idx < bytes_read);

                let c = buf[buf_idx];
                if c == ASCII_LF {
                    curr_line += 1;
                }

                buf_idx += 1;
            }

            let mut curr_column = if l == first_chunk.first_line {
                first_chunk.first_line_offset
            } else {
                0
            };
            while curr_column < window.first_column {
                assert!(buf_idx < bytes_read);

                let c = buf[buf_idx];
                if c == ASCII_LF {
                    // We reached EOL but we haven't even reached the first column.
                    // This simply means that this line does not enter the window.
                    // Therefore, we simply go to the next line.
                    continue 'lines;
                } else if c == ASCII_HT {
                    // TODO: handle better
                    curr_column += TAB_SIZE;
                } else if 32 <= c && c <= 127 {
                    curr_column += 1;
                }

                buf_idx += 1;
            }

            // We found the byte for l at column window.first_column (stored in buf_idx).
            // Now we will read bytes until reaching (at least) window.columns chars.
            let mut columns_read = 0;
            let mut chunk_index = first_chunk_index;
            loop {
                while buf_idx < bytes_read && columns_read < window.columns {
                    let c = buf[buf_idx];
                    if c == ASCII_LF {
                        columns_read = window.columns;
                        break;
                    } else if c == ASCII_HT {
                        columns_read += TAB_SIZE;
                    } else if 32 <= c && c <= 127 {
                        columns_read += 1;
                    }
                    s.push(c as char);
                    buf_idx += 1;
                }

                chunk_index += 1;

                if columns_read >= window.columns || chunk_index >= self.chunks.len() {
                    break;
                }

                let chunk = &self.chunks[chunk_index];
                buf_idx = 0;
                buf.resize(chunk.bytes_len, 0);
                bytes_read = self.reader.read_with_retry(&mut buf).unwrap();

                assert!(bytes_read > 0);
            }
        }

        return s;
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
}

impl BigFileEditor<Cursor<String>> {
    fn from_str(s: &str) -> Self {
        Self::from_reader(Cursor::new(s.to_owned()))
    }
}

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
}
