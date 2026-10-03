//! Truncating terminal writer for tracing.
//!
//! Tracing events from LLM providers can dump 10KB+ JSON bodies to stderr.
//! Rather than truncating at every call site (fragile, easy to miss), we
//! handle it at the writer level: the fmt layer gets a `TruncatingStderr`
//! that caps each event before flushing, while the web gateway `WebLogLayer`
//! still sees the full, untruncated content.
//!
//! ```text
//! tracing::debug!("body: {huge_json}")
//!        |
//!        v
//!   tracing_subscriber::registry()
//!        |
//!        +-- fmt::layer().with_writer(TruncatingStderr)  <-- scrub, then cap at 500B
//!        |       \-- stderr (journal / daemon.stderr.log)
//!        |
//!        +-- fmt::layer().with_writer(SandboxLogWriter)  <-- scrub, cap at 4 KiB
//!        |       \-- <base>/logs/sandbox/daemon.log (read-only /tenant-logs in the sandbox)
//!        |
//!        \-- WebLogLayer
//!                \-- SSE broadcast (full, scrubbed)
//! ```
//!
//! Every daemon log sink is scrubbed at write time by the same
//! [`daemon_log_scrubber`] (WI#287); before that only the SSE stream was, so
//! raw secrets could land in the journal or `daemon.stderr.log`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use lunarwing_safety::LogScrubber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// The daemon's log scrubber, shared by every log sink (WI#287): default leak
/// shapes, log-specific shapes, and the exact values of secret-named
/// environment variables present when first used (after config load). Secrets
/// fetched later from the secrets store are only caught by shape.
pub fn daemon_log_scrubber() -> Arc<LogScrubber> {
    static SCRUBBER: LazyLock<Arc<LogScrubber>> =
        LazyLock::new(|| Arc::new(LogScrubber::new(&known_secret_env_values())));
    Arc::clone(&SCRUBBER)
}

/// Values of environment variables whose names mark them as secrets.
fn known_secret_env_values() -> Vec<String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| is_secret_env_name(name))
        .map(|(_, value)| value)
        .collect()
}

/// Env var names that carry secrets (`GATEWAY_AUTH_TOKEN`, `XMPP_PASSWORD`,
/// `TENSORZERO_API_KEY`, `SECRETS_MASTER_KEY`, ...).
fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "PRIVATE_KEY",
        "MASTER_KEY",
    ]
    .iter()
    .any(|marker| upper.contains(marker))
}

/// Scrub one formatted event, lossily decoding non-UTF-8 bytes. A record
/// replaced whole keeps the event's trailing newline.
fn scrub_event(scrubber: &LogScrubber, event: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(event);
    let mut scrubbed = scrubber.scrub(&text).into_owned();
    if text.ends_with('\n') && !scrubbed.ends_with('\n') {
        scrubbed.push('\n');
    }
    scrubbed.into_bytes()
}

/// Initialize tracing for simple CLI commands (warn level, no fancy layers).
pub fn init_cli_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();
}

/// Initialize tracing for worker/bridge processes (info level).
pub fn init_worker_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("lunarwing=info")),
        )
        .init();
}

/// Maximum bytes per tracing event written to the terminal.
const TERMINAL_MAX_EVENT_BYTES: usize = 500;

/// A `MakeWriter` that creates per-event buffers which truncate on flush.
///
/// Each call to `make_writer()` returns an `EventBuffer`. All `write()`
/// calls accumulate into the buffer. When the buffer drops (after the fmt
/// layer finishes writing one event), it flushes to stderr, truncating if
/// the total exceeds `TERMINAL_MAX_EVENT_BYTES`.
#[derive(Clone)]
pub struct TruncatingStderr {
    max_bytes: usize,
    scrubber: Option<Arc<LogScrubber>>,
}

impl Default for TruncatingStderr {
    fn default() -> Self {
        Self {
            max_bytes: TERMINAL_MAX_EVENT_BYTES,
            scrubber: None,
        }
    }
}

impl TruncatingStderr {
    /// Writer that scrubs each event before truncating and writing it
    /// (WI#287). Scrubbing first matters: truncation could cut a secret below
    /// the length its pattern needs and let the remainder through.
    pub fn scrubbed(scrubber: Arc<LogScrubber>) -> Self {
        Self {
            scrubber: Some(scrubber),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            scrubber: None,
        }
    }
}

