use super::*;

pub(crate) struct RecordingSpawner<F>(pub F);
impl<F> Spawner for RecordingSpawner<F>
where
    F: Fn(&Command) -> io::Result<Output> + Send + Sync,
{
    fn output<'a>(
        &'a self,
        command: &'a mut Command,
    ) -> Pin<Box<dyn Future<Output = io::Result<Output>> + Send + 'a>> {
        Box::pin(async move { (self.0)(command) })
    }
}

pub(crate) fn output(code: i32, stdout: impl Into<Vec<u8>>) -> Output {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    };
    Output {
        status,
        stdout: stdout.into(),
        stderr: Vec::new(),
    }
}

pub(crate) fn reject(_: &Command) -> io::Result<Output> {
    panic!("unexpected command in a filesystem-only updater fixture")
}
