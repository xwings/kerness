//! Where a session's output goes while it is running.
//!
//! The transcript in [`SessionResult`](crate::conversation::Message) is what a
//! caller reads *after* a run. A channel is what they watch *during* one, which
//! is a different job with a different failure mode: a channel that fails must
//! not take the run down with it.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::logging;
use crate::pyfmt::json_dumps;

/// A destination for what a session says as it says it.
pub trait Channel: Send + Sync {
    /// Deliver *message* attributed to *sender*.
    fn send(&self, sender: &str, message: &str) -> Result<()>;

    /// Deliver a framework notice that no agent authored.
    fn send_system(&self, message: &str) -> Result<()>;

    /// This channel's type, named for a diagnostic.
    ///
    /// Required rather than defaulted: the only reader is
    /// [`MultiChannel`]'s failure log, and a placeholder there would name
    /// nothing the caller could go and fix.
    fn type_name(&self) -> String;
}

/// Prints to stdout.
pub struct ConsoleChannel {
    prefix_format: String,
}

impl Default for ConsoleChannel {
    fn default() -> Self {
        ConsoleChannel::new("[{sender}]")
    }
}

impl ConsoleChannel {
    /// *prefix_format* is a template whose `{sender}` is replaced per message.
    pub fn new(prefix_format: impl Into<String>) -> Self {
        ConsoleChannel {
            prefix_format: prefix_format.into(),
        }
    }
}

impl Channel for ConsoleChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        println!(
            "{} {message}",
            self.prefix_format.replace("{sender}", sender)
        );
        Ok(())
    }

    fn send_system(&self, message: &str) -> Result<()> {
        println!("[System] {message}");
        Ok(())
    }

    fn type_name(&self) -> String {
        "ConsoleChannel".to_string()
    }
}

/// Fans out to several channels at once.
///
/// One channel's failure does not stop the others. Channels are typically
/// mixed local and remote — a console plus a log file plus whatever chat
/// transport the caller wrote — and a network blip on the remote one must not
/// cost the session its local transcript, nor abort the run partway through a
/// turn. The failure is logged, naming the channel, because silently swallowing
/// it would make the missing output look like the session never produced it.
pub struct MultiChannel {
    channels: Vec<Arc<dyn Channel>>,
}

impl MultiChannel {
    pub fn new(channels: Vec<Arc<dyn Channel>>) -> Self {
        MultiChannel { channels }
    }

    fn fan_out(&self, deliver: impl Fn(&dyn Channel) -> Result<()>) {
        for channel in &self.channels {
            if let Err(err) = deliver(channel.as_ref()) {
                logging::error(&format!(
                    "Channel {} failed to deliver; continuing: {err}",
                    channel.type_name()
                ));
            }
        }
    }
}

impl Channel for MultiChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        self.fan_out(|channel| channel.send(sender, message));
        Ok(())
    }

    fn send_system(&self, message: &str) -> Result<()> {
        self.fan_out(|channel| channel.send_system(message));
        Ok(())
    }

    fn type_name(&self) -> String {
        "MultiChannel".to_string()
    }
}

/// Writes one JSON object per line to a timestamped file.
pub struct LogChannel {
    log_path: PathBuf,
}

impl LogChannel {
    /// Create *log_dir* and claim a `session_<stamp>.jsonl` inside it.
    pub fn new(log_dir: impl AsRef<Path>) -> Result<Self> {
        let log_dir = log_dir.as_ref();
        fs::create_dir_all(log_dir)
            .map_err(|err| Error::Io(format!("{}: {err}", log_dir.display())))?;
        let stamp = Utc::now().compact();
        Ok(LogChannel {
            log_path: log_dir.join(format!("session_{stamp}.jsonl")),
        })
    }

    /// The file this channel appends to.
    pub fn path(&self) -> &Path {
        &self.log_path
    }

    fn write_event(&self, role: &str, sender: &str, content: &str) -> Result<()> {
        let mut payload = Map::new();
        payload.insert("role".into(), Value::String(role.into()));
        payload.insert("sender".into(), Value::String(sender.into()));
        payload.insert("content".into(), Value::String(content.into()));
        payload.insert("ts".into(), Value::String(Utc::now().iso8601()));
        append_line(&self.log_path, &json_dumps(&Value::Object(payload)))
    }
}

impl Channel for LogChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        self.write_event("assistant", sender, message)
    }

    fn send_system(&self, message: &str) -> Result<()> {
        self.write_event("system", "system", message)
    }

    fn type_name(&self) -> String {
        "LogChannel".to_string()
    }
}

/// Appends plain text to a file.
pub struct FileChannel {
    filepath: PathBuf,
}

impl FileChannel {
    pub fn new(filepath: impl Into<PathBuf>) -> Self {
        FileChannel {
            filepath: filepath.into(),
        }
    }
}

impl Channel for FileChannel {
    fn send(&self, sender: &str, message: &str) -> Result<()> {
        append_line(&self.filepath, &format!("[{sender}] {message}"))
    }

    fn send_system(&self, message: &str) -> Result<()> {
        append_line(&self.filepath, &format!("[System] {message}"))
    }

    fn type_name(&self) -> String {
        "FileChannel".to_string()
    }
}

/// Append *line* and a newline, creating the file if it is not there yet.
///
/// Append rather than truncate: a file opened for writing per message would
/// end the run holding only the last thing said.
fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| Error::Io(format!("{}: {err}", path.display())))?;
    writeln!(file, "{line}").map_err(|err| Error::Io(format!("{}: {err}", path.display())))
}

