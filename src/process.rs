use std::{
    io::{Read, Write},
    process::{Child, ExitStatus},
};

pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
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

pub(crate) fn capture(
    child: &mut Child,
    stdout_limit: usize,
    stderr_limit: usize,
) -> std::io::Result<Output> {
    capture_with_input(child, &[], stdout_limit, stderr_limit)
}

pub(crate) fn capture_with_input(
    child: &mut Child,
    input: &[u8],
    stdout_limit: usize,
    stderr_limit: usize,
) -> std::io::Result<Output> {
    capture_streams(child, input, Some(stdout_limit), Some(stderr_limit))
}

pub(crate) fn capture_with_input_unbounded(
    child: &mut Child,
    input: &[u8],
) -> std::io::Result<Output> {
    capture_streams(child, input, None, None)
}

/// Captures a process launched in its own process group and enforces a deadline
/// across process execution, stdin writing, and both output streams.
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
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut stdin = None;
    let mut stdin_thread = child.stdin.take().map(|mut stdin_pipe| {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = sender.send(Event::Stdin(stdin_pipe.write_all(&input)));
        })
    });
    if stdin_thread.is_none() {
        stdin = Some(Ok(()));
    }
    while status.is_none() || stdout.is_none() || stderr.is_none() || stdin.is_none() {
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
}

fn terminate_process_group(child: &mut Child, group_id: u32) {
    #[cfg(unix)]
    // SAFETY: the segmenter is spawned into a process group whose id is its pid.
    unsafe {
        libc::kill(-(group_id as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

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
