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
