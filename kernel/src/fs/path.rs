// Copyright (C) 2026 KontsnorOS Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Path resolution utilities.
//!
//! Handles resolving file paths through the VFS, including
//! `.` (current directory), `..` (parent directory), and symlinks.

use alloc::string::String;
use alloc::vec::Vec;

/// Check if a path is already clean and normalized without heap allocations.
///
/// Returns `true` if the path contains no duplicate slashes (`//`), no `.` or `..`
/// path components, and (for non-root paths) no trailing slash.
pub fn is_normalized(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path == "/" {
        return true;
    }

    let bytes = path.as_bytes();
    let len = bytes.len();

    // Trailing slash on non-root paths is not normalized
    if len > 1 && bytes[len - 1] == b'/' {
        return false;
    }

    let is_abs = bytes[0] == b'/';
    let mut i = if is_abs { 1 } else { 0 };

    while i < len {
        // Find end of current component
        let start = i;
        while i < len && bytes[i] != b'/' {
            i += 1;
        }
        let comp_len = i - start;

        // Empty component means consecutive slashes (e.g., "//")
        if comp_len == 0 {
            return false;
        }

        // Check for "." or ".."
        if comp_len == 1 && bytes[start] == b'.' {
            return false;
        }
        if comp_len == 2 && bytes[start] == b'.' && bytes[start + 1] == b'.' {
            return false;
        }

        // Skip the slash separating components
        if i < len {
            i += 1;
        }
    }

    true
}

/// Normalize a path by resolving `.` and `..` components.
///
/// # Examples
///
/// ```
/// assert_eq!(normalize("/usr/./local/../bin"), "/usr/bin");
/// assert_eq!(normalize("///foo//bar"), "/foo/bar");
/// ```
pub fn normalize(path: &str) -> String {
    // Fast path: if already normalized, return String copy directly (zero component allocation)
    if is_normalized(path) {
        return String::from(path);
    }

    let is_absolute = path.starts_with('/');
    let mut components: Vec<&str> = Vec::new();

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

    let mut result = String::with_capacity(path.len());
    if is_absolute {
        result.push('/');
    }

    for (i, comp) in components.iter().enumerate() {
        if i > 0 {
            result.push('/');
        }
        result.push_str(comp);
    }

    if result.is_empty() {
        result.push('/');
    }

    result
}

/// Normalize `path` while ensuring it never escapes `root`.
///
/// This is the jail-aware variant used by the VFS path resolver when a task
/// has a non-`"/"` `FsContext::root`.  A `..` component that would pop above
/// `root_components` is silently clamped, making `chroot` escapes impossible
/// via repeated `../` traversal.
///
/// Both `path` and `root` must be absolute (`/`-prefixed) strings; `root`
/// should already be normalized.
pub fn normalize_jailed(path: &str, root: &str) -> String {
    // Build the root component stack so we know the minimum depth.
    let root_parts: Vec<&str> = root.split('/').filter(|s| !s.is_empty()).collect();
    let root_depth = root_parts.len();

    let mut components: Vec<&str> = Vec::new();

    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                // Only pop if we have more components than the root floor.
                if components.len() > root_depth {
                    components.pop();
                }
                // If already at root floor, the `..` is silently discarded.
            }
            name => components.push(name),
        }
    }

    // Ensure components at least contains the root floor.
    if components.len() < root_depth {
        components = root_parts;
    }

    let mut result = String::with_capacity(path.len().max(root.len()) + 1);
    result.push('/');
    for (i, comp) in components.iter().enumerate() {
        if i > 0 {
            result.push('/');
        }
        result.push_str(comp);
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

    let extra = if base.ends_with('/') { 0 } else { 1 };
    let mut result = String::with_capacity(base.len() + extra + name.len());
    result.push_str(base);
    if extra == 1 {
        result.push('/');
    }
    result.push_str(name);
    result
}