impl<'a> MakeWriter<'a> for TruncatingStderr {
    type Writer = EventBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        EventBuffer {
            buf: Vec::with_capacity(256),
            max_bytes: self.max_bytes,
            scrubber: self.scrubber.clone(),
            #[cfg(test)]
            sink: None,
        }
    }
}

/// Per-event buffer that truncates on drop.
pub struct EventBuffer {
    buf: Vec<u8>,
    max_bytes: usize,
    scrubber: Option<Arc<LogScrubber>>,
    /// Test-only: capture output instead of writing to stderr.
    #[cfg(test)]
    sink: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
}

impl Write for EventBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Find the last valid UTF-8 char boundary at or before `pos` in `bytes`.
///
/// Walks backwards from `pos` until we find a byte that isn't a UTF-8
/// continuation byte (0x80..0xBF). Returns 0 if the entire prefix is
/// somehow invalid (shouldn't happen with valid UTF-8 input from tracing).
fn utf8_floor(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    // UTF-8 continuation bytes have the form 10xxxxxx (0x80..0xBF).
    // Walk backwards past them to find the start of the last character.
    while i > 0 && bytes[i] & 0xC0 == 0x80 {
        i -= 1;
    }
    i
}

impl Drop for EventBuffer {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        if let Some(scrubber) = &self.scrubber {
            self.buf = scrub_event(scrubber, &self.buf);
        }

        let output = if self.buf.len() <= self.max_bytes {
            &self.buf[..]
        } else {
            // Truncate at a UTF-8 safe boundary
            let cut = utf8_floor(&self.buf, self.max_bytes);
            let suffix = format!("...[{}B total]\n", self.buf.len());
            let mut truncated = Vec::with_capacity(cut + suffix.len());
            // Strip trailing newline from the cut portion (we add our own via suffix)
            let cut_slice = &self.buf[..cut];
            let trimmed = if cut_slice.last() == Some(&b'\n') {
                &cut_slice[..cut_slice.len() - 1]
            } else {
                cut_slice
            };
            truncated.extend_from_slice(trimmed);
            truncated.extend_from_slice(suffix.as_bytes());

            #[cfg(test)]
            if let Some(ref sink) = self.sink {
                let mut s = sink.lock().expect("test sink lock poisoned");
                s.extend_from_slice(&truncated);
                return;
            }

            let _ = io::stderr().write_all(&truncated);
            return;
        };

        #[cfg(test)]
        if let Some(ref sink) = self.sink {
            let mut s = sink.lock().expect("test sink lock poisoned");
            s.extend_from_slice(output);
            return;
        }

        let _ = io::stderr().write_all(output);
    }
}

/// Name of the sandbox-visible daemon log file inside its directory.
pub const SANDBOX_LOG_FILE: &str = "daemon.log";

/// Rotation threshold for the sandbox-visible log; one rotated generation
/// (`daemon.log.1`) is kept, so at most about twice this is on disk.
const SANDBOX_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Per-event cap for the sandbox-visible log: roomier than the terminal's
/// 500 B, still bounded against multi-KB request bodies at debug level.
const SANDBOX_LOG_MAX_EVENT_BYTES: usize = 4096;

/// A `MakeWriter` appending every event, secret-scrubbed, to the daemon's
/// sandbox-visible log (WI#287).
///
/// The shell sandbox mounts this file's directory read-only at
/// `/tenant-logs`, so the agent can see what the daemon did without host
/// access. It is the daemon's own copy, written by the process on every init
/// system — the journal on systemd and the raw OpenRC/launchd files stay
/// operator-only — and it holds only scrubbed records, never earlier raw log
/// history or other services' logs.
#[derive(Clone)]
pub struct SandboxLogWriter {
    file: Arc<SandboxLogFile>,
}

struct SandboxLogFile {
    path: PathBuf,
    rotated: PathBuf,
    max_bytes: u64,
    scrubber: Arc<LogScrubber>,
    /// Open handle and bytes written to it so far.
    state: Mutex<(File, u64)>,
}

impl SandboxLogWriter {
    /// Open (creating `dir` 0700 and the file 0600 as needed) the log at
    /// `dir/daemon.log`.
    pub fn open(dir: &Path, scrubber: Arc<LogScrubber>) -> io::Result<Self> {
        Self::open_with_max_bytes(dir, scrubber, SANDBOX_LOG_MAX_BYTES)
    }

