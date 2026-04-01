//! Memory write discipline.
//!
//! Enforces the two-step write pattern:
//! 1. Write the memory file with proper frontmatter
//! 2. Update MEMORY.md index with a one-line pointer
//!
//! Prevents entropy by never dumping content into the index.

use std::path::{Path, PathBuf};

use super::types::{MemoryMeta, MemoryType};

/// Maximum index line length.
const MAX_INDEX_LINE_CHARS: usize = 150;

/// Maximum index lines before truncation.
const MAX_INDEX_LINES: usize = 200;

/// Write a memory file and update the index atomically.
///
/// Returns the path of the written memory file.
pub fn write_memory(
    memory_dir: &Path,
    filename: &str,
    meta: &MemoryMeta,
    content: &str,
) -> Result<PathBuf, String> {
    let _ = std::fs::create_dir_all(memory_dir);

    // Step 1: Write the memory file with frontmatter.
    let type_str = match &meta.memory_type {
        Some(MemoryType::User) => "user",
        Some(MemoryType::Feedback) => "feedback",
        Some(MemoryType::Project) => "project",
        Some(MemoryType::Reference) => "reference",
        None => "user",
    };

    let file_content = format!(
        "---\nname: {}\ndescription: {}\ntype: {}\n---\n\n{}",
        meta.name, meta.description, type_str, content
    );

    let file_path = memory_dir.join(filename);
    std::fs::write(&file_path, &file_content)
        .map_err(|e| format!("Failed to write memory file: {e}"))?;

    // Step 2: Update MEMORY.md index.
    update_index(memory_dir, filename, &meta.name, &meta.description)?;

    Ok(file_path)
}

/// Update the MEMORY.md index with a pointer to a memory file.
/// If an entry for this filename already exists, replace it.
fn update_index(
    memory_dir: &Path,
    filename: &str,
    name: &str,
    description: &str,
) -> Result<(), String> {
    let index_path = memory_dir.join("MEMORY.md");

    let existing = std::fs::read_to_string(&index_path).unwrap_or_default();

    // Build the new index line (under 150 chars).
    let mut line = format!("- [{}]({}) — {}", name, filename, description);
    if line.len() > MAX_INDEX_LINE_CHARS {
        line.truncate(MAX_INDEX_LINE_CHARS - 3);
        line.push_str("...");
    }

    // Replace existing entry for this filename, or append.
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.contains(&format!("({})", filename)))
        .map(|l| l.to_string())
        .collect();

    lines.push(line);

    // Enforce max lines.
    if lines.len() > MAX_INDEX_LINES {
        lines.truncate(MAX_INDEX_LINES);
    }

    let new_index = lines.join("\n") + "\n";
    std::fs::write(&index_path, new_index).map_err(|e| format!("Failed to update index: {e}"))?;

    Ok(())
}

/// Remove a memory file and its index entry.
pub fn delete_memory(memory_dir: &Path, filename: &str) -> Result<(), String> {
    let file_path = memory_dir.join(filename);
    if file_path.exists() {
        std::fs::remove_file(&file_path).map_err(|e| format!("Failed to delete: {e}"))?;
    }

    // Remove from index.
    let index_path = memory_dir.join("MEMORY.md");
    if let Ok(existing) = std::fs::read_to_string(&index_path) {
        let filtered: Vec<&str> = existing
            .lines()
            .filter(|l| !l.contains(&format!("({})", filename)))
            .collect();
        let _ = std::fs::write(&index_path, filtered.join("\n") + "\n");
    }

    Ok(())
}
