//! Offline fixtures shared by the host-control examples.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use kerness::provider::ProviderBase;
use kerness::{Error, Provider, ProviderResponse, ReasoningEffort, Result, ToolSpec};
use serde_json::Value;

/// Only a directory created by this process is removed at shutdown.
pub struct Workspace(PathBuf);

impl Workspace {
    pub fn new(label: &str) -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for attempt in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "kerness-{label}-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Could not create a fresh example workspace",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn write(&self, name: &str, contents: &str) -> io::Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(contents.as_bytes())?;
        Ok(path)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "Could not remove example workspace {}: {error}",
                self.0.display()
            );
        }
    }
}

/// Exhausting the script is an error, so unexpected provider calls are visible.
pub struct Scripted {
    base: ProviderBase,
    replies: Vec<&'static str>,
    calls: AtomicUsize,
}

impl Scripted {
    pub fn new(replies: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            base: ProviderBase::new(0, 0.0, None),
            replies: replies.to_vec(),
            calls: AtomicUsize::new(0),
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for Scripted {
    fn name(&self) -> &str {
        "offline-script"
    }
    fn base(&self) -> &ProviderBase {
        &self.base
    }

    fn chat(
        &self,
        _model: &str,
        _messages: &[Value],
        _tools: Option<&[ToolSpec]>,
        _effort: ReasoningEffort,
    ) -> Result<ProviderResponse> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.replies
            .get(index)
            .map(|reply| ProviderResponse::text(*reply))
            .ok_or_else(|| Error::provider("Offline script exhausted: unexpected provider request"))
    }
}
