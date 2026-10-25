#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::{
    io::{Read, Write},
    process::{Child, Command, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

const DEFAULT_REFRESH_SUBPROCESS_TIMEOUT_SECS: u64 = 300;
const MAX_REFRESH_SUBPROCESS_TIMEOUT_SECS: u64 = 3600;

pub(crate) fn refresh_subprocess_timeout() -> crate::Result<std::time::Duration> {
    let seconds = match std::env::var("REPODB_REFRESH_TIMEOUT_SECS") {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "REPODB_REFRESH_TIMEOUT_SECS must be an integer from 1 through 3600")?,
        Err(std::env::VarError::NotPresent) => DEFAULT_REFRESH_SUBPROCESS_TIMEOUT_SECS,
        Err(error) => return Err(error.into()),
    };
    if !(1..=MAX_REFRESH_SUBPROCESS_TIMEOUT_SECS).contains(&seconds) {
        return Err("REPODB_REFRESH_TIMEOUT_SECS must be an integer from 1 through 3600".into());
    }
    let configured = std::time::Duration::from_secs(seconds);
    Ok(crate::budget::remaining_refresh_time()?
        .map_or(configured, |remaining| configured.min(remaining)))
}

pub(crate) struct ProcessDeadline {
    cancelled: mpsc::Sender<()>,
    timed_out: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessDeadline {
    pub(crate) fn start(process_id: u32, timeout: std::time::Duration) -> Self {
        let (cancelled, receiver) = mpsc::channel();
        let timed_out = Arc::new(AtomicBool::new(false));
        let worker_timed_out = Arc::clone(&timed_out);
        let worker = std::thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                worker_timed_out.store(true, Ordering::Release);
                kill_process_group(process_id);
            }
        });
        Self {
            cancelled,
            timed_out,
            worker: Some(worker),
        }
    }

    pub(crate) fn finish(mut self) -> bool {
        let _ = self.cancelled.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.timed_out.load(Ordering::Acquire)
    }
}

