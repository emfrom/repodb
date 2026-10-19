use crate::Result;
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
    cache: HashMap<String, Vec<u8>>,
    cached_bytes: usize,
    finished: bool,
}

impl GitBlobReader {
    pub fn new(repository: Option<&str>) -> Result<Self> {
        let mut command = Command::new("git");
        if let Some(repository) = repository {
            command.args(["-C", repository]);
        }
        let mut child = command
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
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
            cache: HashMap::new(),
            cached_bytes: 0,
            finished: false,
        })
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
                return Err(
                    format!("Git blob {id} is too large to hydrate safely ({size} bytes)").into(),
                );
            }
            let mut bytes = vec![0; size];
            self.stdout.read_exact(&mut bytes)?;
            let mut delimiter = [0; 1];
            self.stdout.read_exact(&mut delimiter)?;
            if delimiter[0] != b'\n' {
                return Err(format!("malformed git batch payload for {id}").into());
            }
            if size <= MAX_CACHED_BLOB_BYTES {
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
                return Ok(bytes);
            }
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
    use super::{GitBlobReader, MAX_STDERR_EXCERPT_BYTES};
    use std::{fs, process::Command};

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
        let payload = [b'\0', b'\n', b'\xff', b'x'];
        fs::write(root.path().join("binary"), payload).unwrap();
        git(root.path(), &["add", "binary"]);
        let id = git(root.path(), &["hash-object", "binary"]);

        let mut reader = GitBlobReader::new(Some(root.path().to_str().unwrap())).unwrap();
        assert_eq!(reader.blob(&id).unwrap(), payload);
        assert_eq!(reader.blob(&id).unwrap(), payload);
    }

    #[test]
    fn stderr_drainer_retains_only_a_bounded_excerpt_while_draining() {
        let input = vec![b'x'; MAX_STDERR_EXCERPT_BYTES * 128];
        let excerpt = super::drain_stderr(input.as_slice());

        assert_eq!(excerpt.len(), MAX_STDERR_EXCERPT_BYTES);
        assert_eq!(excerpt, vec![b'x'; MAX_STDERR_EXCERPT_BYTES]);
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