    fn open_with_max_bytes(
        dir: &Path,
        scrubber: Arc<LogScrubber>,
        max_bytes: u64,
    ) -> io::Result<Self> {
        create_private_dir(dir)?;
        let path = dir.join(SANDBOX_LOG_FILE);
        let file = open_private_append(&path)?;
        let written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Ok(Self {
            file: Arc::new(SandboxLogFile {
                rotated: dir.join(format!("{SANDBOX_LOG_FILE}.1")),
                path,
                max_bytes,
                scrubber,
                state: Mutex::new((file, written)),
            }),
        })
    }
}

impl SandboxLogFile {
    /// Append one scrubbed, capped event, rotating first when it would push
    /// the file past the threshold. Write errors are dropped: logging must
    /// never fail or recurse into the logger.
    fn append(&self, event: &[u8]) {
        let scrubbed = scrub_event(&self.scrubber, event);
        let record = cap_event(&scrubbed, SANDBOX_LOG_MAX_EVENT_BYTES);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.1 > 0 && state.1 + record.len() as u64 > self.max_bytes {
            let _ = std::fs::rename(&self.path, &self.rotated);
            match open_private_append(&self.path) {
                Ok(file) => *state = (file, 0),
                Err(_) => return,
            }
        }
        if state.0.write_all(&record).is_ok() {
            state.1 += record.len() as u64;
        }
    }
}

impl<'a> MakeWriter<'a> for SandboxLogWriter {
    type Writer = SandboxLogEvent;

    fn make_writer(&'a self) -> Self::Writer {
        SandboxLogEvent {
            buf: Vec::with_capacity(256),
            file: Arc::clone(&self.file),
        }
    }
}

/// Per-event buffer for [`SandboxLogWriter`], appended on drop.
pub struct SandboxLogEvent {
    buf: Vec<u8>,
    file: Arc<SandboxLogFile>,
}

impl Write for SandboxLogEvent {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SandboxLogEvent {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            self.file.append(&self.buf);
        }
    }
}

