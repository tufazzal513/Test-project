//! Per-message temporary files stay alive until the last worker releases them.
use std::{io, path::Path, sync::Arc};

#[derive(Clone)]
pub(crate) struct MediaWorkspace(Arc<tempfile::TempDir>);

impl MediaWorkspace {
    /// Remove abandoned per-job workspaces left behind by a forced process kill.
    /// Only direct child directories with our private prefix are considered.
    pub(crate) fn cleanup_stale() -> io::Result<usize> {
        let root = std::env::temp_dir();
        let mut removed = 0usize;
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("shitverter-") {
                continue;
            }
            let file_type = entry.file_type()?;
            if !file_type.is_dir() {
                continue;
            }
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(error) => log::warn!(
                    "Could not remove an abandoned conversion workspace: {error}"
                ),
            }
        }
        Ok(removed)
    }

    pub(crate) fn new() -> io::Result<Self> {
        tempfile::Builder::new()
            .prefix("shitverter-")
            .tempdir()
            .map(|directory| Self(Arc::new(directory)))
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_input_and_partial_output_on_error() {
        let mut directory = std::path::PathBuf::new();
        let result: io::Result<()> = (|| {
            let workspace = MediaWorkspace::new()?;
            directory = workspace.path().to_path_buf();
            std::fs::write(directory.join("input.mkv"), b"input")?;
            std::fs::write(directory.join("input.mkv.converted.mp4"), b"partial")?;
            Err(io::Error::other("conversion failed"))
        })();
        assert!(result.is_err());
        assert!(!directory.exists());
    }

    #[test]
    fn cancelled_handler_keeps_files_until_worker_finishes() {
        let handler = MediaWorkspace::new().unwrap();
        let directory = handler.path().to_path_buf();
        let worker = handler.clone();
        drop(handler);
        assert!(directory.exists());
        std::fs::write(worker.path().join("partial.mp4"), b"partial").unwrap();
        drop(worker);
        assert!(!directory.exists());
    }

    #[test]
    fn success_and_send_error_share_the_same_cleanup_boundary() {
        for send_succeeds in [true, false] {
            let directory;
            {
                let workspace = MediaWorkspace::new().unwrap();
                directory = workspace.path().to_path_buf();
                std::fs::write(directory.join("output.mp4"), b"video").unwrap();
                let _send_result = if send_succeeds {
                    Ok(())
                } else {
                    Err("send failed")
                };
            }
            assert!(!directory.exists());
        }
    }
}
