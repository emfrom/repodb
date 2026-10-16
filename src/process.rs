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
        output.extend_from_slice(&buffer[..keep]);
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
    let stdout = child.stdout.take().expect("captured stdout");
    let stderr = child.stderr.take().expect("captured stderr");
    let stdout = std::thread::spawn(move || drain_bounded(stdout, stdout_limit));
    let stderr = std::thread::spawn(move || drain_bounded(stderr, stderr_limit));
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
    let (stderr, stderr_exceeded) = stderr
        .join()
        .map_err(|_| std::io::Error::other("stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
        exceeded: stdout_exceeded || stderr_exceeded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

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
