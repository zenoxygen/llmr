use std::ffi::OsStr;
use std::fs::{metadata, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use ignore::WalkBuilder;
use tiktoken_rs::{cl100k_base, CoreBPE};

/// Default values for the limits.
const DEFAULT_MAX_FILE_SIZE: u64 = 1024 * 1024; // 1MB
const DEFAULT_MAX_TOTAL_SIZE: u64 = 1024 * 1024 * 100; // 100MB
const DEFAULT_MAX_FILES: usize = 10000;

#[derive(Parser, Debug)]
#[command(author, version, about = "Feed your codebase into any LLM.")]
struct Args {
    /// Output the report.
    #[arg(short, long)]
    report: bool,
    /// Maximum file size to process (in bytes).
    #[arg(short = 'f', long = "file-size", default_value_t = DEFAULT_MAX_FILE_SIZE)]
    max_file_size: u64,
    /// Maximum total size of files to process (in bytes).
    #[arg(short = 't', long = "total-size", default_value_t = DEFAULT_MAX_TOTAL_SIZE)]
    max_total_size: u64,
    /// Maximum number of files to process.
    #[arg(short = 'n', long = "num-files", default_value_t = DEFAULT_MAX_FILES)]
    max_files: usize,
}

/// Check if a file is likely a text file.
fn is_text_file(path: &Path) -> Result<bool> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut buffer = [0u8; 1024];

    let bytes_read = file
        .read(&mut buffer)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    Ok(!buffer[..bytes_read]
        .iter()
        .any(|&byte| byte < 0x20 && byte != 0x09 && byte != 0x0a && byte != 0x0d))
}

/// Read the content of a file.
fn read_file_content(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut content = String::new();

    file.read_to_string(&mut content)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    Ok(content)
}

/// Helper function to format size in bytes, KB, MB.
fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{} bytes", size)
    } else if size < 1024 * 1024 {
        format!("{:.2} KB", size as f64 / 1024.0)
    } else {
        format!("{:.2} MB", size as f64 / (1024.0 * 1024.0))
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let start_time = Instant::now();
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;

    let mut total_files: usize = 0;
    let mut total_size: u64 = 0;
    let mut file_contents: Vec<(PathBuf, String)> = Vec::new();
    let mut tree_structure: String = String::new();
    let mut errors: Vec<String> = Vec::new();

    // Build the file walker, respecting gitignore
    let walker = WalkBuilder::new(&current_dir).git_ignore(true).build();

    // Iterate through all entries (files and directories) found by the walker
    for entry in walker {
        let entry = entry.with_context(|| "Error during directory traversal")?;
        let path = entry.path();

        // Skip the root path of the current directory
        if path == current_dir {
            tree_structure.push_str(&format!(
                "└── {}\n",
                path.file_name().and_then(OsStr::to_str).unwrap_or(".")
            ));
            continue;
        }

        // Handle directories
        if entry.file_type().map_or(false, |ft| ft.is_dir()) {
            let relative_path = path.strip_prefix(&current_dir).with_context(|| {
                format!("Failed to strip prefix for directory: {}", path.display())
            })?;
            let indent_level = relative_path.iter().count();
            let indent = "    ".repeat(indent_level - 1);
            let file_name = relative_path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or(".");

            tree_structure.push_str(&format!("{}├── {}\n", indent, file_name));
            continue;
        }

        // Process files
        if path.is_file() {
            let relative_path = path
                .strip_prefix(&current_dir)
                .with_context(|| format!("Failed to strip prefix for file: {}", path.display()))?;
            let indent_level = relative_path.iter().count();
            let indent = "    ".repeat(indent_level - 1);
            let file_name = relative_path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or(".");

            let file_size = metadata(path)
                .with_context(|| format!("Failed to get metadata for file: {}", path.display()))?
                .len();

            // Check limits for number of files
            if total_files >= args.max_files {
                errors.push(format!(
                    "Skipping file {}: Maximum file limit ({}) reached",
                    path.display(),
                    args.max_files
                ));
                continue;
            }

            // Check limits for total size
            if total_size + file_size > args.max_total_size {
                errors.push(format!(
                    "Skipping file {}: Total size limit ({}) reached",
                    path.display(),
                    format_size(args.max_total_size)
                ));
                continue;
            }

            // Check limits for max file size
            if file_size > args.max_file_size {
                errors.push(format!(
                    "Skipping file {}: File exceeds maximum size ({})",
                    path.display(),
                    format_size(args.max_file_size)
                ));
                continue;
            }

            // If the file is a text file, then we process it
            if is_text_file(path)
                .with_context(|| format!("Error checking if file is text: {}", path.display()))?
            {
                match read_file_content(path) {
                    Ok(content) => {
                        // Push the content to the vector
                        file_contents.push((path.to_path_buf(), content));
                        // Increment counters
                        total_size += file_size;
                        total_files += 1;
                        tree_structure.push_str(&format!("{}└── {}\n", indent, file_name));
                    }
                    Err(e) => {
                        errors.push(format!("Error reading file {}: {}", path.display(), e));
                    }
                }
            } else {
                tree_structure.push_str(&format!("{}└── {} [Non-text file]\n", indent, file_name));
            }
        }
    }

    // Print the directory structure
    println!("{}", tree_structure.trim_end());

    // Print all the file content
    for (path, content) in &file_contents {
        println!("==================================================");
        println!(
            "File: {}",
            path.strip_prefix(&current_dir)
                .with_context(|| format!("Failed to strip prefix for file: {}", path.display()))?
                .display()
        );
        println!("==================================================");
        println!("{}", content.trim_end());
    }

    // Print the errors
    for error in &errors {
        eprintln!("{}", error);
    }

    if args.report {
        // Combine all the content of the files in a String
        let combined_content = file_contents
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<Vec<&str>>()
            .join("");

        // Estimate tokens
        let bpe: CoreBPE = cl100k_base().context("Failed to get BPE tokenizer")?;
        let estimated_tokens = bpe.encode_ordinary(&combined_content).len();

        let elapsed_time = start_time.elapsed();

        // Print the report at the end
        println!("Analyzing: {}", current_dir.display());
        println!("Files analyzed: {}", total_files);
        println!("Estimated tokens: {}", estimated_tokens);
        println!("Time elapsed: {:.2?}", elapsed_time);
    }

    Ok(())
}
