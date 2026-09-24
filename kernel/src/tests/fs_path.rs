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

//! VFS path canonicalization unit tests.

use crate::fs::path::{
    basename, dirname, is_normalized, join, normalize, normalize_jailed, split_path,
};
use crate::kprintln;

#[test_case]
fn test_vfs_path_canonicalization() {
    kprintln!("[test] Starting VFS path canonicalization unit tests...");

    // 1. Test is_normalized scanner
    assert!(is_normalized("/"));
    assert!(is_normalized("/usr/bin"));
    assert!(is_normalized("/a/b/c"));
    assert!(is_normalized("foo/bar"));
    assert!(!is_normalized(""));
    assert!(!is_normalized("//usr/bin"));
    assert!(!is_normalized("/usr/bin/"));
    assert!(!is_normalized("/usr/./bin"));
    assert!(!is_normalized("/usr/../bin"));

    // 2. Test normalize component resolution
    assert_eq!(normalize("/usr/bin"), "/usr/bin");
    assert_eq!(normalize("/usr/./local/../bin"), "/usr/bin");
    assert_eq!(normalize("///foo//bar"), "/foo/bar");
    assert_eq!(normalize("a/b/../c"), "a/c");
    assert_eq!(normalize(""), "/");
    assert_eq!(normalize("/../../.."), "/");

    // 3. Test join helper
    assert_eq!(join("/usr", "bin"), "/usr/bin");
    assert_eq!(join("/usr/", "bin"), "/usr/bin");
    assert_eq!(join("/usr", "/bin"), "/bin");

    // 4. Test normalize_jailed chroot clamping
    assert_eq!(normalize_jailed("/jail/a/b/../../c", "/jail"), "/jail/c");
    assert_eq!(normalize_jailed("/jail/../../..", "/jail"), "/jail");
    assert_eq!(
        normalize_jailed("/jail/foo/../../bar", "/jail"),
        "/jail/bar"
    );

    // 5. Test split_path, basename, and dirname
    assert_eq!(split_path("/usr/local/bin"), ("/usr/local", "bin"));
    assert_eq!(split_path("/foo"), ("/", "foo"));
    assert_eq!(split_path("bar"), (".", "bar"));

    assert_eq!(basename("/usr/bin/cat"), "cat");
    assert_eq!(basename("file.txt"), "file.txt");

    assert_eq!(dirname("/usr/bin/cat"), "/usr/bin");
    assert_eq!(dirname("/file.txt"), "/");
    assert_eq!(dirname("file.txt"), ".");

    kprintln!("[test] VFS path canonicalization unit tests PASSED!");
}
