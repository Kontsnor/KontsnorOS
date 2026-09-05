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

//! In-kernel Extended Attributes (xattr) storage.

use crate::sync::spinlock::TicketLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub static XATTR_STORE: TicketLock<BTreeMap<(u64, String), Vec<u8>>> =
    TicketLock::new(BTreeMap::new());

/// Get an extended attribute for an inode.
pub fn get_xattr(ino: u64, name: &str) -> Option<Vec<u8>> {
    let store = XATTR_STORE.lock();
    store.get(&(ino, String::from(name))).cloned()
}

/// Set an extended attribute for an inode.
/// Flags:
/// - XATTR_CREATE (1): Fail if attribute already exists.
/// - XATTR_REPLACE (2): Fail if attribute does not exist.
pub fn set_xattr(ino: u64, name: &str, value: &[u8], flags: i32) -> Result<(), i64> {
    let mut store = XATTR_STORE.lock();
    let key = (ino, String::from(name));
    let exists = store.contains_key(&key);

    if flags == 1 && exists {
        return Err(-17); // EEXIST
    }
    if flags == 2 && !exists {
        return Err(-61); // ENODATA
    }

    store.insert(key, value.to_vec());
    Ok(())
}

/// List all extended attribute names for an inode.
/// Returns names formatted as consecutive null-terminated strings.
pub fn list_xattr(ino: u64) -> Vec<u8> {
    let store = XATTR_STORE.lock();
    let mut result = Vec::new();
    for ((i, name), _) in store.iter() {
        if *i == ino {
            result.extend_from_slice(name.as_bytes());
            result.push(0);
        }
    }
    result
}

/// Remove an extended attribute for an inode.
pub fn remove_xattr(ino: u64, name: &str) -> Result<(), i64> {
    let mut store = XATTR_STORE.lock();
    let key = (ino, String::from(name));
    if store.remove(&key).is_some() {
        Ok(())
    } else {
        Err(-61) // ENODATA
    }
}