/// Cap one event at `max` bytes on a UTF-8 boundary, marking the cut.
fn cap_event(event: &[u8], max: usize) -> Vec<u8> {
    if event.len() <= max {
        return event.to_vec();
    }
    let cut = utf8_floor(event, max);
    let mut capped = event[..cut].to_vec();
    capped.extend_from_slice(format!("...[{}B total]\n", event.len()).as_bytes());
    capped
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::tracing_fmt::{
        EventBuffer, SANDBOX_LOG_FILE, SandboxLogWriter, TruncatingStderr, is_secret_env_name,
        utf8_floor,
    };
    use lunarwing_safety::{LogScrubber, REDACTED_LOG_RECORD};
    use tracing_subscriber::fmt::MakeWriter;

    use std::io::Write;

    /// Helper: create an EventBuffer that captures output to a shared Vec
    /// instead of writing to stderr.
    fn test_buffer(max_bytes: usize) -> (EventBuffer, Arc<Mutex<Vec<u8>>>) {
        test_buffer_with_scrubber(max_bytes, None)
    }

    fn test_buffer_with_scrubber(
        max_bytes: usize,
        scrubber: Option<Arc<LogScrubber>>,
    ) -> (EventBuffer, Arc<Mutex<Vec<u8>>>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let buf = EventBuffer {
            buf: Vec::new(),
            max_bytes,
            scrubber,
            sink: Some(Arc::clone(&sink)),
        };
        (buf, sink)
    }

    const TEST_SECRET: &str = "7f3e9a1c5b2d8e4f6a0c9b3d7e1f5a2c";

    fn test_scrubber() -> Arc<LogScrubber> {
        Arc::new(LogScrubber::new(&[TEST_SECRET]))
    }

    /// WI#287: stderr (journal / daemon.stderr.log) is scrubbed, and before
    /// truncation, so a cut can never leave a secret prefix behind.
    #[test]
    fn test_stderr_event_is_scrubbed_before_truncation() {
        let (mut buf, sink) = test_buffer_with_scrubber(48, Some(test_scrubber()));
        let event = format!("auth ok token={TEST_SECRET} for user alice and more trailing text\n");
        buf.write_all(event.as_bytes()).unwrap();
        drop(buf);

        let output = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert!(!output.contains(&TEST_SECRET[..12]), "{output}");
        assert!(output.contains("[REDACTED]"), "{output}");
    }

    #[test]
    fn test_blocked_stderr_record_keeps_its_newline() {
        let (mut buf, sink) = test_buffer_with_scrubber(500, Some(test_scrubber()));
        buf.write_all(b"key AKIAIOSFODNN7EXAMPLE leaked\n").unwrap();
        drop(buf);

        let output = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert_eq!(output, format!("{REDACTED_LOG_RECORD}\n"));
    }

    #[test]
    fn test_secret_env_names() {
        for name in [
            "GATEWAY_AUTH_TOKEN",
            "XMPP_PASSWORD",
            "TENSORZERO_API_KEY",
            "SECRETS_MASTER_KEY",
            "webhook_secret",
        ] {
            assert!(is_secret_env_name(name), "{name}");
        }
        for name in ["RUST_LOG", "LUNARWING_BASE_DIR", "SANDBOX_PROTECTED_PATHS"] {
            assert!(!is_secret_env_name(name), "{name}");
        }
    }

    fn write_event(writer: &SandboxLogWriter, event: &str) {
        let mut handle = writer.make_writer();
        handle.write_all(event.as_bytes()).unwrap();
    }

    /// WI#287: the sandbox-visible log holds scrubbed records only, in a
    /// private directory and file.
    #[test]
    fn test_sandbox_log_writer_writes_scrubbed_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("logs").join("sandbox");
        let writer = SandboxLogWriter::open(&dir, test_scrubber()).unwrap();
        write_event(&writer, "INFO memory_write ok path=MEMORY.md\n");
        write_event(&writer, &format!("DEBUG header {TEST_SECRET}\n"));
        write_event(&writer, "WARN key AKIAIOSFODNN7EXAMPLE seen\n");

        let content = std::fs::read_to_string(dir.join(SANDBOX_LOG_FILE)).unwrap();
        assert!(
            content.contains("memory_write ok path=MEMORY.md"),
            "{content}"
        );
        assert!(!content.contains(TEST_SECRET), "{content}");
        assert!(!content.contains("AKIAIOSFODNN7EXAMPLE"), "{content}");
        assert!(content.contains(REDACTED_LOG_RECORD), "{content}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            let file_mode = std::fs::metadata(dir.join(SANDBOX_LOG_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }
    }

    #[test]
    fn test_sandbox_log_writer_rotates_and_caps_events() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("sandbox");
        let writer = SandboxLogWriter::open_with_max_bytes(&dir, test_scrubber(), 64).unwrap();
        write_event(&writer, "first event padded to forty bytes.....\n");
        write_event(&writer, "second event pushes past the threshold\n");

        let current = std::fs::read_to_string(dir.join(SANDBOX_LOG_FILE)).unwrap();
        let rotated = std::fs::read_to_string(dir.join(format!("{SANDBOX_LOG_FILE}.1"))).unwrap();
        assert!(current.starts_with("second event"), "{current}");
        assert!(rotated.starts_with("first event"), "{rotated}");

        let huge = format!("{}\n", "x".repeat(10_000));
        write_event(&writer, &huge);
        let current = std::fs::read_to_string(dir.join(SANDBOX_LOG_FILE)).unwrap();
        assert!(
            current.len() < 5_000,
            "event must be capped: {}",
            current.len()
        );
        assert!(current.contains("[10001B total]"), "{current}");
    }

    #[test]
    fn test_short_event_not_truncated() {
        let (mut buf, sink) = test_buffer(500);
        buf.write_all(b"hello world\n").unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        assert_eq!(&*output, b"hello world\n");
    }

    #[test]
    fn test_long_event_truncated() {
        let (mut buf, sink) = test_buffer(20);
        let data = "abcdefghijklmnopqrstuvwxyz0123456789\n";
        buf.write_all(data.as_bytes()).unwrap();
        let total = data.len();
        drop(buf);

        let output = sink.lock().unwrap();
        let output_str = String::from_utf8_lossy(&output);
        // Should contain the suffix with total byte count
        assert!(
            output_str.contains(&format!("...[{}B total]", total)),
            "expected truncation suffix, got: {}",
            output_str
        );
        // Should be shorter than the original
        assert!(output.len() < total);
    }

    #[test]
    fn test_utf8_boundary_safe() {
        // "Helloé" = [72, 101, 108, 108, 111, 195, 169]
        //                                         ^-- 2-byte UTF-8 char
        // If we truncate at 6 bytes, we'd land in the middle of 'é'.
        // utf8_floor should back up to byte 5 (start of 'é' = 195).
        let (mut buf, sink) = test_buffer(6);
        let data = "Helloé world";
        buf.write_all(data.as_bytes()).unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        let output_str = String::from_utf8(output.clone());
        assert!(
            output_str.is_ok(),
            "output should be valid UTF-8, got bytes: {:?}",
            output
        );
        let s = output_str.unwrap();
        assert!(
            s.contains("...["),
            "should be truncated with suffix, got: {}",
            s
        );
        // The truncated prefix must be valid UTF-8 up to the cut point.
        // "Hello" (5 bytes) is the last valid cut before the 2-byte é.
        assert!(
            s.starts_with("Hello"),
            "should start with 'Hello', got: {}",
            s
        );
    }

    #[test]
    fn test_utf8_floor_basic() {
        // ASCII: every byte is a valid boundary
        assert_eq!(utf8_floor(b"hello", 3), 3);

        // 2-byte UTF-8 char é = [0xC3, 0xA9]
        // Landing on the continuation byte (0xA9) should back up to 0xC3
        let bytes = "Hé".as_bytes(); // [72, 0xC3, 0xA9]
        assert_eq!(utf8_floor(bytes, 2), 1); // backs up to start of é

        // 3-byte UTF-8 char (e.g. あ = [0xE3, 0x81, 0x82])
        let bytes = "aあ".as_bytes(); // [97, 0xE3, 0x81, 0x82]
        assert_eq!(utf8_floor(bytes, 2), 1); // backs up past continuation to 0xE3
        assert_eq!(utf8_floor(bytes, 3), 1); // same: 0x82 is continuation, 0x81 is too
    }

    #[test]
    fn test_multiple_writes_accumulated() {
        let (mut buf, sink) = test_buffer(500);
        buf.write_all(b"hello ").unwrap();
        buf.write_all(b"world\n").unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        assert_eq!(&*output, b"hello world\n");
    }

    #[test]
    fn test_empty_buffer_no_output() {
        let (_buf, sink) = test_buffer(500);
        // drop without writing
        drop(_buf);

        let output = sink.lock().unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn test_default_max_bytes() {
        let writer = TruncatingStderr::default();
        assert_eq!(writer.max_bytes, 500);
    }

    #[test]
    fn test_custom_max_bytes() {
        let writer = TruncatingStderr::with_max_bytes(100);
        assert_eq!(writer.max_bytes, 100);
    }

    #[test]
    fn test_exactly_at_limit_not_truncated() {
        let (mut buf, sink) = test_buffer(5);
        buf.write_all(b"hello").unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        assert_eq!(&*output, b"hello");
    }

    #[test]
    fn test_one_over_limit_truncated() {
        let (mut buf, sink) = test_buffer(5);
        buf.write_all(b"hello!").unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        let s = String::from_utf8_lossy(&output);
        assert!(s.contains("...[6B total]"), "got: {}", s);
    }

    #[test]
    fn test_4byte_utf8_boundary() {
        // 4-byte UTF-8 char: 𝄞 (musical symbol) = [0xF0, 0x9D, 0x84, 0x9E]
        let data = "AB𝄞CD";
        // bytes: [65, 66, 0xF0, 0x9D, 0x84, 0x9E, 67, 68]
        // Truncating at byte 4 lands in the middle of the 4-byte char
        let (mut buf, sink) = test_buffer(4);
        buf.write_all(data.as_bytes()).unwrap();
        drop(buf);

        let output = sink.lock().unwrap();
        let s = String::from_utf8(output.clone());
        assert!(s.is_ok(), "output must be valid UTF-8, got: {:?}", output);
        let s = s.unwrap();
        // Should back up to byte 2 (just "AB"), since bytes 2..5 are all part of 𝄞
        assert!(s.starts_with("AB"), "expected 'AB', got: {}", s);
        assert!(s.contains("...["), "should be truncated, got: {}", s);
    }
}
