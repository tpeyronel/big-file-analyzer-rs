use std::{
    cmp::Ordering,
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    ops::{Deref, DerefMut},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use futures::{select, FutureExt};
use tokio::sync::{oneshot, watch};

use crate::file::ReadRetry;

pub const TAB_SIZE: usize = 8;

const ASCII_HT: u8 = 9;
const ASCII_LF: u8 = 10;
const ASCII_CR: u8 = 13;

const INDEXING_BUFFER_SIZE: usize = 1024 * 1024;

const CHUNK_SIZE: usize = 4096;

const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl WalkState {
    fn advance_char(&mut self, c: u8) {
        // TODO: handle UTF-8

        if c == ASCII_CR {
            self.curr_line += 1;
            self.curr_column = 0;
        } else if c == ASCII_LF {
            if !self.prev_cr {
                self.curr_line += 1;
                self.curr_column = 0;
            }
        } else if c == ASCII_HT {
            self.curr_column += TAB_SIZE - (self.curr_column % TAB_SIZE);
        } else if c <= 127 {
            self.curr_column += 1;
        }

        self.prev_cr = c == ASCII_CR;
    }
}

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub struct WindowSubscriber {
    frame_tx: watch::Sender<FileWindowFrame>,
    frame_rx: watch::Receiver<FileWindowFrame>,
    index_rx: watch::Receiver<FileIndex>,
    indexing_done: bool,
    reader: Arc<Mutex<dyn ReadSeek>>,
}

impl WindowSubscriber {
    pub async fn set_frame(&mut self, frame: FileWindowFrame) {
        self.frame_tx.send(frame).expect("TODO");
    }

    pub async fn read_window(&mut self) -> Option<FileWindow> {
        select! {
            res = async {
                if !self.indexing_done {
                    self.index_rx.changed().await
                } else {
                    futures::future::pending().await
                }
            }.fuse() => {
                if res.is_err() {
                    self.indexing_done = true;
                }

                let index = self.index_rx.borrow();

                return Some(index.read_window(&self.frame_rx.borrow()));
            },
            res = self.frame_rx.changed().fuse() => {
                res.expect("TODO");

                let index = self.index_rx.borrow();

                return Some(index.read_window(&self.frame_rx.borrow()));
            },
        }
    }
}

enum BigFileEditorState {
    Indexing {
        indexing_done_rx: Option<oneshot::Receiver<()>>,
    },
    Indexed,
}

pub struct BigFileEditor {
    state: BigFileEditorState,
    index_rx: watch::Receiver<FileIndex>,
    reader: Arc<Mutex<dyn ReadSeek>>,
}

impl BigFileEditor {
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self::from_file(file))
    }

    pub fn from_file(file: File) -> Self {
        Self::from_reader(std::io::BufReader::new(file))
    }

    pub fn from_reader(reader: impl Read + Seek + Send + 'static) -> Self {
        let reader = Arc::new(Mutex::new(reader));

        let (indexing_done_tx, indexing_done_rx) = oneshot::channel();
        let (index_tx, index_rx) = watch::channel(FileIndex::new(reader.clone()));

        let reader_clone = reader.clone();
        let _indexing_thread_handle = thread::spawn(move || {
            Self::run_indexing_thread(reader_clone, index_tx, indexing_done_tx)
        });

        Self {
            state: BigFileEditorState::Indexing {
                indexing_done_rx: Some(indexing_done_rx),
            },
            index_rx,
            reader,
        }
    }

    pub fn read_window(&mut self, frame: &FileWindowFrame) -> FileWindow {
        match &mut self.state {
            BigFileEditorState::Indexing { indexing_done_rx } => {
                indexing_done_rx
                    .take()
                    .unwrap()
                    .blocking_recv()
                    .expect("TODO");
                self.state = BigFileEditorState::Indexed;
            },
            BigFileEditorState::Indexed => (),
        }

        self.index_rx.borrow().read_window(frame)
    }

    pub async fn subscribe_window(&mut self, initial_frame: FileWindowFrame) -> WindowSubscriber {
        let (frame_tx, frame_rx) = watch::channel(initial_frame);

        WindowSubscriber {
            frame_tx,
            frame_rx,
            index_rx: self.index_rx.clone(),
            indexing_done: false,
            reader: self.reader.clone(),
        }
    }

    fn run_indexing_thread(
        reader: Arc<Mutex<dyn ReadSeek + Send>>,
        index_tx: watch::Sender<FileIndex>,
        indexing_done_tx: oneshot::Sender<()>,
    ) {
        // TODO: handle potential UTF-8 BOM.
        let mut buf = vec![0; INDEXING_BUFFER_SIZE];

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

        let mut read_offset = 0;
        let mut last_update = Instant::now();

        loop {
            if last_update.elapsed() >= UPDATE_INTERVAL {
                Self::update_index(&index_tx, std::mem::replace(&mut chunks, vec![]));
                last_update = Instant::now();
            }

            let bytes_read = read_with_retry(reader.deref(), read_offset, &mut buf);
            if bytes_read == 0 {
                break;
            }
            read_offset += bytes_read;

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
                    });

                    chunk_first_byte += chunk_bytes;
                    chunk_bytes = 0;

                    first_line = walk_state.curr_line;
                    first_column = walk_state.curr_column;
                }

                walk_state.advance_char(c);

                chunk_bytes += 1;
            }
        }

        if chunk_bytes > 0 {
            chunks.push(FileChunkIndex {
                first_byte: chunk_first_byte,
                bytes_len: chunk_bytes,
                first_line,
                first_line_offset: first_column,
            });
        }

        Self::update_index(&index_tx, chunks);

        indexing_done_tx.send(()).expect("TODO");
    }

    fn update_index(index_tx: &watch::Sender<FileIndex>, new_chunks: Vec<FileChunkIndex>) {
        if new_chunks.is_empty() {
            return;
        }

        index_tx.send_modify(|index| {
            index.chunks.extend(new_chunks);
        });
    }
}

