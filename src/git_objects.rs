use crate::{
    Result,
    process::{self, ProcessDeadline},
};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
};

const MAX_CACHED_BLOB_BYTES: usize = 32 * 1024 * 1024;
const MAX_HYDRATED_BLOB_BYTES: usize = 256 * 1024 * 1024;
const MAX_STDERR_EXCERPT_BYTES: usize = 8 * 1024;

fn drain_stderr(mut stderr: impl Read) -> Vec<u8> {
    let mut excerpt = Vec::with_capacity(MAX_STDERR_EXCERPT_BYTES);
    let mut buffer = [0; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let retained = (MAX_STDERR_EXCERPT_BYTES - excerpt.len()).min(count);
                excerpt.extend_from_slice(&buffer[..retained]);
            }
        }
    }
    excerpt
}

/// A query-scoped persistent `git cat-file --batch` reader. Blob retention is
/// bounded; a blob larger than the cache limit is streamed and not retained.
pub struct GitBlobReader {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<thread::JoinHandle<Vec<u8>>>,
    deadline: Option<ProcessDeadline>,
    cache: HashMap<String, Vec<u8>>,
    cached_bytes: usize,
    large_blob: Option<(String, Vec<u8>)>,
    finished: bool,
}

impl GitBlobReader {
    pub fn new(repository: Option<&str>) -> Result<Self> {
        let mut command = Command::new("git");
        if let Some(repository) = repository {
            command.args(["-C", repository]);
        }
        command
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = process::spawn_in_process_group(&mut command)?;
        let deadline = ProcessDeadline::start(child.id(), process::refresh_subprocess_timeout()?);
        let stdin = child.stdin.take().ok_or("missing git batch stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("missing git batch stdout")?);
        let stderr = child
            .stderr
            .take()
            .map(|stderr| thread::spawn(move || drain_stderr(stderr)));
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr,
            deadline: Some(deadline),
            cache: HashMap::new(),
            cached_bytes: 0,
            large_blob: None,
            finished: false,
        })
    }

    fn terminate(&mut self) {
        drop(self.stdin.take());
        let process_id = self.child.id();
        process::terminate_process_group(&mut self.child, process_id);
        let _ = self.child.wait();
        if let Some(deadline) = self.deadline.take() {
            let _ = deadline.finish();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
        if let Some(deadline) = self.deadline.take() {
            let _ = deadline.finish();
        }
        self.finished = true;
    }

    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        drop(self.stdin.take());
        let status = self.child.wait()?;
        let stderr = self
            .stderr
            .take()
            .map(|reader| reader.join().unwrap_or_default())
            .unwrap_or_default();
        self.finished = true;
        let timed_out = self.deadline.take().is_some_and(ProcessDeadline::finish);
        if timed_out {
            return Err("process timed out".into());
        }
        if !status.success() {
            return Err(format!(
                "git cat-file --batch failed: {}",
                String::from_utf8_lossy(&stderr)
            )
            .into());
        }
        Ok(())
    }

    pub fn blob(&mut self, id: &str) -> Result<Vec<u8>> {
        if let Some((cached_id, bytes)) = &self.large_blob
            && cached_id == id
        {
            return Ok(bytes.clone());
        }
        if !self.cache.contains_key(id) {
            let stdin = self.stdin.as_mut().ok_or("git batch reader is closed")?;
            writeln!(stdin, "{id}")?;
            stdin.flush()?;
            let mut header = Vec::new();
            self.stdout.read_until(b'\n', &mut header)?;
            let header = std::str::from_utf8(&header).map_err(|_| "malformed git batch header")?;
            let fields = header.trim_end().split(' ').collect::<Vec<_>>();
            if fields.len() == 2 && fields[1] == "missing" {
                return Err(format!("cannot hydrate Git blob {id}: object is missing").into());
            }
            if fields.len() != 3 || fields[1] != "blob" || fields[0] != id {
                return Err(format!("malformed git batch response for {id}").into());
            }
            let size: usize = fields[2]
                .parse()
                .map_err(|_| "invalid git batch object size")?;
            if size > MAX_HYDRATED_BLOB_BYTES {
                let error = format!("Git blob {id} is too large to hydrate safely ({size} bytes)");
                self.terminate();
                return Err(error.into());
            }
            let mut bytes = vec![0; size];
            self.stdout.read_exact(&mut bytes)?;
            let mut delimiter = [0; 1];
            self.stdout.read_exact(&mut delimiter)?;
            if delimiter[0] != b'\n' {
                return Err(format!("malformed git batch payload for {id}").into());
            }
            if size <= MAX_CACHED_BLOB_BYTES {
                self.large_blob = None;
                while self.cached_bytes.saturating_add(size) > MAX_CACHED_BLOB_BYTES {
                    let Some(key) = self.cache.keys().next().cloned() else {
                        break;
                    };
                    if let Some(removed) = self.cache.remove(&key) {
                        self.cached_bytes -= removed.len();
                    }
                }
                self.cached_bytes += size;
                self.cache.insert(id.to_owned(), bytes);
            } else {
                self.cache.clear();
                self.cached_bytes = 0;
                self.large_blob = Some((id.to_owned(), bytes));
            }
        }
        if let Some((_, bytes)) = &self.large_blob {
            return Ok(bytes.clone());
        }
        self.cache
            .get(id)
            .cloned()
            .ok_or_else(|| "missing hydrated Git blob".into())
    }
}