impl Drop for ProcessDeadline {
    fn drop(&mut self) {
        let _ = self.cancelled.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn kill_process_group(process_id: u32) {
    #[cfg(unix)]
    // SAFETY: refresh children are started in a process group whose id is their pid.
    unsafe {
        libc::kill(-(process_id as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = process_id;
}

pub(crate) fn spawn_in_process_group(command: &mut Command) -> std::io::Result<Child> {
    refresh_subprocess_timeout().map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0).spawn()
    }
    #[cfg(not(unix))]
    {
        command.spawn()
    }
}

pub(crate) fn spawn_in_process_group_with_file_limit(
    command: &mut Command,
    maximum_file_bytes: u64,
) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let maximum_file_bytes = libc::rlim_t::try_from(maximum_file_bytes).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "file limit is too large")
        })?;
        // SAFETY: the pre-exec hook only invokes async-signal-safe getrlimit/setrlimit
        // and captures a plain integer. It applies a child-only file-size ceiling.
        unsafe {
            command.pre_exec(move || {
                let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
                if libc::getrlimit(libc::RLIMIT_FSIZE, current.as_mut_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut current = current.assume_init();
                let mut soft_limit = maximum_file_bytes.min(current.rlim_max);
                if current.rlim_cur != libc::RLIM_INFINITY {
                    soft_limit = soft_limit.min(current.rlim_cur);
                }
                current.rlim_cur = soft_limit;
                if libc::setrlimit(libc::RLIMIT_FSIZE, &current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    spawn_in_process_group(command)
}

pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exceeded: bool,
}

pub(crate) struct StreamOutput {
    pub status: ExitStatus,
    pub stderr: Vec<u8>,
    pub exceeded: bool,
}

pub(crate) fn drain_bounded(reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    let mut exceeded = false;
    let mut reader = reader;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = limit.saturating_sub(output.len()).min(count);
        // Exact growth keeps retained allocation within the caller's byte budget.
        if keep > output.capacity() - output.len() {
            output
                .try_reserve_exact(keep)
                .map_err(std::io::Error::other)?;
        }
        output.extend_from_slice(&buffer[..keep]);
        debug_assert!(output.capacity() <= limit);
        exceeded |= keep != count;
    }
    Ok((output, exceeded))
}

#[cfg(test)]
pub(crate) fn capture_with_input(
    child: &mut Child,
    input: &[u8],
    stdout_limit: usize,
    stderr_limit: usize,
) -> std::io::Result<Output> {
    capture_streams(child, input, Some(stdout_limit), Some(stderr_limit))
}

#[cfg(test)]
pub(crate) fn capture_with_input_unbounded(
    child: &mut Child,
    input: &[u8],
) -> std::io::Result<Output> {
    capture_streams(child, input, None, None)
}

/// Feeds stdin, drains stderr, and delivers newline-delimited stdout records
/// incrementally while enforcing a total stdout byte limit.
#[cfg(test)]
pub(crate) fn capture_lines_with_input(
    child: &mut Child,
    input: Vec<u8>,
    stdout_limit: usize,
    stderr_limit: usize,
    mut on_line: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<StreamOutput> {
    enum Event {
        Line(Vec<u8>),
        StdoutError(std::io::Error),
        Done(bool),
    }

    let (sender, receiver) = std::sync::mpsc::sync_channel(8);
    let stdout = child.stdout.take().expect("captured stdout");
    let stdout_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut total = 0usize;
        let mut exceeded = false;
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(count) => {
                    total = total.saturating_add(count);
                    if total > stdout_limit {
                        exceeded = true;
                    } else if sender.send(Event::Line(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Event::StdoutError(error));
                    break;
                }
            }
        }
        let _ = sender.send(Event::Done(exceeded));
    });
    let stderr = child.stderr.take().expect("captured stderr");
    let stderr_thread = std::thread::spawn(move || drain_bounded(stderr, stderr_limit));
    let mut stdin = child.stdin.take().expect("captured stdin");
    let stdin_thread = std::thread::spawn(move || stdin.write_all(&input));
    let mut callback_error = None;
    let mut exceeded = false;
    let mut stdout_error = None;
    while let Ok(event) = receiver.recv() {
        match event {
            Event::Line(line) if callback_error.is_none() => {
                if let Err(error) = on_line(&line) {
                    callback_error = Some(error);
                }
            }
            Event::Line(_) => {}
            Event::StdoutError(error) => stdout_error = Some(error),
            Event::Done(was_exceeded) => {
                exceeded = was_exceeded;
                break;
            }
        }
    }
    let status = child.wait()?;
    stdout_thread
        .join()
        .map_err(|_| std::io::Error::other("stdout reader panicked"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| std::io::Error::other("stderr reader panicked"))??
        .0;
    stdin_thread
        .join()
        .map_err(|_| std::io::Error::other("stdin writer panicked"))??;
    if let Some(error) = stdout_error {
        return Err(error);
    }
    if let Some(error) = callback_error {
        return Err(error);
    }
    Ok(StreamOutput {
        status,
        stderr,
        exceeded,
    })
}

/// Captures a process launched in its own process group and enforces a deadline
/// across process execution, stdin writing, and both output streams.
pub(crate) fn capture_lines_with_input_timeout(
    child: &mut Child,
    input: &[u8],
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: std::time::Duration,
    mut on_line: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<StreamOutput> {
    let output = capture_with_input_timeout(child, input, stdout_limit, stderr_limit, timeout)?;
    for line in output.stdout.split_inclusive(|byte| *byte == b'\n') {
        on_line(line)?;
    }
    Ok(StreamOutput {
        status: output.status,
        stderr: output.stderr,
        exceeded: output.exceeded,
    })
}

pub(crate) fn capture_with_input_timeout(
    child: &mut Child,
    input: &[u8],
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: std::time::Duration,
) -> std::io::Result<Output> {
    enum Event {
        Stdin(std::io::Result<()>),
        Stdout(std::io::Result<(Vec<u8>, bool)>),
        Stderr(std::io::Result<(Vec<u8>, bool)>),
    }

    let started = std::time::Instant::now();
    let group_id = child.id();
    let (sender, receiver) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().expect("captured stdout");
    let stderr = child.stderr.take().expect("captured stderr");
    let stdout_sender = sender.clone();
    let stdout_thread = std::thread::spawn(move || {
        let _ = stdout_sender.send(Event::Stdout(drain_bounded(stdout, stdout_limit)));
    });
    let stderr_sender = sender.clone();
    let stderr_thread = std::thread::spawn(move || {
        let _ = stderr_sender.send(Event::Stderr(drain_bounded(stderr, stderr_limit)));
    });
    std::thread::scope(|scope| -> std::io::Result<Output> {
        let mut status = None;
        let mut stdout = None;
        let mut stderr = None;
        let mut stdin = None;
        let mut stdin_thread = child.stdin.take().map(|mut stdin_pipe| {
            scope.spawn(move || {
                let _ = sender.send(Event::Stdin(stdin_pipe.write_all(input)));
            })
        });
        if stdin_thread.is_none() {
            stdin = Some(Ok(()));
        }
        while status.is_none() || stdout.is_none() || stderr.is_none() || stdin.is_none() {
            if let Err(error) = crate::budget::check_refresh_deadline() {
                terminate_process_group(child, group_id);
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                if let Some(thread) = stdin_thread.take() {
                    let _ = thread.join();
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    error.to_string(),
                ));
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(result) => status = result,
                    Err(error) => {
                        terminate_process_group(child, group_id);
                        let _ = child.wait();
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        if let Some(thread) = stdin_thread.take() {
                            let _ = thread.join();
                        }
                        return Err(error);
                    }
                }
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                terminate_process_group(child, group_id);
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                if let Some(thread) = stdin_thread.take() {
                    let _ = thread.join();
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "process timed out",
                ));
            }
            if status.is_some() && stdout.is_some() && stderr.is_some() && stdin.is_some() {
                break;
            }
            match receiver.recv_timeout(remaining.min(std::time::Duration::from_millis(10))) {
                Ok(Event::Stdin(result)) => stdin = Some(result),
                Ok(Event::Stdout(result)) => stdout = Some(result),
                Ok(Event::Stderr(result)) => stderr = Some(result),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }

        if let Some(thread) = stdin_thread.take() {
            thread
                .join()
                .map_err(|_| std::io::Error::other("stdin writer panicked"))?;
        }
        stdout_thread
            .join()
            .map_err(|_| std::io::Error::other("stdout reader panicked"))?;
        stderr_thread
            .join()
            .map_err(|_| std::io::Error::other("stderr reader panicked"))?;
        let (stdout, stdout_exceeded) = stdout
            .expect("stdout result received")
            .map_err(|_| std::io::Error::other("stdout reader failed"))?;
        let (stderr, _) = stderr
            .expect("stderr result received")
            .map_err(|_| std::io::Error::other("stderr reader failed"))?;
        stdin
            .expect("stdin result received")
            .map_err(|_| std::io::Error::other("stdin writer failed"))?;
        Ok(Output {
            status: status.expect("child status received"),
            stdout,
            stderr,
            exceeded: stdout_exceeded,
        })
    })
}

