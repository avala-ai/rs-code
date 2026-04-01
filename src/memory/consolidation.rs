//! Memory consolidation ("dreaming").
//!
//! Background process that reviews memory files and consolidates
//! them: merging duplicates, resolving contradictions, converting
//! relative dates to absolute, pruning stale entries, and keeping
//! the index under limits.
//!
//! Uses a lock file to prevent concurrent consolidation across
//! multiple agent sessions.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Minimum hours between consolidation runs.
const MIN_HOURS_BETWEEN_RUNS: u64 = 24;

/// Lock file name within the memory directory.
const LOCK_FILE: &str = ".consolidate-lock";

/// Check if consolidation should run.
pub fn should_consolidate(memory_dir: &Path) -> bool {
    let lock_path = memory_dir.join(LOCK_FILE);

    // If lock doesn't exist, we've never consolidated.
    let modified = match std::fs::metadata(&lock_path)
        .ok()
        .and_then(|m| m.modified().ok())
    {
        Some(t) => t,
        None => return true, // Never run before.
    };

    let elapsed = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);

    elapsed.as_secs() >= MIN_HOURS_BETWEEN_RUNS * 3600
}

/// Try to acquire the consolidation lock.
/// Returns the lock path if acquired, None if another process holds it.
pub fn try_acquire_lock(memory_dir: &Path) -> Option<PathBuf> {
    let lock_path = memory_dir.join(LOCK_FILE);

    // Check for existing lock.
    if lock_path.exists()
        && let Ok(content) = std::fs::read_to_string(&lock_path)
    {
        let pid_str = content.trim();
        if let Ok(pid) = pid_str.parse::<u32>() {
            // Check if the holding process is still alive.
            if is_process_alive(pid) {
                // Check if lock is stale (> 1 hour).
                if let Ok(meta) = std::fs::metadata(&lock_path)
                    && let Ok(modified) = meta.modified()
                {
                    let age = SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or(Duration::ZERO);
                    if age.as_secs() < 3600 {
                        return None; // Lock is fresh and holder is alive.
                    }
                }
            }
            // Process is dead or lock is stale — reclaim.
        }
    }

    // Write our PID to the lock file.
    let pid = std::process::id();
    if std::fs::write(&lock_path, pid.to_string()).is_err() {
        return None;
    }

    // Verify we actually hold the lock (race protection).
    if let Ok(content) = std::fs::read_to_string(&lock_path)
        && content.trim() == pid.to_string()
    {
        return Some(lock_path);
    }

    None // Lost the race.
}

/// Release the consolidation lock by updating its mtime to now.
/// This marks the consolidation as complete (mtime = last consolidated time).
pub fn release_lock(lock_path: &Path) {
    // Rewrite the file to update mtime to now.
    let _ = std::fs::write(lock_path, std::process::id().to_string());
}

/// Roll back the lock on failure (rewind mtime so next session retries).
pub fn rollback_lock(lock_path: &Path) {
    // Delete the lock file so next check sees "never consolidated".
    let _ = std::fs::remove_file(lock_path);
}

/// Build the consolidation prompt for the dream agent.
pub fn build_consolidation_prompt(memory_dir: &Path) -> String {
    let mut prompt = String::from(
        "You are a memory consolidation agent. Review and improve the memory \
         directory. Work in four phases:\n\n\
         Phase 1 — Orient:\n\
         - List the memory directory contents\n\
         - Read MEMORY.md to understand the current index\n\
         - Skim existing files to avoid creating duplicates\n\n\
         Phase 2 — Identify issues:\n\
         - Find duplicate or near-duplicate memories\n\
         - Find contradictions between memory files\n\
         - Find memories with relative dates (convert to absolute)\n\
         - Find memories about things derivable from code (delete these)\n\n\
         Phase 3 — Consolidate:\n\
         - Merge duplicates into single files\n\
         - Delete contradicted facts at the source\n\
         - Update vague descriptions with specific ones\n\
         - Remove memories about code patterns, git history, or debugging\n\n\
         Phase 4 — Prune and index:\n\
         - Update MEMORY.md to stay under 200 lines\n\
         - Remove pointers to deleted files\n\
         - Shorten verbose index entries (detail belongs in topic files)\n\
         - Resolve contradictions between index and files\n\n\
         Be aggressive about pruning. Less memory is better than stale memory.\n",
    );

    prompt.push_str(&format!("\nMemory directory: {}\n", memory_dir.display()));

    prompt
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks if process exists without sending a signal.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        true // Assume alive on non-Unix.
    }
}
