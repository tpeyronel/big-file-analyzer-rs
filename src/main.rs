use std::{
    cmp::Ordering,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    time::Instant,
};

use file::ReadRetry;

mod file;

enum EolSequence {
    LF,
    CRLF,
}

#[derive(Debug)]
struct FileChunkIndex {
    // The first byte of this chunk (relative to the beginning of the file).
    first_byte: usize,
    // The length in bytes of this chunk.
    bytes_len: usize,
    // The "screen space" length of this chunk.
    display_len: usize,

    // The first line in this chunk (relative to the first line of the file).
    first_line: usize,
    // The amount of lines in this chunk.
    line_count: usize,
}

#[derive(Debug)]
struct FileIndex {
    chunks: Vec<FileChunkIndex>,
}

impl FileIndex {
    pub fn read_window(self, reader: &mut BufReader<File>, window: &FileWindow) -> String {
        let mut s = String::new();

        for l in window.first_line..window.first_line + window.lines {
            // TODO: not unwrap() i guess
            let first_chunk_index = self
                .chunks
                .binary_search_by(|chunk| {
                    if l < chunk.first_line {
                        Ordering::Less
                    } else if l > chunk.first_line + chunk.line_count {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
                .unwrap();

            let first_chunk = &self.chunks[first_chunk_index];
            reader.seek(SeekFrom::Start(first_chunk.first_byte as u64));

            let mut buf = vec![0u8; first_chunk.bytes_len];
            reader.read_with_retry(&mut buf).unwrap();

            let mut newlines_read = 0;
            let mut buf_idx = 0;
            while newlines_read != window.first_line - first_chunk.first_line {
                let c = buf[buf_idx];
                if c == 10 {
                    newlines_read += 1;
                }

                buf_idx += 1;
            }

            let mut columns_read = 0;
            let mut chunk_index = first_chunk_index;
            loop {
                while buf_idx < buf.len() && columns_read < window.columns {
                    let c = buf[buf_idx];
                    if c == 9 {
                        columns_read += 8;
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
                reader.read_with_retry(&mut buf).unwrap();
            }
            s.push('\n');
        }

        return s;
    }
}

#[derive(Debug)]
struct FileWindow {
    first_line: usize,
    first_column: usize,
    lines: usize,
    columns: usize,
}

// fn index_lines_chars(s: &str) -> Vec<LineData> {
//     let mut lines_data = vec![];

//     let mut i = 0;
//     for c in s.chars() {
//         if c == '\n' {
//             lines_data.push(i);
//         }
//         i += 1;
//     }

//     lines_data
// }

fn index_file_bytes(s: &str) -> FileIndex {
    let mut lines_data = vec![];

    let mut line_start = 0;
    let mut line_length = 0;
    let mut line_display_length = 0;
    let mut cr = false;

    for &c in s.as_bytes() {
        line_length += 1;

        if c > 127 {
            panic!("non-ascii found ({})", c);
        } else if c == 9 {
            /* TAB (\t) */
            line_display_length += 8;
        } else if c == 13 {
            /* CR (\r) */
            cr = true;
        } else if c == 10 {
            /* NL (\n) */
            if cr {
                lines_data.push(LineIndex {
                    line_start,
                    line_length,
                    line_display_length,
                });
                line_start += line_length;
            }
            cr = false;
        } else if c >= 32 && c != 127 {
            line_display_length += 1;
        }
    }

    lines_data
}

// fn read_and_index_chars(filename: &str) -> Vec<LineData> {
//     let start = Instant::now();
//     let contents = std::fs::read_to_string(filename).unwrap();
//     println!("Read file in {}ms", start.elapsed().as_millis());

//     let start = Instant::now();
//     let indices = index_lines_chars(&contents);
//     println!("Indexed file in {}ms", start.elapsed().as_millis());

//     indices
// }

fn read_and_index_bytes(filename: &str) -> FileIndex {
    let start = Instant::now();
    let contents = std::fs::read_to_string(filename).unwrap();
    println!("Read file in {}ms", start.elapsed().as_millis());

    let start = Instant::now();
    let index = index_file_bytes(&contents);
    println!("Indexed file in {}ms", start.elapsed().as_millis());

    index
}

// fn read_and_index_buf_reader(filename: &str) -> Vec<LineData> {
//     let file = std::fs::File::open(filename).unwrap();
//     let mut reader = std::io::BufReader::new(file);

//     let mut buf = vec![0; 64 * 1024 * 1024];
//     let mut indices = vec![];
//     let mut cr = false;
//     while let Ok(nread) = reader.read(&mut buf) {
//         if nread == 0 {
//             break;
//         }

//         for i in 0..nread as u32 {
//             let c = buf[i as usize];

//             if c == 13 {
//                 cr = true;
//             } else if cr {
//                 if c == 10 {
//                     indices.push(i);
//                 }
//                 cr = false;
//             }
//         }
//     }

//     indices
// }

fn main() {
    let start = Instant::now();
    let filename = "index.html";
    // let indices = read_and_index_chars(filename);
    let index = read_and_index_bytes(filename);
    // let indices = read_and_index_buf_reader(filename);

    println!("Read and indexed file in {}ms", start.elapsed().as_millis());

    let file = std::fs::File::open(filename).unwrap();
    let mut reader = std::io::BufReader::new(file);
    println!("Window:");
    println!(
        "{}",
        index.read_window(
            &mut reader,
            &FileWindow {
                first_line: 2,
                first_column: 2,
                lines: 8,
                columns: 8,
            },
        )
    )
    // for line in &indices[..indices.len().min(128)] {
    //     println!("{:?}", line);
    // }
}