/// A UTC instant, to the microsecond.
///
/// Only what the two timestamp formats below need. Pulling in a date-time
/// crate to render two strings would cost the whole crate a dependency for
/// forty lines of arithmetic.
struct Utc {
    seconds: i64,
    microseconds: u32,
}

impl Utc {
    fn now() -> Self {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before 1970");
        Utc {
            seconds: since_epoch.as_secs() as i64,
            microseconds: since_epoch.subsec_micros(),
        }
    }

    /// Split into `(year, month, day, hour, minute, second)`.
    fn parts(&self) -> (i64, u32, u32, u32, u32, u32) {
        let days = self.seconds.div_euclid(86_400);
        let seconds_today = self.seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        (
            year,
            month,
            day,
            (seconds_today / 3600) as u32,
            (seconds_today / 60 % 60) as u32,
            (seconds_today % 60) as u32,
        )
    }

    /// `20260827T134501Z` — a filename, so no separators.
    fn compact(&self) -> String {
        let (year, month, day, hour, minute, second) = self.parts();
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
    }

    /// `2026-08-27T13:45:01.123456+00:00`, matching Python's `isoformat`.
    ///
    /// The fractional part is omitted on a whole second, as Python does.
    fn iso8601(&self) -> String {
        let (year, month, day, hour, minute, second) = self.parts();
        let fraction = if self.microseconds == 0 {
            String::new()
        } else {
            format!(".{:06}", self.microseconds)
        };
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00")
    }
}

/// Days since the Unix epoch to a civil `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, which shifts the year to start in March
/// so that the leap day lands at the end and the month-length pattern becomes
/// a single linear expression.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * march_month + 2) / 5 + 1) as u32;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CaptureChannel {
        messages: Mutex<Vec<String>>,
    }

    impl CaptureChannel {
        fn new() -> Arc<Self> {
            Arc::new(CaptureChannel {
                messages: Mutex::new(Vec::new()),
            })
        }

        fn messages(&self) -> Vec<String> {
            self.messages.lock().expect("capture lock").clone()
        }
    }

    impl Channel for CaptureChannel {
        fn send(&self, sender: &str, message: &str) -> Result<()> {
            self.messages
                .lock()
                .expect("capture lock")
                .push(format!("{sender}: {message}"));
            Ok(())
        }

        fn send_system(&self, message: &str) -> Result<()> {
            self.messages
                .lock()
                .expect("capture lock")
                .push(format!("system: {message}"));
            Ok(())
        }

        fn type_name(&self) -> String {
            "CaptureChannel".to_string()
        }
    }

    struct BrokenChannel;

    impl Channel for BrokenChannel {
        fn send(&self, _sender: &str, _message: &str) -> Result<()> {
            Err(Error::Io("network down".into()))
        }

        fn send_system(&self, _message: &str) -> Result<()> {
            Err(Error::Io("network down".into()))
        }

        fn type_name(&self) -> String {
            "BrokenChannel".to_string()
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-channel-{tag}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_failing_channel_does_not_starve_the_others() {
        let good = CaptureChannel::new();
        let multi = MultiChannel::new(vec![Arc::new(BrokenChannel), good.clone()]);
        multi.send("Alice", "msg").expect("fan-out never fails");
        multi.send_system("sys").expect("fan-out never fails");

        assert_eq!(good.messages(), vec!["Alice: msg", "system: sys"]);
    }

    #[test]
    fn the_file_channel_appends_a_line_per_message() {
        let dir = TempDir::new("file");
        let path = dir.0.join("output.txt");
        let channel = FileChannel::new(&path);
        channel.send("Alice", "first").expect("send");
        channel.send("Bob", "second").expect("send");
        channel.send_system("done").expect("send");

        assert_eq!(
            fs::read_to_string(&path).expect("read back"),
            "[Alice] first\n[Bob] second\n[System] done\n"
        );
    }

    #[test]
    fn the_log_channel_writes_one_typed_event_per_line() {
        let dir = TempDir::new("log");
        let channel = LogChannel::new(&dir.0).expect("create");
        channel.send("Alice", "hello").expect("send");
        channel.send_system("system msg").expect("send");

        let written = fs::read_to_string(channel.path()).expect("read back");
        let lines: Vec<&str> = written.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let first: Value = serde_json::from_str(lines[0]).expect("valid JSON");
        assert_eq!(first["role"], "assistant");
        assert_eq!(first["sender"], "Alice");
        assert_eq!(first["content"], "hello");
        assert!(first.get("ts").is_some(), "every event is timestamped");

        let second: Value = serde_json::from_str(lines[1]).expect("valid JSON");
        assert_eq!(second["role"], "system");
        assert_eq!(second["sender"], "system");
    }

    #[test]
    fn timestamps_render_the_way_python_renders_them() {
        // 2026-08-27T13:45:01.000123Z, as seconds since the epoch.
        let instant = Utc {
            seconds: 1_787_838_301,
            microseconds: 123,
        };
        assert_eq!(instant.compact(), "20260827T134501Z");
        assert_eq!(instant.iso8601(), "2026-08-27T13:45:01.000123+00:00");

        let whole_second = Utc {
            seconds: 1_787_838_301,
            microseconds: 0,
        };
        assert_eq!(whole_second.iso8601(), "2026-08-27T13:45:01+00:00");
    }

    #[test]
    fn leap_days_and_century_boundaries_land_on_the_right_date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29), "a leap year");
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
