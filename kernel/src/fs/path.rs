//! Path resolution utilities.
//!
//! Handles resolving file paths through the VFS, including
//! `.` (current directory), `..` (parent directory), and symlinks.

use alloc::string::String;
use alloc::vec::Vec;

/// Normalize a path by resolving `.` and `..` components.
///
/// # Examples
///
/// ```
/// assert_eq!(normalize("/usr/./local/../bin"), "/usr/bin");
/// assert_eq!(normalize("///foo//bar"), "/foo/bar");
/// ```
pub fn normalize(path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    let is_absolute = path.starts_with('/');

    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                if !components.is_empty() {
                    components.pop();
                }
            }
            name => components.push(name),
        }
    }

    let mut result = String::new();
    if is_absolute {
        result.push('/');
    }
    result.push_str(&components.join("/"));

    if result.is_empty() {
        result.push('/');
    }

    result
}

/// Split a path into its parent directory and final component.
///
/// # Examples
///
/// ```
/// assert_eq!(split_path("/usr/local/bin"), ("/usr/local", "bin"));
/// assert_eq!(split_path("/foo"), ("/", "foo"));
/// ```
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(pos) => (&path[..pos], &path[pos + 1..]),
        None => (".", path),
    }
}

/// Get the file name from a path.
pub fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[pos + 1..],
        None => path,
    }
}

/// Get the directory portion of a path.
pub fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(pos) => &path[..pos],
        None => ".",
    }
}

/// Join two path components.
pub fn join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return String::from(name);
    }

    let mut result = String::from(base);
    if !result.ends_with('/') {
        result.push('/');
    }
    result.push_str(name);
    result
}
