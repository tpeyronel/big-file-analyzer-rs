use std::{
    cmp::Ordering,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use crate::file::ReadRetry;

const TAB_SIZE: usize = 8;

const ASCII_HT: u8 = 9;
const ASCII_LF: u8 = 10;
const ASCII_CR: u8 = 13;

const CHUNK_SIZE: usize = 64;

#[derive(Debug)]
pub struct FileWindow {
    pub first_line: usize,
    pub first_column: usize,
    pub lines: usize,
    pub columns: usize,
}

#[derive(Debug)]
pub struct BigFileEditor {
    chunks: Vec<FileChunkIndex>,
    reader: BufReader<File>,
}

impl BigFileEditor {
    pub fn from_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self::from_file(file))
    }

    pub fn from_file(file: File) -> Self {
        Self::from_reader(std::io::BufReader::new(file))
    }

    pub fn from_reader(mut reader: BufReader<File>) -> Self {
        let mut buf = vec![0; 64 * 1024 * 1024];

        let mut chunks = vec![];

        let mut first_line = 0;
        let mut first_column = 0;
        let mut lines = 0;
        let mut columns = 0;
        let mut chunk_first_byte = 0;
        let mut chunk_bytes = 0;
        loop {
            let bytes_read = reader.read(&mut buf).expect("TODO");
            if bytes_read == 0 {
                break;
            }

            for i in 0..bytes_read as u32 {
                let c = buf[i as usize];
                if 32 <= c && c <= 127 {
                    print!("{}", c as char);
                } else {
                    print!("\\{}", c);
                }

                if c == ASCII_LF {
                    lines += 1;
                    columns = 0;
                } else if c == ASCII_HT {
                    columns += TAB_SIZE;
                } else if 32 <= c && c <= 127 {
                    columns += 1;
                }

                chunk_bytes += 1;

                if chunk_bytes >= CHUNK_SIZE {
                    chunks.push(FileChunkIndex {
                        first_byte: chunk_first_byte,
                        bytes_len: chunk_bytes,
                        first_line,
                        first_line_offset: first_column,
                        last_line: first_line + lines,
                        last_line_length: columns,
                    });
                    println!();
                    println!("{:#?}", chunks.last().unwrap());

                    chunk_first_byte += chunk_bytes;
                    chunk_bytes = 0;

                    first_line += lines;
                    first_column = columns;

                    lines = 0;
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
                println!();
                println!("{:#?}", chunks.last().unwrap());
            }
        }

        Self {
            chunks,
            reader,
        }
    }
}

impl BigFileEditor {
    pub fn read_window(&mut self, window: &FileWindow) -> String {
        let mut s = String::new();

        for l in window.first_line..window.first_line + window.lines {
            // Find the chunk in which l:window.first_column is.
            let Ok(first_chunk_index) = self.chunks.binary_search_by(|chunk| {
                if l < chunk.first_line {
                    Ordering::Less
                } else if l > chunk.last_line {
                    Ordering::Greater
                } else if chunk.first_line == chunk.last_line {
                    if window.first_column < chunk.first_line_offset {
                        Ordering::Less
                    } else if window.first_column > chunk.last_line_length {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                } else if l == chunk.first_line && window.first_column < chunk.first_line_offset {
                    Ordering::Less
                } else if l == chunk.last_line && window.first_column > chunk.last_line_length {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            }) else {
                // Window first column is past the line's end.
                s.push('\n');
                continue;
            };

            let first_chunk = &self.chunks[first_chunk_index];
            assert!(first_chunk.first_line <= l && l <= first_chunk.last_line);
            assert!(first_chunk.first_line != l || first_chunk.first_line_offset <= window.first_column);
            assert!(first_chunk.last_line != l || window.first_column <= first_chunk.last_line_length);

            // Read first chunk (for finding beginning of first_line).
            let mut buf = vec![0u8; first_chunk.bytes_len];
            self.reader.seek(SeekFrom::Start(first_chunk.first_byte as u64));
            let mut bytes_read = self.reader.read_with_retry(&mut buf).unwrap();
            assert_eq!(bytes_read, first_chunk.bytes_len); // TODO: handle

            let mut buf_idx = 0;

            let mut curr_line = first_chunk.first_line;
            while curr_line != l {
                assert!(buf_idx < bytes_read);

                let c = buf[buf_idx];
                if c == 10 {
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
                if c == 10 {
                    // Should not happen
                    todo!();
                } else if c == 9 {
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
                    if c == 10 {
                        columns_read = window.columns;
                        break;
                    } else if c == 9 {
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
            s.push('\n');
        }

        return s;
    }
}

#[derive(Debug)]
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
    last_line_length: usize,
}