pub(crate) fn terminate_process_group(child: &mut Child, group_id: u32) {
    kill_process_group(group_id);
    let _ = child.kill();
}

#[cfg(test)]
fn capture_streams(
    child: &mut Child,
    input: &[u8],
    stdout_limit: Option<usize>,
    stderr_limit: Option<usize>,
) -> std::io::Result<Output> {
    fn drain(reader: impl Read, limit: Option<usize>) -> std::io::Result<(Vec<u8>, bool)> {
        match limit {
            Some(limit) => drain_bounded(reader, limit),
            None => {
                let mut output = Vec::new();
                let mut reader = reader;
                reader.read_to_end(&mut output)?;
                Ok((output, false))
            }
        }
    }
    let stdout = child.stdout.take().expect("captured stdout");
    let stderr = child.stderr.take().expect("captured stderr");
    let stdout = std::thread::spawn(move || drain(stdout, stdout_limit));
    let stderr = std::thread::spawn(move || drain(stderr, stderr_limit));
    if let Some(mut stdin) = child.stdin.take() {
        let write_result = stdin.write_all(input);
        drop(stdin);
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    let status = child.wait()?;
    let (stdout, stdout_exceeded) = stdout
        .join()
        .map_err(|_| std::io::Error::other("stdout reader panicked"))??;
    let (stderr, _stderr_exceeded) = stderr
        .join()
        .map_err(|_| std::io::Error::other("stderr reader panicked"))??;
    // Stderr is always drained but only an excerpt is retained for diagnostics;
    // excess diagnostic output must not reject otherwise valid command output.
    Ok(Output {
        status,
        stdout,
        stderr,
        exceeded: stdout_exceeded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn streamed_stdout_handles_ten_thousand_records_and_large_stdin() {
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "cat >/dev/null; i=0; while [ $i -lt 10000 ]; do printf '[1,0]\\n'; i=$((i+1)); done",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut records = 0;
        let output = capture_lines_with_input(
            &mut child,
            vec![b'x'; 128 * 1024],
            1024 * 1024,
            64 * 1024,
            |line| {
                assert_eq!(line, b"[1,0]\n");
                records += 1;
                Ok(())
            },
        )
        .unwrap();
        assert!(output.status.success());
        assert!(!output.exceeded);
        assert_eq!(records, 10_000);
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_output_floods_are_drained_with_bounded_retention() {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = super::spawn_in_process_group(&mut command).unwrap();
        let output = super::capture_with_input_timeout(
            &mut child,
            b"",
            64,
            32,
            std::time::Duration::from_secs(5),
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 64);
        assert!(output.exceeded);
        assert_eq!(output.stderr.len(), 32);
        assert!(output.stdout.capacity() <= 64);
        assert!(output.stderr.capacity() <= 32);
    }

    #[test]
    fn bounded_drain_capacity_never_exceeds_limit() {
        for limit in [0usize, 1, 8191, 8192, 8193, 16 * 1024 + 3] {
            let input = vec![b'x'; limit.saturating_add(4096)];
            let (output, exceeded) = drain_bounded(input.as_slice(), limit).unwrap();
            assert_eq!(output.len(), limit);
            assert!(output.capacity() <= limit);
            assert_eq!(exceeded, input.len() > limit);
        }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_descendants_holding_capture_pipes() {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 2 & exit 0"])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let started = std::time::Instant::now();
        let result = capture_with_input_timeout(
            &mut child,
            &[],
            64,
            64,
            std::time::Duration::from_millis(100),
        );
        assert!(matches!(
            result,
            Err(ref error) if error.kind() == std::io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn line_capture_timeout_terminates_hung_embedding_child() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "cat >/dev/null; sleep 2"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = spawn_in_process_group(&mut command).unwrap();
        let result = capture_lines_with_input_timeout(
            &mut child,
            b"input",
            1024,
            64,
            std::time::Duration::from_millis(100),
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(ref error) if error.kind() == std::io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn unbounded_capture_drains_output_while_feeding_large_stdin() {
        let mut child = Command::new("sh")
            .args(["-c", "cat >/dev/null; head -c 1048576 /dev/zero"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = vec![b'x'; 1024 * 1024];
        let output = capture_with_input_unbounded(&mut child, &input).unwrap();
        assert!(output.status.success());
        assert!(!output.exceeded);
        assert_eq!(output.stdout.len(), 1024 * 1024);
    }

    #[test]
    fn drains_output_while_feeding_large_stdin() {
        let mut child = Command::new("sh")
            .args([
                "-c",
                "dd if=/dev/zero bs=65536 count=4 2>/dev/null; cat >/dev/null; printf done",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = vec![b'x'; 256 * 1024];
        let output = capture_with_input(&mut child, &input, 16, 64).unwrap();
        assert!(output.status.success());
        assert!(output.exceeded);
        assert_eq!(output.stdout.len(), 16);
    }
}