impl BigFileEditor {
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
}

struct FileIndex {
    chunks: Vec<FileChunkIndex>,
    reader: Arc<Mutex<dyn ReadSeek + Send>>,
}

impl FileIndex {
    fn new(reader: Arc<Mutex<dyn ReadSeek + Send>>) -> Self {
        Self {
            chunks: vec![],
            reader,
        }
    }

    fn read_window(&self, frame: &FileWindowFrame) -> FileWindow {
        let mut lines = vec![];

        let alignment_offset = frame.first_column % TAB_SIZE;
        let first_column = frame.first_column - alignment_offset;

        for l in frame.first_line..frame.first_line + frame.lines {
            let line = self.read_line(l, first_column, frame.columns + alignment_offset);
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
    fn read_line(&self, l: usize, first_column: usize, columns: usize) -> String {
        assert_eq!(first_column % TAB_SIZE, 0);

        // Find the chunk in which l:window.first_column is.
        let first_chunk_index = Self::find_chunk_by_coordinates(&self.chunks, l, first_column);
        assert!(first_chunk_index < self.chunks.len());

        let first_chunk = &self.chunks[first_chunk_index];

        assert!(
            first_chunk.first_line < l
                || (first_chunk.first_line == l && first_chunk.first_line_offset <= first_column)
        );
        if first_chunk_index + 1 < self.chunks.len() {
            let next_chunk = &self.chunks[first_chunk_index + 1];
            assert!(
                next_chunk.first_line > l
                    || (next_chunk.first_line == l && next_chunk.first_line_offset > first_column)
            );
        }

        // We know that if l:window.first_column exists, then it is
        // somewhere inside first_chunk. Now we need to find exactly where it is.

        // Read first chunk.
        let mut buf = vec![0u8; first_chunk.bytes_len];
        let mut read_offset = first_chunk.first_byte;
        let mut bytes_read = read_with_retry(&self.reader, read_offset, &mut buf);
        read_offset += bytes_read;
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

            walk_state.advance_char(c);

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

                walk_state.advance_char(c);

                buf_idx += 1;
            }

            chunk_index += 1;

            // If we read the entire line,
            // or if we read all the requested columns,
            // or if we reached EOF (indicated by having no further chunks),
            // then break and return what we have read.
            if walk_state.curr_line > l
                || walk_state.curr_column >= last_column
                || chunk_index >= self.chunks.len()
            {
                break;
            }

            // Prepare for next iteration by reading the next chunk.
            let chunk = &self.chunks[chunk_index];
            buf_idx = 0;
            buf.resize(chunk.bytes_len, 0);
            bytes_read = read_with_retry(&self.reader, read_offset, &mut buf);
            read_offset += bytes_read;

            assert!(bytes_read > 0);
        }

        line
    }

