//! Strict proxy-fixture cleanup with read-only evidence on the failure path.

use std::path::{Path, PathBuf};

#[track_caller]
pub(super) fn close_temp_dir(data_dir: tempfile::TempDir) {
    let root = data_dir.path().to_path_buf();
    if let Err(error) = data_dir.close() {
        cleanup_failure(&root, error);
    }
}

/// Stable debugger entry reached only after strict directory removal failed.
/// The process and remaining files are still available for ownership inspection.
#[cold]
#[inline(never)]
#[track_caller]
fn cleanup_failure(root: &Path, error: std::io::Error) -> ! {
    fn remaining_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(kind) = entry.file_type() {
                    if kind.is_dir() {
                        remaining_files(root, &path, files);
                    } else if let Ok(relative) = path.strip_prefix(root) {
                        files.push(relative.to_path_buf());
                    }
                }
            }
        }
    }

    let mut files = Vec::new();
    remaining_files(root, root, &mut files);
    files.sort();
    #[cfg(windows)]
    // TempDir::close wraps the OS error in path context. Never gate this query
    // on the outer raw_os_error: inspect every strict cleanup failure once.
    let lock_owners = {
        let absolute_files: Vec<_> = files.iter().map(|file| root.join(file)).collect();
        let report = super::windows_lock_owners::lock_owners(&absolute_files);
        let owners: Vec<_> = report
            .owners
            .iter()
            .map(|owner| (owner.pid, owner.kind, owner.application_type))
            .collect();
        format!(
            "PID/kind/application_type: {owners:?}; query_error: {:?}; end_session_error: {:?}",
            report.query_error, report.end_session_error
        )
    };
    #[cfg(not(windows))]
    let lock_owners = "not available on this platform";
    panic!(
        "remove temporary gateway directory: {error}; remaining relative files: {files:?}; lock owners: {lock_owners}"
    );
}