impl Drop for GitBlobReader {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.stdin.take());
            let _ = self.child.wait();
            if let Some(stderr) = self.stderr.take() {
                let _ = stderr.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GitBlobReader, MAX_STDERR_EXCERPT_BYTES, ProcessDeadline, drain_stderr, thread};
    use std::{
        collections::HashMap,
        fs,
        io::BufReader,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn batch_reader_reuses_blob_and_preserves_binary_bytes() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        let payload = *b"\0\n\xffx";
        fs::write(root.path().join("binary"), payload).unwrap();
        git(root.path(), &["add", "binary"]);
        let id = git(root.path(), &["hash-object", "binary"]);

        let mut reader = GitBlobReader::new(Some(root.path().to_str().unwrap())).unwrap();
        assert_eq!(reader.blob(&id).unwrap(), payload);
        assert_eq!(reader.blob(&id).unwrap(), payload);
    }

    #[test]
    fn batch_reader_reuses_large_blob_across_many_units_with_bounded_retention() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        let payload = vec![b'x'; super::MAX_CACHED_BLOB_BYTES + 1];
        fs::write(root.path().join("large"), &payload).unwrap();
        git(root.path(), &["add", "large"]);
        let id = git(root.path(), &["hash-object", "large"]);

        let mut reader = GitBlobReader::new(Some(root.path().to_str().unwrap())).unwrap();
        for unit in 0..64 {
            let blob = reader.blob(&id).unwrap();
            let start = unit * 1024;
            assert_eq!(&blob[start..start + 1024], &payload[start..start + 1024]);
        }
        assert_eq!(reader.cached_bytes, 0);
        assert_eq!(
            reader.large_blob.as_ref().map(|(blob, _)| blob.as_str()),
            Some(id.as_str())
        );
        assert_eq!(reader.large_blob.as_ref().unwrap().1.len(), payload.len());
    }

    #[cfg(unix)]
    #[test]
    fn persistent_reader_timeout_kills_the_process_group() {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 5")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let deadline = ProcessDeadline::start(child.id(), std::time::Duration::from_millis(50));
        let stderr = child
            .stderr
            .take()
            .map(|stderr| thread::spawn(move || drain_stderr(stderr)));
        let mut reader = GitBlobReader {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            stderr,
            deadline: Some(deadline),
            cache: HashMap::new(),
            cached_bytes: 0,
            large_blob: None,
            finished: false,
        };

        let started = std::time::Instant::now();
        let error = reader.finish().unwrap_err().to_string();
        assert!(error.contains("process timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn stderr_drainer_retains_only_a_bounded_excerpt_while_draining() {
        let input = vec![b'x'; MAX_STDERR_EXCERPT_BYTES * 128];
        let excerpt = super::drain_stderr(input.as_slice());

        assert_eq!(excerpt.len(), MAX_STDERR_EXCERPT_BYTES);
        assert_eq!(excerpt, vec![b'x'; MAX_STDERR_EXCERPT_BYTES]);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_batch_response_kills_child_before_waiting_for_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fake_git = bin.join("git");
        fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nprintf 'oversized blob {}\\n'\nexec yes x\n",
                super::MAX_HYDRATED_BLOB_BYTES + 1
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(&fake_git)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let stderr = child
            .stderr
            .take()
            .map(|stderr| std::thread::spawn(move || super::drain_stderr(stderr)));
        let mut reader = GitBlobReader {
            child,
            stdin,
            stdout,
            stderr,
            deadline: None,
            cache: std::collections::HashMap::new(),
            cached_bytes: 0,
            large_blob: None,
            finished: false,
        };
        let started = Instant::now();
        let error = reader.blob("oversized").unwrap_err().to_string();
        assert!(error.contains("too large to hydrate safely"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            reader.child.try_wait().unwrap().is_some(),
            "child was not reaped"
        );
        assert!(reader.finished);
    }

    #[test]
    fn batch_reader_reports_missing_indexed_objects() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        let mut reader = GitBlobReader::new(Some(root.path().to_str().unwrap())).unwrap();
        assert!(
            reader
                .blob("0000000000000000000000000000000000000000")
                .is_err()
        );
    }
}