    fn find_chunk_by_coordinates(
        chunks: &Vec<FileChunkIndex>,
        line: usize,
        column: usize,
    ) -> usize {
        match chunks.binary_search_by(|chunk| {
            if line < chunk.first_line {
                Ordering::Greater
            } else if line > chunk.first_line {
                Ordering::Less
            } else {
                if column < chunk.first_line_offset {
                    Ordering::Greater
                } else if column > chunk.first_line_offset {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            }
        }) {
            Ok(idx) => idx,
            // Should never happen that Err(idx) == Err(0), because that would mean that (line, column)
            // is to the left of (0, 0).
            Err(idx) => idx - 1,
        }
    }
}

fn read_with_retry(reader: &Mutex<dyn ReadSeek + Send>, offset: usize, buf: &mut [u8]) -> usize {
    let mut reader = reader.lock().expect("TODO");
    reader.seek(SeekFrom::Start(offset as u64)).expect("TODO");
    reader.deref_mut().read_with_retry(buf).expect("TODO")
}

#[cfg(test)]
mod reading_tests {
    use super::*;

    #[test]
    fn read_0x0() {
        let file = concat!(
            "\n",
            "a\n",
            "bb\n",
            "ccc\n",
            "dddd\n",
            "eeeee\n",
            "ffffff\n",
            "ggggggg\n",
        );

        let mut editor = BigFileEditor::from_str(file);

        let frame = FileWindowFrame {
            first_line: 0,
            first_column: 0,
            lines: 0,
            columns: 0,
        };
        let window = editor.read_window(&frame);
        assert!(window.lines.is_empty());
        assert_eq!(window.frame, frame);

        let frame = FileWindowFrame {
            first_line: 1,
            first_column: 1,
            lines: 0,
            columns: 0,
        };
        let window = editor.read_window(&frame);
        assert!(window.lines.is_empty());

        let frame = FileWindowFrame {
            first_line: 17,
            first_column: 17,
            lines: 0,
            columns: 0,
        };
        let window = editor.read_window(&frame);
        assert!(window.lines.is_empty());
    }

    #[test]
    fn read_1x1() {
        test_1x1(0, 0, "\n");
        test_1x1(1, 0, "a");
        test_1x1(1, 1, "a\n");
        test_1x1(1, 2, "a\n");
        test_1x1(1, TAB_SIZE, "");
        test_1x1(4, 0, "d");
        test_1x1(4, 1, "dd");
        test_1x1(4, TAB_SIZE, "\n");
        test_1x1(5, TAB_SIZE, "e");
        test_1x1(5, 2 * TAB_SIZE, "\n");

        fn test_1x1(first_line: usize, first_column: usize, expected_line: &str) {
            let file = concat!(
                "\n",
                "a\n",
                "bb\n",
                "cccc\n",
                "dddddddd\n",
                "eeeeeeeeeeeeeeee\n",
                "ffffffffffffffffffffffffffffffff\n",
                "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            let mut editor = BigFileEditor::from_str(file);

            let frame = FileWindowFrame {
                first_line,
                first_column,
                lines: 1,
                columns: 1,
            };

            let window = editor.read_window(&frame);
            assert_eq!(
                window.frame,
                FileWindowFrame {
                    first_column: frame.first_column - (frame.first_column % TAB_SIZE),
                    ..frame
                }
            );
            assert_eq!(window.lines.len(), 1);
            assert_eq!(window.lines[0], expected_line.to_string());
        }
    }

    fn test_complex_generic(
        file: &str,
        (first_line, first_column): (usize, usize),
        (lines, columns): (usize, usize),
        expected_lines: &[&str],
    ) {
        assert_eq!(lines, expected_lines.len());

        let mut editor = BigFileEditor::from_str(file);

        let frame = FileWindowFrame {
            first_line,
            first_column,
            lines,
            columns,
        };

        let window = editor.read_window(&frame);
        assert_eq!(
            window.frame,
            FileWindowFrame {
                first_column: frame.first_column - (frame.first_column % TAB_SIZE),
                ..frame
            }
        );
        assert_eq!(window.lines.len(), lines);
        for (line, expected_line) in window.lines.iter().zip(expected_lines.iter()) {
            assert_eq!(line, expected_line);
        }
    }

    #[test]
    fn aligned_tabs() {
        test_complex((0, 0), (4, 1), &["\t", "\t", "\t", "\t"]);
        test_complex((0, 0), (4, 2), &["\t", "\t", "\t", "\t"]);
        test_complex((0, 0), (4, 8), &["\t", "\t", "\t", "\t"]);
        test_complex((0, 0), (4, 9), &["\t\n", "\ta", "\tb", "\tc"]);
        test_complex((0, 1), (4, 9), &["\t\n", "\ta\n", "\tbb", "\tcc"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\t\n",
                "\ta\n",
                "\tbb\n",
                "\tcccc\n",
                "\tdddddddd\n",
                "\teeeeeeeeeeeeeeee\n",
                "\tffffffffffffffffffffffffffffffff\n",
                "\tgggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn non_aligned_tabs() {
        test_complex((0, 0), (4, 1), &["\t", "a", "b", "c"]);
        test_complex((0, 0), (4, 2), &["\t", "a\t", "bb", "cc"]);
        test_complex((0, 0), (4, 8), &["\t", "a\t", "bb\t", "ccc\t"]);
        test_complex((0, 0), (4, 9), &["\t\n", "a\t\n", "bb\t\n", "ccc\tc"]);
        test_complex((0, 1), (4, 9), &["\t\n", "a\t\n", "bb\t\n", "ccc\tc\n"]);
        test_complex(
            (4, 0),
            (4, 8),
            &["dddd\t", "eeeee\t", "ffffff\t", "ggggggg\t"],
        );
        test_complex(
            (4, 0),
            (4, 9),
            &["dddd\td", "eeeee\te", "ffffff\tf", "ggggggg\tg"],
        );

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\t\n",
                "a\t\n",
                "bb\t\n",
                "ccc\tc\n",
                "dddd\tdddd\n",
                "eeeee\teeeeeeeeeee\n",
                "ffffff\tffffffffffffffffffffffffff\n",
                "ggggggg\tggggggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn far_aligned_tabs() {
        test_complex((4, 10), (4, 8), &["\n", "\tee", "\tff", "\tgg"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\n",
                "a\n",
                "bb\n",
                "cccc\n",
                "dddddddd\n",
                "eeeeeeee\teeeeeeee\n",
                "ffffffff\tffffffffffffffffffffffff\n",
                "gggggggg\tgggggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn far_non_aligned_tabs() {
        test_complex((4, 10), (4, 8), &["\n", "eeee\tee", "fff\tff", "gg\tgg"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\n",
                "a\n",
                "bb\n",
                "cccc\n",
                "dddddddd\n",
                "eeeeeeeeeeee\teeee\n",
                "fffffffffff\tfffffffffffffffffffff\n",
                "gggggggggg\tgggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn control_chars() {
        test_complex(
            (0, 0),
            (1, 10),
            &["\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09"],
        );
        test_complex(
            (0, 0),
            (1, 16),
            &["\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09"],
        );
        test_complex(
            (0, 0),
            (1, 17),
            &["\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A"],
        );
        test_complex(
            (0, 0),
            (1, 18),
            &["\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A"],
        );
        test_complex((1, 0), (1, 2), &["\x0B\x0C"]);
        test_complex((1, 0), (1, 3), &["\x0B\x0C\x0D"]);
        test_complex((1, 0), (1, 4), &["\x0B\x0C\x0D"]);
        test_complex(
            (2, 0),
            (1, 17),
            &["\x0E\x0F\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E"],
        );
        test_complex(
            (2, 0),
            (1, 18),
            &["\x0E\x0F\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E\x1F"],
        );
        test_complex(
            (2, 0),
            (1, 19),
            &["\x0E\x0F\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E\x1F"],
        );

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                // Special characters are \x09 (\t), \x0A (\n), and \x0D (\r)
                // Both the \x0A and the \x0D should be treated as a new line.
                // The rest of the characters are garbage and have a width of 1.
                "\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A",
                "\x0B\x0C\x0D",
                "\x0E\x0F\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1A\x1B\x1C\x1D\x1E\x1F",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn newlines_lf() {
        test_complex((0, 0), (1, 1), &["\n"]);
        test_complex((0, 0), (1, 2), &["\n"]);
        test_complex((0, 0), (2, 1), &["\n", "\n"]);
        test_complex((0, 0), (2, 2), &["\n", "\n"]);
        test_complex((2, 0), (2, 42), &["\n", "\n"]);
        test_complex((0, 0), (4, 42), &["\n", "\n", "\n", "\n"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!("\n", "\n", "\n", "\n",);

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn newlines_crlf() {
        test_complex((0, 0), (1, 1), &["\r\n"]);
        test_complex((0, 0), (1, 2), &["\r\n"]);
        test_complex((0, 0), (2, 1), &["\r\n", "\r\n"]);
        test_complex((0, 0), (2, 2), &["\r\n", "\r\n"]);
        test_complex((2, 0), (2, 42), &["\r\n", "\r\n"]);
        test_complex((0, 0), (4, 42), &["\r\n", "\r\n", "\r\n", "\r\n"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!("\r\n", "\r\n", "\r\n", "\r\n",);

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn newlines_cr() {
        test_complex((0, 0), (1, 1), &["\r"]);
        test_complex((0, 0), (1, 2), &["\r"]);
        test_complex((0, 0), (2, 1), &["\r", "\r"]);
        test_complex((0, 0), (2, 2), &["\r", "\r"]);
        test_complex((2, 0), (2, 42), &["\r", "\r"]);
        test_complex((0, 0), (4, 42), &["\r", "\r", "\r", "\r"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!("\r", "\r", "\r", "\r",);

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn newlines_all_mixed() {
        test_complex((0, 0), (1, 1), &["\n"]);
        test_complex((0, 0), (1, 2), &["\n"]);
        test_complex((0, 0), (2, 1), &["\n", "\r\n"]);
        test_complex((0, 0), (2, 2), &["\n", "\r\n"]);
        test_complex((2, 0), (2, 42), &["\r", "\r\n"]);
        test_complex((0, 0), (4, 42), &["\n", "\r\n", "\r", "\r\n"]);
        test_complex((0, 0), (5, 42), &["\n", "\r\n", "\r", "\r\n", "\n"]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!("\n", "\r\n", "\r", "\r\n", "\n");

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn read_complex() {
        test_complex((0, 0), (4, 1), &["\n", "a", "b", "c"]);
        test_complex((0, 0), (4, 2), &["\n", "a\n", "bb", "cc"]);
        test_complex((0, 0), (4, 4), &["\n", "a\n", "bb\n", "cccc"]);
        test_complex((2, 0), (4, 4), &["bb\n", "cccc", "dddd", "eeee"]);
        test_complex(
            (2, 2),
            (4, 8),
            &["bb\n", "cccc\n", "dddddddd\n", "eeeeeeeeee"],
        );
        test_complex((6, 0), (4, 4), &["ffff", "gggg", "", ""]);
        test_complex((6, 64), (4, 4), &["", "\n", "", ""]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\n",
                "a\n",
                "bb\n",
                "cccc\n",
                "dddddddd\n",
                "eeeeeeeeeeeeeeee\n",
                "ffffffffffffffffffffffffffffffff\n",
                "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }

    #[test]
    fn read_complex_crlf() {
        test_complex((0, 0), (4, 1), &["\r\n", "a", "b", "c"]);
        test_complex((0, 0), (4, 2), &["\r\n", "a\r\n", "bb", "cc"]);
        test_complex((0, 0), (4, 4), &["\r\n", "a\r\n", "bb\r\n", "cccc"]);
        test_complex((2, 0), (4, 4), &["bb\r\n", "cccc", "dddd", "eeee"]);
        test_complex(
            (2, 2),
            (4, 8),
            &["bb\r\n", "cccc\r\n", "dddddddd\r\n", "eeeeeeeeee"],
        );
        test_complex(
            (2, 7),
            (4, 8),
            &["bb\r\n", "cccc\r\n", "dddddddd\r\n", "eeeeeeeeeeeeeee"],
        );
        test_complex((2, 8), (4, 8), &["", "", "\r\n", "eeeeeeee"]);
        test_complex((6, 0), (4, 4), &["ffff", "gggg", "", ""]);
        test_complex((6, 64), (4, 4), &["", "\r\n", "", ""]);

        fn test_complex(
            (first_line, first_column): (usize, usize),
            (lines, columns): (usize, usize),
            expected_lines: &[&str],
        ) {
            let file = concat!(
                "\r\n",
                "a\r\n",
                "bb\r\n",
                "cccc\r\n",
                "dddddddd\r\n",
                "eeeeeeeeeeeeeeee\r\n",
                "ffffffffffffffffffffffffffffffff\r\n",
                "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg\r\n",
            );

            test_complex_generic(
                file,
                (first_line, first_column),
                (lines, columns),
                expected_lines,
            );
        }
    }
}
