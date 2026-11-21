//! Handle-relative filesystem operations for Coding Assistant snapshots.
//!
//! The production entry points at the end of the implementation use verified
//! directory handles, bounded in-memory collection, exclusive destination
//! creation, schema-2 inventories, exact-tree preflight, and retry-safe cleanup.
//! Unit tests retain deterministic race hooks for regression coverage.

use cap_primitives::fs::{open_dir_nofollow, read_dir};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
pub(crate) type RuntimeTreeIdentity = (u64, u64);
#[cfg(windows)]
pub(crate) type RuntimeTreeIdentity = (u64, [u8; 16]);

/// Collect candidate regular-file paths relative to an already-open root.
///
/// Every directory descent is performed relative to its held parent handle and
/// uses open_dir_nofollow. Returned paths are descriptive only and must never
/// be used for I/O; a future copier must open each file relative to its parent
/// handle with an independently reviewed no-follow file-open primitive.
#[allow(dead_code)]
pub(crate) fn collect_regular_file_paths(
    root: &File,
    max_depth: usize,
    max_entries: usize,
) -> io::Result<Vec<PathBuf>> {
    if max_depth == 0 || max_entries == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "candidate scan limits must be non-zero"));
    }
    let mut files = Vec::new();
    let mut entries_scanned = 0usize;
    walk_directory(root, Path::new(""), 0, max_depth, max_entries, &mut entries_scanned, &mut files, &mut |_| Ok(()))?;
    Ok(files)
}

/// Enumerate a runtime bundle through a held root handle. Unlike the older
/// snapshot candidate walker, this rejects links, reparse points, special files,
/// excessive depth, and excessive entry counts instead of silently skipping them.
/// Returned paths are descriptive only; file reads must still use the bounded
/// handle-relative reader.
pub(crate) fn collect_runtime_tree_inventory(
    root: &File,
    max_depth: usize,
    max_entries: usize,
) -> io::Result<(Vec<String>, Vec<String>)> {
    if max_depth == 0 || max_entries == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime inventory limits must be non-zero"));
    }
    let metadata = root.metadata()?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime inventory root is not an ordinary directory"));
    }
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut count = 0usize;
    collect_runtime_tree_directory(root, Path::new(""), 0, max_depth, max_entries, &mut count, &mut files, &mut directories)?;
    Ok((files, directories))
}

/// Read a bounded, stable list of immediate child names from a held directory handle.
/// The caller receives no partial list: any overflow or read error fails closed.
pub(crate) fn collect_bounded_directory_entry_names(
    directory: &File,
    max_entries: usize,
) -> io::Result<Vec<OsString>> {
    if max_entries == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "directory entry limit must be non-zero"));
    }
    let mut names = Vec::new();
    for entry in read_dir(directory, Path::new("."))? {
        let entry = entry?;
        if names.len() >= max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "directory exceeds its entry limit"));
        }
        names.push(entry.file_name());
    }
    Ok(names)
}

#[derive(Clone)]
pub(crate) struct RuntimeTreeEntry {
    pub(crate) name: OsString,
    pub(crate) is_directory: bool,
    pub(crate) is_file: bool,
    pub(crate) is_reparse: bool,
    pub(crate) identity: RuntimeTreeIdentity,
}

#[cfg(unix)]
pub(crate) fn enumerate_runtime_tree_entries(parent: &File, max_entries: usize) -> io::Result<Vec<RuntimeTreeEntry>> {
    let mut entries = Vec::new();
    for entry in read_dir(parent, Path::new("."))? {
        if entries.len() >= max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains too many entries"));
        }
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let metadata = entry.metadata()?;
        entries.push(RuntimeTreeEntry {
            name,
            is_directory: kind.is_dir(),
            is_file: kind.is_file(),
            is_reparse: kind.is_symlink() || metadata.file_type().is_symlink(),
            identity: metadata_identity(&metadata)?,
        });
    }
    Ok(entries)
}

#[cfg(windows)]
pub(crate) fn enumerate_runtime_tree_entries(parent: &File, max_entries: usize) -> io::Result<Vec<RuntimeTreeEntry>> {
    windows_identity::enumerate_directory(parent, max_entries)
        .map(|entries| entries.into_iter().map(|(name, attributes, identity)| RuntimeTreeEntry {
            name,
            is_directory: attributes & 0x10 != 0,
            is_file: attributes & 0x10 == 0,
            is_reparse: attributes & 0x400 != 0,
            identity,
        }).collect())
}

#[cfg(unix)]
pub(crate) fn runtime_tree_handle_matches(identity: (u64, u64), handle: &File) -> io::Result<bool> {
    Ok(identity == handle_identity(handle)?)
}

#[cfg(windows)]
pub(crate) fn runtime_tree_handle_matches(identity: (u64, [u8; 16]), handle: &File) -> io::Result<bool> {
    Ok(identity == windows_identity::handle_file_id(handle)?)
}

/// Read one named backup or journal file relative to an already-open directory
/// capability. The file identity is captured before opening, verified on the
/// opened handle, and checked again after the bounded read. Other directory
/// entries are allowed, but the target itself must remain the same ordinary,
/// single-link regular file throughout the operation.
pub(crate) fn read_bounded_file_from_directory_handle(
    parent: &File,
    name: &OsStr,
    max_bytes: u64,
    max_entries: usize,
) -> io::Result<Vec<u8>> {
    if max_bytes == 0 || max_entries == 0 || !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "backup filename and read limits must be valid"));
    }
    let before_entries = enumerate_runtime_tree_entries(parent, max_entries)?;
    let before = before_entries.iter().find(|entry| entry.name == name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "backup file does not exist"))?;
    if !before.is_file || before.is_reparse {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "backup entry is not an ordinary regular file"));
    }
    let expected_identity = before.identity;
    let mut file = open_file_nofollow(parent, name)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata)
        || !has_single_hard_link(&file) || metadata.len() == 0 || metadata.len() > max_bytes
        || !runtime_tree_handle_matches(expected_identity, &file)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "backup file identity or metadata is unsafe"));
    }
    let expected_length = metadata.len();
    let capacity = usize::try_from(expected_length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "backup file size cannot be represented"))?;
    let mut contents = Vec::with_capacity(capacity);
    file.by_ref().take(max_bytes.saturating_add(1)).read_to_end(&mut contents)?;
    let after = file.metadata()?;
    if contents.is_empty() || contents.len() as u64 != expected_length
        || contents.len() as u64 > max_bytes || after.len() != expected_length
        || !after.is_file() || is_reparse_or_symlink(&after) || !has_single_hard_link(&file)
        || !runtime_tree_handle_matches(expected_identity, &file)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "backup file changed while being read"));
    }
    let after_entries = enumerate_runtime_tree_entries(parent, max_entries)?;
    let after = after_entries.iter().find(|entry| entry.name == name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "backup file disappeared while being read"))?;
    if !after.is_file || after.is_reparse || after.identity != expected_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "backup directory entry changed while being read"));
    }
    Ok(contents)
}

fn collect_runtime_tree_directory(
    parent: &File,
    relative_directory: &Path,
    depth: usize,
    max_depth: usize,
    max_entries: usize,
    count: &mut usize,
    files: &mut Vec<String>,
    directories: &mut Vec<String>,
) -> io::Result<()> {
    let entries = enumerate_runtime_tree_entries(parent, max_entries)?;
    for entry in entries {
        *count = count.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "runtime inventory entry count overflowed"))?;
        if *count > max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains too many entries"));
        }
        let name = entry.name;
        if !is_single_normal_component(&name) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains an invalid entry name"));
        }
        if entry.is_reparse {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains a symbolic link or reparse point"));
        }
        let relative = relative_directory.join(&name);
        let relative_text = relative.to_string_lossy().replace('/', "\\");
        if entry.is_directory {
            if depth >= max_depth {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle exceeds its directory depth limit"));
            }
            let child = open_dir_nofollow(parent, Path::new(&name))?;
            let child_metadata = child.metadata()?;
            if !child_metadata.is_dir()
                || is_reparse_or_symlink(&child_metadata)
                || !runtime_tree_handle_matches(entry.identity, &child)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle directory changed identity or is a reparse point"));
            }
            directories.push(relative_text);
            collect_runtime_tree_directory(&child, &relative, depth + 1, max_depth, max_entries, count, files, directories)?;
        } else if entry.is_file {
            let file = open_file_nofollow(parent, &name)?;
            let file_metadata = file.metadata()?;
            if !file_metadata.is_file()
                || is_reparse_or_symlink(&file_metadata)
                || !runtime_tree_handle_matches(entry.identity, &file)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle file changed identity or is a reparse point"));
            }
            files.push(relative_text);
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains a special or unsupported entry"));
        }
    }
    Ok(())
}

/// Open one regular file relative to an already-open directory handle on Windows.
///
/// This is deliberately test-only. NtCreateFile's RootDirectory pins the
/// parent handle; OBJ_DONT_REPARSE and FILE_OPEN_REPARSE_POINT prevent
/// reparse traversal. If the native call is unavailable or rejects the
/// request, the caller must fail closed rather than fall back to a path open.
#[cfg(windows)]
pub(crate) fn open_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_nofollow_with_share_access(
        parent, name, 0x0000_0001 | 0x0000_0002 | 0x0000_0004,
        0x0000_0001 | 0x0000_0080 | 0x0010_0000,
    )
}

/// Open a verified child with the additional right needed to restore its DACL.
#[cfg(windows)]
pub(crate) fn open_file_nofollow_for_security(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_nofollow_with_share_access(
        parent, name, 0x0000_0001 | 0x0000_0002 | 0x0000_0004,
        0x0000_0001 | 0x0000_0080 | 0x0002_0000 | 0x0004_0000 | 0x0010_0000,
    )
}

#[cfg(unix)]
pub(crate) fn open_file_nofollow_for_security(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_nofollow(parent, name)
}

/// Open a result file while denying concurrent write and delete sharing on
/// Windows. Once this handle is acquired, new writers/deleters cannot open the
/// file until it is closed; if a writer is already active, acquisition fails.
#[cfg(windows)]
fn open_result_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_nofollow_with_share_access(parent, name, 0x0000_0001, 0x0000_0001 | 0x0000_0080 | 0x0010_0000)
}

#[cfg(windows)]
fn open_file_nofollow_with_share_access(
    parent: &File,
    name: &OsStr,
    share_access: u32,
    desired_access: u32,
) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };

    if !is_single_normal_component(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file name must be exactly one normal path component",
        ));
    }

    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"));
    }
    let byte_len = wide.len().checked_mul(mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)]
    union IoStatusValue {
        status: i32,
        pointer: *mut std::ffi::c_void,
    }
    #[repr(C)]
    struct IoStatusBlock {
        status: IoStatusValue,
        information: usize,
    }

    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
    const OBJ_DONT_REPARSE: u32 = 0x0000_1000;
    const FILE_READ_DATA: u32 = 0x0000_0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_OPEN: u32 = 0x0000_0001;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    // Rust File performs synchronous ReadFile calls; request a synchronous
    // native handle rather than merely granting SYNCHRONIZE access.
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void,
            desired_access: u32,
            object_attributes: *const ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *const std::ffi::c_void,
            ea_length: u32,
        ) -> i32;
    }

    let mut unicode_name = UnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: wide.as_ptr() as *mut u16,
    };
    let object_attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status: IoStatusValue { status: 0 },
        information: 0,
    };
    let mut handle = ptr::null_mut();

    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            ptr::null(),
            0,
            share_access,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };

    // NT_SUCCESS(status) is status >= 0. Do not retry with weaker flags.
    if status < 0 || handle.is_null() {
        // Preserve the distinction between an absent directory entry and an
        // unsafe/denied open. Handle-relative bounded readers use NotFound to
        // detect the end of an append-only sequence; collapsing every NTSTATUS
        // to PermissionDenied would make a valid shorter history unreadable.
        let error_kind = match status as u32 {
            0xC000_0034 | 0xC000_003A | 0xC000_000F => io::ErrorKind::NotFound,
            _ => io::ErrorKind::PermissionDenied,
        };
        return Err(io::Error::new(
            error_kind,
            format!("handle-relative no-reparse file open failed (NTSTATUS 0x{:08X})", status as u32),
        ));
    }

    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x0000_0400 != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened entry is not an ordinary regular file",
        ));
    }

    Ok(file)
}

/// Open a result file without following links. Unix relies on the caller's
/// process-lifecycle gate; Windows additionally denies concurrent write/delete
/// sharing through the native handle's share mode.
#[cfg(unix)]
fn open_result_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_nofollow(parent, name)
}

/// Open one regular file relative to an already-open directory handle on Unix.
///
/// O_NOFOLLOW rejects a final symlink and openat pins resolution to the held
/// parent directory. O_NONBLOCK prevents a raced-in FIFO from blocking before
/// the opened handle can be checked as a regular file.
#[cfg(unix)]
pub(crate) fn open_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, io::{AsRawFd, FromRawFd}},
    };

    if !is_single_normal_component(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file name must be exactly one normal path component",
        ));
    }
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name contains NUL"))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened entry is not an ordinary regular file",
        ));
    }
    Ok(file)
}

/// Acquire a root directory handle only when its identity agrees with the
/// directory observed at the supplied path both before and after opening.
/// Callers must still authorize the selected root path before opening it.
pub(crate) fn open_verified_root_directory(path: &Path) -> io::Result<File> {
    open_verified_root_directory_with_hook(path, || Ok(()))
}

// The hook is private to this test-only module and lets the regression test
// replace the root after its identity has been captured but before it is opened.
fn open_verified_root_directory_with_hook<F>(path: &Path, before_open: F) -> io::Result<File>
where
    F: FnOnce() -> io::Result<()>,
{
    open_verified_root_directory_with_hooks(path, before_open, |_| Ok(()))
}

fn open_verified_root_directory_with_hooks<F, G>(
    path: &Path,
    before_open: F,
    after_component_open: G,
) -> io::Result<File>
where
    F: FnOnce() -> io::Result<()>,
    G: FnMut(&Path) -> io::Result<()>,
{
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project root must be an absolute path",
        ));
    }

    let before = std::fs::symlink_metadata(path)?;
    if !before.is_dir() || is_reparse_or_symlink(&before) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project root is not an ordinary directory",
        ));
    }

    // Capture the selected directory identity before walking from the volume
    // root. Every subsequent component is opened relative to a held parent
    // handle, so an ancestor cannot be silently re-resolved by an ambient path.
    let before_identity = path_identity(path)?;
    before_open()?;

    let handle = open_directory_from_filesystem_root_with_hook(path, after_component_open)?;
    let opened = handle.metadata()?;
    if !opened.is_dir()
        || is_reparse_or_symlink(&opened)
        || handle_identity(&handle)? != before_identity
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened project root does not match the directory observed before open",
        ));
    }

    let after = std::fs::symlink_metadata(path)?;
    if !after.is_dir()
        || is_reparse_or_symlink(&after)
        || path_identity(path)? != before_identity
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "project root changed while its directory handle was acquired",
        ));
    }

    Ok(handle)
}


/// Acquire a directory by starting at a filesystem root and opening each
/// normal component relative to the already-held parent handle. No ambient
/// absolute child path is used after the root anchor is opened.
fn open_directory_from_filesystem_root(path: &Path) -> io::Result<File> {
    open_directory_from_filesystem_root_with_hook(path, |_| Ok(()))
}

#[cfg(unix)]
fn open_directory_from_filesystem_root_with_hook<F>(
    path: &Path,
    mut after_component_open: F,
) -> io::Result<File>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let mut components = path.components();
    match components.next() {
        Some(Component::RootDir) => {}
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "project root must begin at the filesystem root",
            ));
        }
    }

    let mut current = cap_primitives::fs::open_ambient_dir(
        Path::new("/"),
        ambient_authority::ambient_authority(),
    )?;
    let mut current_path = PathBuf::from("/");
    for component in components {
        match component {
            Component::Normal(name) if is_single_normal_component(name) => {
                let child = open_dir_nofollow(&current, Path::new(name))?;
                let metadata = child.metadata()?;
                if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "opened ancestor is not an ordinary directory",
                    ));
                }
                current = child;
                current_path.push(name);
                after_component_open(&current_path)?;
            }
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "project root contains an unsupported path component",
                ));
            }
        }
    }
    Ok(current)
}

/// Windows drive paths are supported, including canonical \\?\C:\ paths.
/// UNC shares and device namespaces remain unsupported until their root-handle
/// semantics have separate review and physical acceptance coverage.
#[cfg(windows)]
fn open_directory_from_filesystem_root_with_hook<F>(
    path: &Path,
    mut after_component_open: F,
) -> io::Result<File>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    use std::path::Prefix;

    let mut components = path.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
        {
            prefix.as_os_str()
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "only local drive-letter project roots are supported",
            ));
        }
    };
    match components.next() {
        Some(Component::RootDir) => {}
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "project root must include a drive root",
            ));
        }
    }

    let mut volume_root = PathBuf::from(prefix);
    volume_root.push("\\");
    let mut current = cap_primitives::fs::open_ambient_dir(
        &volume_root,
        ambient_authority::ambient_authority(),
    )?;
    let mut current_path = volume_root.clone();

    for component in components {
        match component {
            Component::Normal(name) if is_single_normal_component(name) => {
                let child = open_dir_nofollow(&current, Path::new(name))?;
                let metadata = child.metadata()?;
                if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "opened ancestor is not an ordinary directory",
                    ));
                }
                current = child;
                current_path.push(name);
                after_component_open(&current_path)?;
            }
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "project root contains an unsupported path component",
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(windows)]
fn is_reparse_or_symlink(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || (metadata.file_attributes() & 0x400 != 0)
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn path_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn metadata_identity(metadata: &cap_primitives::fs::Metadata) -> io::Result<(u64, u64)> {
    use cap_primitives::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn handle_identity(handle: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = handle.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
mod windows_identity {
    use std::{io, path::Path, ptr};
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use std::fs::File;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime { low: u32, high: u32 }
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16, access: u32, share: u32, security: *mut std::ffi::c_void,
            creation: u32, flags: u32, template: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn GetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }

    const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    const OPEN_EXISTING: u32 = 3;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    fn identity(info: &ByHandleFileInformation) -> (u32, u64) {
        (
            info.volume_serial_number,
            ((info.file_index_high as u64) << 32) | info.file_index_low as u64,
        )
    }

    fn query(handle: *mut std::ffi::c_void) -> io::Result<(u32, u64)> {
        let mut info = ByHandleFileInformation::default();
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(identity(&info))
    }

    pub(super) fn path_identity(path: &Path) -> io::Result<(u32, u64)> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(), FILE_READ_ATTRIBUTES, SHARE_ALL, ptr::null_mut(),
                OPEN_EXISTING, BACKUP_SEMANTICS | OPEN_REPARSE_POINT, ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let result = query(handle);
        unsafe { CloseHandle(handle); }
        result
    }

    pub(super) fn handle_identity(handle: &File) -> io::Result<(u32, u64)> {
        query(handle.as_raw_handle())
    }


    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileId128 { identifier: [u8; 16] }

    #[repr(C)]
    #[derive(Default)]
    struct FileIdInfo { volume_serial_number: u64, file_id: FileId128 }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut std::ffi::c_void,
            information_class: i32,
            information: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    fn query_file_id(handle: *mut std::ffi::c_void) -> io::Result<(u64, [u8; 16])> {
        let mut info = FileIdInfo::default();
        if unsafe {
            GetFileInformationByHandleEx(
                handle, 18, &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<FileIdInfo>() as u32,
            )
        } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((info.volume_serial_number, info.file_id.identifier))
    }

    pub(super) fn handle_file_id(handle: &File) -> io::Result<(u64, [u8; 16])> {
        query_file_id(handle.as_raw_handle())
    }

    pub(super) fn enumerate_directory(
        directory: &File,
        max_entries: usize,
    ) -> io::Result<Vec<(std::ffi::OsString, u32, (u64, [u8; 16]))>> {
        use std::os::windows::ffi::OsStringExt;
        use std::collections::HashSet;

        if max_entries == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains too many entries"));
        }
        let (volume, _) = query_file_id(directory.as_raw_handle())?;
        let mut results = Vec::new();
        let mut names = HashSet::new();
        let mut restart = true;

        loop {
            let mut buffer = [0u64; 8192];
            let class = if restart { 20 } else { 19 }; // FileIdExtdDirectoryRestartInfo, then FileIdExtdDirectoryInfo.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    directory.as_raw_handle(),
                    class,
                    buffer.as_mut_ptr() as *mut std::ffi::c_void,
                    std::mem::size_of_val(&buffer) as u32,
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if error == 18 { break; } // ERROR_NO_MORE_FILES
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Windows file-ID directory enumeration failed (error {})", error),
                ));
            }
            restart = false;
            let bytes = std::mem::size_of_val(&buffer);
            let base = buffer.as_ptr() as *const u8;
            let mut offset = 0usize;
            let mut parsed = 0usize;
            loop {
                if offset.checked_add(88).map_or(true, |end| end > bytes) {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory identity record is truncated"));
                }
                let record = unsafe { base.add(offset) };
                let next = unsafe { std::ptr::read_unaligned(record as *const u32) } as usize;
                let attributes = unsafe { std::ptr::read_unaligned(record.add(56) as *const u32) };
                let name_length = unsafe { std::ptr::read_unaligned(record.add(60) as *const u32) } as usize;
                if name_length == 0 || name_length % 2 != 0 || name_length > 510
                    || offset.checked_add(88).and_then(|v| v.checked_add(name_length)).map_or(true, |end| end > bytes)
                {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory identity record has an invalid name"));
                }
                let mut file_id = [0u8; 16];
                unsafe { std::ptr::copy_nonoverlapping(record.add(72), file_id.as_mut_ptr(), 16); }
                let name_ptr = unsafe { record.add(88) as *const u16 };
                let wide = unsafe { std::slice::from_raw_parts(name_ptr, name_length / 2) };
                let name = std::ffi::OsString::from_wide(wide);
                if name != "." && name != ".." {
                    if !names.insert(name.clone()) {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory enumeration returned a duplicate name"));
                    }
                    if results.len() >= max_entries {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime bundle contains too many entries"));
                    }
                    results.push((name, attributes, (volume, file_id)));
                }
                parsed += 1;
                if next == 0 { break; }
                if next < 88 + name_length || next % 8 != 0
                    || offset.checked_add(next).map_or(true, |end| end > bytes)
                {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory identity record has an invalid next-entry offset"));
                }
                offset += next;
                if parsed > bytes / 88 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory identity record chain is invalid"));
                }
            }
            if parsed == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Windows directory enumeration returned no records"));
            }
        }
        Ok(results)
    }

}

#[cfg(windows)]
fn path_identity(path: &Path) -> io::Result<(u32, u64)> {
    windows_identity::path_identity(path)
}

#[cfg(windows)]
fn handle_identity(handle: &File) -> io::Result<(u32, u64)> {
    windows_identity::handle_identity(handle)
}

#[cfg(any(unix, windows))]
fn same_path_handle_identity(path: &Path, handle: &File) -> io::Result<bool> {
    Ok(path_identity(path)? == handle_identity(handle)?)
}

/// Confirm that an already-held ordinary directory handle still identifies
/// the directory currently named by this exact path. Symlinks/reparse points
/// are rejected rather than canonicalized through.
pub(crate) fn verified_directory_handle_matches_path(
    path: &Path,
    handle: &File,
) -> io::Result<bool> {
    if !path.is_absolute() {
        return Ok(false);
    }
    let path_metadata = std::fs::symlink_metadata(path)?;
    let handle_metadata = handle.metadata()?;
    if !path_metadata.is_dir()
        || is_reparse_or_symlink(&path_metadata)
        || !handle_metadata.is_dir()
        || is_reparse_or_symlink(&handle_metadata)
    {
        return Ok(false);
    }
    same_path_handle_identity(path, handle)
}

/// Return the stable filesystem identity of a verified directory path and its
/// held handle. The pair is volume/device plus file ID/inode, normalized across
/// Windows and Unix. A mismatch is an error rather than a weak path-only proof.
pub(crate) fn verified_directory_identity(
    path: &Path,
    handle: &File,
) -> io::Result<(u64, u64)> {
    if !path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "directory path must be absolute"));
    }
    let path_metadata = std::fs::symlink_metadata(path)?;
    let handle_metadata = handle.metadata()?;
    if !path_metadata.is_dir()
        || is_reparse_or_symlink(&path_metadata)
        || !handle_metadata.is_dir()
        || is_reparse_or_symlink(&handle_metadata)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "directory path or handle is not an ordinary directory"));
    }
    #[cfg(unix)]
    {
        let path_id = path_identity(path)?;
        let handle_id = handle_identity(handle)?;
        if path_id != handle_id {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "directory path no longer identifies the held handle"));
        }
        Ok(path_id)
    }
    #[cfg(windows)]
    {
        let path_id = path_identity(path)?;
        let handle_id = handle_identity(handle)?;
        if path_id != handle_id {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "directory path no longer identifies the held handle"));
        }
        Ok((path_id.0 as u64, path_id.1))
    }
}

fn same_handle_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(handle_identity(left)? == handle_identity(right)?)
}

/// Read one bounded file beneath an already-open runtime root. Every directory
/// is opened relative to its held parent handle without following reparse points;
/// the final file is opened relative to its held parent with no-follow semantics.
/// The returned bytes are the exact bounded content that the caller must digest.
pub(crate) fn read_bounded_relative_file(
    root: &File,
    relative_path: &str,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    if max_bytes == 0 || relative_path.is_empty() || relative_path.len() > 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime artifact path or size limit is invalid"));
    }
    let components = relative_path.split('\\').collect::<Vec<_>>();
    if components.is_empty() || components.len() > 32 || components.iter().any(|part| {
        part.is_empty() || *part == "." || *part == ".."
            || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime artifact path is not normalized"));
    }

    let mut parent = root.try_clone()?;
    for component in &components[..components.len() - 1] {
        let child = open_dir_nofollow(&parent, Path::new(component))?;
        let metadata = child.metadata()?;
        if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime artifact parent is not an ordinary directory"));
        }
        parent = child;
    }

    let file_name = OsStr::new(components[components.len() - 1]);
    let mut file = open_file_nofollow(&parent, file_name)?;
    let before = file.metadata()?;
    if !before.is_file() || is_reparse_or_symlink(&before) || before.len() == 0 || before.len() > max_bytes {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime artifact is empty, non-regular, or oversized"));
    }
    let expected_length = before.len();
    let capacity = usize::try_from(expected_length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "runtime artifact size cannot be represented"))?;
    let mut contents = Vec::with_capacity(capacity);
    file.by_ref().take(max_bytes.saturating_add(1)).read_to_end(&mut contents)?;
    let after = file.metadata()?;
    if contents.is_empty() || contents.len() as u64 > max_bytes
        || contents.len() as u64 != expected_length || after.len() != expected_length
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime artifact changed or exceeded its limit while being read"));
    }
    Ok(contents)
}

/// Read exactly one bounded regular file from a directory whose identity is
/// pinned by a verified handle. Enumeration and opening are relative to that
/// held handle; no ambient child path is used for the file read.
pub(crate) fn read_single_bounded_file_from_directory(
    directory_path: &Path,
    expected_name: &OsStr,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    if max_bytes == 0 || !is_single_normal_component(expected_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "result file name and byte limit must be valid",
        ));
    }

    let directory = open_verified_root_directory(directory_path)?;
    read_single_bounded_file_from_handle(directory_path, &directory, expected_name, max_bytes)
}

/// The caller supplies the already-open result-directory capability. The path
/// is used only to prove that it still names that exact held directory before
/// and after enumeration/read; all child operations remain handle-relative.
fn read_single_bounded_file_from_handle(
    directory_path: &Path,
    directory: &File,
    expected_name: &OsStr,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    let initial_identity = verified_directory_identity(directory_path, directory)?;
    let mut found: Option<Vec<u8>> = None;
    // Keep the opened file capability alive through the final inventory and
    // link-count checks; a pathname-only check would lose that identity.
    let mut opened_file: Option<File> = None;
    for entry in read_dir(directory, Path::new("."))? {
        let entry = entry?;
        let name = entry.file_name();
        if name != expected_name || found.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "result directory must contain exactly the expected file",
            ));
        }
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "result entry is not an ordinary regular file",
            ));
        }

        let mut file = open_result_file_nofollow(directory, &name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse_or_symlink(&metadata)
            || !has_single_hard_link(&file)
            || metadata.len() == 0 || metadata.len() > max_bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "result file is empty, non-regular, or exceeds its byte limit",
            ));
        }
        let expected_length = metadata.len();
        let capacity = usize::try_from(expected_length).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "result file size cannot be represented")
        })?;
        let mut contents = Vec::with_capacity(capacity);
        file.by_ref().take(max_bytes.saturating_add(1)).read_to_end(&mut contents)?;
        let after = file.metadata()?;
        if contents.is_empty()
            || contents.len() as u64 > max_bytes
            || contents.len() as u64 != expected_length
            || after.len() != expected_length
            || !has_single_hard_link(&file)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "result file changed or exceeded its byte limit while being read",
            ));
        }
        found = Some(contents);
        opened_file = Some(file);
    }

    let contents = found.ok_or_else(|| io::Error::new(
        io::ErrorKind::NotFound,
        "result directory does not contain the expected file",
    ))?;

    // Recheck the named directory and exact one-file inventory after reading.
    // A concurrent path replacement or unexpected entry must invalidate the
    // read rather than allowing a result from a detached directory to escape.
    let final_identity = verified_directory_identity(directory_path, directory)?;
    if final_identity != initial_identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "result directory identity changed while reading",
        ));
    }
    let mut final_entries = read_dir(directory, Path::new("."))?;
    let first = final_entries.next().transpose()?;
    let second = final_entries.next().transpose()?;
    if first.as_ref().map(|entry| entry.file_name()).as_deref() != Some(expected_name)
        || second.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "result directory inventory changed while reading",
        ));
    }
    let first = first.expect("the first result entry was checked above");
    let first_type = first.file_type()?;
    if !first_type.is_file() || first_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "result entry is no longer an ordinary regular file",
        ));
    }
    let final_file_metadata = opened_file
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "result file handle was lost"))?
        .metadata()?;
    if !final_file_metadata.is_file()
        || is_reparse_or_symlink(&final_file_metadata)
        || !has_single_hard_link(opened_file.as_ref().expect("checked above"))
        || final_file_metadata.len() != contents.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "result file identity, link count, or size changed while being read",
        ));
    }
    Ok(contents)
}

/// Guest-written result files must have exactly one filesystem link. This
/// prevents a result entry from aliasing another host-visible file through a
/// hard link, even though hard links are not symlinks or reparse points.
pub(crate) fn has_single_hard_link(file: &File) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        file.metadata().map(|metadata| metadata.nlink() == 1).unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
        };

        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // Query the link count from the exact open file handle. The Windows
        // MetadataExt::number_of_links API remains unstable on our toolchain.
        unsafe {
            GetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                &mut information,
            ) != 0 && information.nNumberOfLinks == 1
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        false
    }
}

fn is_single_normal_component(name: &OsStr) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn walk_directory<F>(
    parent: &File,
    relative_directory: &Path,
    depth: usize,
    max_depth: usize,
    max_entries: usize,
    entries_scanned: &mut usize,
    files: &mut Vec<PathBuf>,
    before_directory_open: &mut F,
) -> io::Result<()>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    for entry in read_dir(parent, Path::new("."))? {
        *entries_scanned = entries_scanned.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "candidate scan entry count overflowed")
        })?;
        if *entries_scanned > max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "candidate scan exceeded its entry limit"));
        }
        let entry = entry?;
        let name = entry.file_name();

        // Directory entries must be a single filename. Never feed an absolute,
        // prefixed, parent, or multi-component path into a capability operation.
        if !is_single_normal_component(&name) {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }

        let relative_path = relative_directory.join(&name);
        if file_type.is_dir() {
            if depth >= max_depth {
                continue;
            }

            // The hook exists only to make the enumerate/replace/open race
            // deterministic in tests. Production traversal uses a no-op hook.
            before_directory_open(&relative_path)?;

            // Crucially, open relative to the held parent handle, not an
            // ambient absolute path. A directory swapped for a symlink or
            // junction after enumeration must fail closed here.
            let child = open_dir_nofollow(parent, Path::new(&name))?;
            if !child.metadata()?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "directory entry changed type during safe traversal",
                ));
            }

            walk_directory(
                &child,
                &relative_path,
                depth + 1,
                max_depth,
                max_entries,
                entries_scanned,
                files,
                before_directory_open,
            )?;
        } else if file_type.is_file() {
            // This is only a candidate name. No file content is opened here.
            files.push(relative_path);
        }
    }

    Ok(())
}


/// Fixed limits for the experimental in-memory snapshot reader.
#[derive(Clone, Copy, Debug)]
struct BoundedSnapshotLimits {
    max_depth: usize,
    max_files: usize,
    /// Maximum directory entries inspected, including ignored files and directories.
    max_entries: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
}

/// One bounded file payload captured through a held parent directory handle.
#[derive(Debug, PartialEq, Eq)]
struct BoundedSnapshotFile {
    relative_path: PathBuf,
    contents: Vec<u8>,
}

/// Result of the source-side prototype. Payloads remain in memory; this
/// deliberately does not create destination files or claim destination safety.
#[derive(Debug, PartialEq, Eq)]
struct BoundedSnapshotContents {
    files: Vec<BoundedSnapshotFile>,
    bytes: u64,
    truncated: bool,
}

#[allow(dead_code)]
fn collect_bounded_snapshot_contents<AF, AD>(
    root: &File,
    limits: BoundedSnapshotLimits,
    include_file: AF,
    exclude_directory: AD,
) -> io::Result<BoundedSnapshotContents>
where
    AF: Fn(&str) -> bool,
    AD: Fn(&str) -> bool,
{
    collect_bounded_snapshot_contents_with_hook(
        root,
        limits,
        &include_file,
        &exclude_directory,
        |_| Ok(()),
    )
}

fn collect_bounded_snapshot_contents_with_hook<AF, AD, H>(
    root: &File,
    limits: BoundedSnapshotLimits,
    include_file: &AF,
    exclude_directory: &AD,
    mut before_file_open: H,
) -> io::Result<BoundedSnapshotContents>
where
    AF: Fn(&str) -> bool,
    AD: Fn(&str) -> bool,
    H: FnMut(&Path) -> io::Result<()>,
{
    if limits.max_files == 0 || limits.max_entries == 0
        || limits.max_file_bytes == 0 || limits.max_total_bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot limits must be non-zero",
        ));
    }
    let root_metadata = root.metadata()?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot root handle is not an ordinary directory",
        ));
    }

    let mut result = BoundedSnapshotContents {
        files: Vec::new(),
        bytes: 0,
        truncated: false,
    };
    let mut entries_scanned = 0usize;
    collect_bounded_directory(
        root,
        Path::new(""),
        0,
        limits,
        include_file,
        exclude_directory,
        &mut before_file_open,
        &mut entries_scanned,
        &mut result,
    )?;
    Ok(result)
}

fn collect_bounded_directory<AF, AD, H>(
    parent: &File,
    relative_directory: &Path,
    depth: usize,
    limits: BoundedSnapshotLimits,
    include_file: &AF,
    exclude_directory: &AD,
    before_file_open: &mut H,
    entries_scanned: &mut usize,
    result: &mut BoundedSnapshotContents,
) -> io::Result<()>
where
    AF: Fn(&str) -> bool,
    AD: Fn(&str) -> bool,
    H: FnMut(&Path) -> io::Result<()>,
{
    if depth > limits.max_depth {
        result.truncated = true;
        return Ok(());
    }

    // Capture native per-entry identities while enumerating. A no-follow
    // open alone prevents link traversal, but it does not prove that an
    // ordinary file/directory opened moments later is the same object that was
    // inspected in this directory listing.
    let entries = match enumerate_runtime_tree_entries(parent, limits.max_entries) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            result.truncated = true;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
        if result.files.len() >= limits.max_files || result.bytes >= limits.max_total_bytes {
            result.truncated = true;
            break;
        }
        *entries_scanned = entries_scanned.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "snapshot directory entry count overflowed")
        })?;
        if *entries_scanned > limits.max_entries {
            result.truncated = true;
            break;
        }
        let name = entry.name;
        if !is_single_normal_component(&name) {
            result.truncated = true;
            continue;
        }
        let Some(name_text) = name.to_str() else {
            result.truncated = true;
            continue;
        };
        if entry.is_reparse {
            continue;
        }

        let relative_path = relative_directory.join(&name);
        if entry.is_directory {
            if exclude_directory(name_text) {
                continue;
            }
            if depth >= limits.max_depth {
                result.truncated = true;
                continue;
            }
            let child = open_dir_nofollow(parent, Path::new(&name))?;
            let child_metadata = child.metadata()?;
            if !child_metadata.is_dir()
                || is_reparse_or_symlink(&child_metadata)
                || !runtime_tree_handle_matches(entry.identity, &child)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "snapshot source directory changed identity or is not an ordinary directory",
                ));
            }
            collect_bounded_directory(
                &child, &relative_path, depth + 1, limits, include_file,
                exclude_directory, before_file_open, entries_scanned, result,
            )?;
            if result.truncated
                && (result.files.len() >= limits.max_files
                    || result.bytes >= limits.max_total_bytes
                    || *entries_scanned >= limits.max_entries)
            {
                break;
            }
            continue;
        }

        if !entry.is_file || !include_file(name_text) {
            continue;
        }
        before_file_open(&relative_path)?;
        let mut file = match open_file_nofollow(parent, &name) {
            Ok(file) => file,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        let before = file.metadata()?;
        if !before.is_file()
            || is_reparse_or_symlink(&before)
            || !runtime_tree_handle_matches(entry.identity, &file)?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "opened snapshot source changed identity or is not an ordinary regular file",
            ));
        }
        // A hard link can expose content whose other name is outside the
        // selected project. Reject it rather than silently including it.
        if !has_single_hard_link(&file) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot source file has multiple filesystem links",
            ));
        }
        if before.len() == 0 || before.len() > limits.max_file_bytes {
            continue;
        }

        let remaining = limits.max_total_bytes.saturating_sub(result.bytes);
        let read_limit = limits.max_file_bytes.min(remaining);
        if read_limit == 0 {
            result.truncated = true;
            break;
        }
        let mut contents = Vec::with_capacity(
            before.len().min(read_limit).min(usize::MAX as u64) as usize
        );
        file.by_ref().take(read_limit.saturating_add(1)).read_to_end(&mut contents)?;
        if contents.is_empty() || contents.len() as u64 > read_limit {
            result.truncated = true;
            break;
        }

        // Detect ordinary concurrent size/time changes on the opened object.
        // This is not an atomic filesystem snapshot; it is a conservative
        // consistency check around a bounded read from the same file handle.
        let after = file.metadata()?;
        if !after.is_file()
            || is_reparse_or_symlink(&after)
            || !has_single_hard_link(&file)
            || after.len() != before.len()
            || after.modified()? != before.modified()?
            || contents.len() as u64 != before.len()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot source identity, link count, or size changed while being read",
            ));
        }

        result.bytes = result.bytes.saturating_add(contents.len() as u64);
        result.files.push(BoundedSnapshotFile {
            relative_path,
            contents,
        });
    }
    Ok(())
}


/// Create a new entry relative to an already-held destination directory handle.
/// This is a test-only prototype: no ambient destination path is used after
/// the workspace handle is acquired.
#[cfg(unix)]
pub(crate) fn create_destination_entry(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    use std::{ffi::CString, os::unix::{ffi::OsStrExt, io::{AsRawFd, FromRawFd}}};
    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "destination name must be one normal component"));
    }
    let relative_name = PathBuf::from(name);
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL"))?;
    if directory {
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 { return Err(io::Error::last_os_error()); }
        // Open the newly-created directory with a normal read-capable
        // directory descriptor. cap-primitives' traversal handle may be
        // intentionally unsuitable for fchmod, while this descriptor is
        // needed by callers that must apply permissions to this exact object.
        // O_NOFOLLOW prevents a raced symlink from being accepted.
        let directory_fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if directory_fd < 0 { return Err(io::Error::last_os_error()); }
        let directory = unsafe { File::from_raw_fd(directory_fd) };
        // mkdirat and openat are separate syscalls. Confirm the name still
        // resolves (without following links) to the exact directory handle.
        use std::os::unix::fs::MetadataExt;
        let opened = directory.metadata()?;
        let mut current: libc::stat = unsafe { std::mem::zeroed() };
        let stat_result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                &mut current,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if stat_result != 0 { return Err(io::Error::last_os_error()); }
        if !opened.is_dir()
            || opened.dev() != current.st_dev as u64
            || opened.ino() != current.st_ino as u64
            || (current.st_mode & libc::S_IFMT) != libc::S_IFDIR
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "new destination directory changed during handle acquisition"));
        }
        return Ok(directory);
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(), name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC
                | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "created destination is not a regular file"));
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn create_destination_entry(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };
    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "destination name must be one normal component"));
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid destination name"));
    }
    let byte_len = wide.len().checked_mul(2).and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination name is too long"))?;
    #[repr(C)] struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)] struct ObjectAttributes {
        length: u32, root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString, attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)] union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)] struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
            object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64, file_attributes: u32, share_access: u32,
            create_disposition: u32, create_options: u32,
            ea_buffer: *const std::ffi::c_void, ea_length: u32,
        ) -> i32;
    }
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const OBJ_DONT_REPARSE: u32 = 0x1000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    // Coding Assistant replacement files receive the original Unix-style
    // permissions through this handle before installation. Keep the Windows
    // handle capable of applying file attributes as well as writing data.
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_WRITE_DATA: u32 = 0x2;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_ADD_FILE: u32 = 0x2;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
    const FILE_TRAVERSE: u32 = 0x20;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_CREATE: u32 = 2;
    const FILE_DIRECTORY_FILE: u32 = 0x1;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    let mut unicode_name = UnicodeString {
        length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut handle = ptr::null_mut();
    let access = if directory {
        FILE_LIST_DIRECTORY | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE
    } else {
        FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES | SYNCHRONIZE
    };
    let options = (if directory { FILE_DIRECTORY_FILE } else { FILE_NON_DIRECTORY_FILE })
        | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
    let status = unsafe {
        NtCreateFile(
            &mut handle, access, &attributes, &mut io_status, ptr::null(),
            if directory { 0x10 } else { 0x80 },
            FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_CREATE, options, ptr::null(), 0,
        )
    };
    if status < 0 || handle.is_null() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists,
            format!("exclusive handle-relative destination creation failed (NTSTATUS 0x{:08X})", status as u32)));
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if (directory && (!metadata.is_dir() || (metadata.file_attributes() & 0x400 != 0)))
        || (!directory && (!metadata.is_file() || (metadata.file_attributes() & 0x400 != 0)))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "created destination has an unsafe type"));
    }
    Ok(file)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotCleanupManifest {
    schema_version: u32,
    snapshot_id: String,
    owner: String,
    directories: Vec<String>,
    files: Vec<SnapshotCleanupFile>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotCleanupFile {
    path: String,
    size: u64,
}

/// In-memory authority captured by cleanup preflight. These handles keep the
/// exact objects observed during native enumeration alive until cleanup ends;
/// serialized manifest paths alone never authorize deletion.
struct SnapshotCleanupPlan {
    manifest: SnapshotCleanupManifest,
    file_handles: std::collections::BTreeMap<String, File>,
    directory_handles: std::collections::BTreeMap<String, File>,
    marker_handle: File,
}

fn portable_manifest_path(path: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest path contains a non-normal component"));
        };
        let value = value.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "manifest path is not UTF-8"))?;
        if value.is_empty() || value == "." || value == ".." || value.contains('/') || value.contains('\\') || value.contains(':') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest path contains an ambiguous component"));
        }
        parts.push(value);
    }
    if parts.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest path is empty"));
    }
    Ok(parts.join("/"))
}

/// Build a version-2 inventory from the exact payload accepted by the writer.
/// This inventory is not, by itself, authorization to delete filesystem entries.
fn build_snapshot_cleanup_manifest(snapshot_id: &str, contents: &BoundedSnapshotContents, limits: BoundedSnapshotLimits) -> io::Result<SnapshotCleanupManifest> {
    use std::collections::BTreeSet;
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|b| b.is_ascii_hexdigit()) || contents.truncated || contents.files.is_empty() || contents.files.len() > limits.max_files {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot inventory an invalid or incomplete snapshot"));
    }
    let mut directories = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut manifest_files = Vec::with_capacity(contents.files.len());
    let mut total = 0u64;
    for item in &contents.files {
        let path = portable_manifest_path(&item.relative_path)?;
        if path == ".friday-snapshot-owner.json" || item.contents.is_empty() || item.contents.len() as u64 > limits.max_file_bytes || !files.insert(path.clone()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest contains a reserved/duplicate path or invalid file size"));
        }
        let components: Vec<&str> = path.split('/').collect();
        if components.len().saturating_sub(1) > limits.max_depth {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest path exceeds the depth limit"));
        }
        for index in 1..components.len() { directories.insert(components[..index].join("/")); }
        total = total.checked_add(item.contents.len() as u64).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "manifest byte count overflow"))?;
        manifest_files.push(SnapshotCleanupFile { path, size: item.contents.len() as u64 });
    }
    if total != contents.bytes || total > limits.max_total_bytes || directories.iter().any(|directory| files.contains(directory)) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest inventory conflicts with snapshot contents or limits"));
    }
    Ok(SnapshotCleanupManifest { schema_version: 2, snapshot_id: snapshot_id.to_string(), owner: "FRIDAY-CodingAssistant".to_string(), directories: directories.into_iter().collect(), files: manifest_files })
}

/// Validate versioned inventory before a future cleanup preflight. No I/O mutation occurs here.
fn validate_snapshot_cleanup_manifest(bytes: &[u8], expected_snapshot_id: &str, limits: BoundedSnapshotLimits) -> io::Result<SnapshotCleanupManifest> {
    use std::collections::BTreeSet;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest is empty or oversized"));
    }
    let manifest: SnapshotCleanupManifest = serde_json::from_slice(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest is malformed or contains unsupported fields"))?;
    if manifest.schema_version != 2 || manifest.snapshot_id != expected_snapshot_id || manifest.snapshot_id.len() != 32 || !manifest.snapshot_id.bytes().all(|b| b.is_ascii_hexdigit()) || manifest.owner != "FRIDAY-CodingAssistant" || manifest.files.is_empty() || manifest.files.len() > limits.max_files || manifest.directories.len() > limits.max_files.saturating_mul(limits.max_depth.max(1)) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest identity, version, or limits are invalid"));
    }
    let mut expected_directories = BTreeSet::new();
    let mut seen_files = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        if file.size == 0 || file.size > limits.max_file_bytes || !seen_files.insert(file.path.clone()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest has a duplicate path or invalid file size"));
        }
        let path = Path::new(&file.path);
        if portable_manifest_path(path)? != file.path || file.path == ".friday-snapshot-owner.json" {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest contains an unsafe or reserved path"));
        }
        let components: Vec<&str> = file.path.split('/').collect();
        if components.len().saturating_sub(1) > limits.max_depth {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest exceeds the depth limit"));
        }
        for index in 1..components.len() { expected_directories.insert(components[..index].join("/")); }
        total = total.checked_add(file.size).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest byte count overflow"))?;
    }
    let listed_directories: BTreeSet<String> = manifest.directories.iter().cloned().collect();
    if listed_directories.len() != manifest.directories.len() || listed_directories != expected_directories || listed_directories.iter().any(|directory| seen_files.contains(directory)) || total > limits.max_total_bytes {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup manifest directory inventory or total size is inconsistent"));
    }
    Ok(manifest)
}

/// Test-only prototype for removing one inventoried regular file relative to a held parent.
///
/// Unix unlinkat is name-based rather than handle-based. This helper therefore
/// reopens and compares the entry immediately before unlinking, but it cannot
/// eliminate a hostile same-user rename race between that check and unlinkat.
/// It must remain disconnected from production cleanup until the private
/// workspace threat model and directory ownership/permissions are enforced.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn remove_verified_snapshot_file(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
) -> io::Result<()> {
    remove_verified_snapshot_file_bound(parent, name, expected_size, None)
}

#[cfg(unix)]
fn remove_verified_snapshot_file_bound(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
    expected_file: Option<&File>,
) -> io::Result<()> {
    use std::os::unix::{ffi::OsStrExt, io::AsRawFd};

    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot cleanup requires one normal filename component"));
    }
    let opened = open_file_nofollow(parent, name)?;
    let metadata = opened.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file does not match its expected type or size"));
    }
    if let Some(expected) = expected_file {
        if !same_handle_identity(expected, &opened)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file no longer matches the identity captured during preflight"));
        }
    }
    let current = open_file_nofollow(parent, name)?;
    if !same_handle_identity(&opened, &current)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file changed before cleanup"));
    }
    let name_c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "snapshot filename contains NUL"))?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), 0) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Remove one inventoried regular file through the file handle opened relative
/// to the held parent. The handle does not grant FILE_SHARE_DELETE, preventing
/// a concurrent rename/replacement while the delete disposition is applied.
/// If the native open or disposition API is unavailable, fail closed.
#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn remove_verified_snapshot_file(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
) -> io::Result<()> {
    remove_verified_snapshot_file_bound(parent, name, expected_size, None)
}

#[cfg(windows)]
fn remove_verified_snapshot_file_bound(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
    expected_file: Option<&File>,
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };

    // The ownership marker is intentionally deleted only after every
    // inventoried payload entry and directory has been removed and the root
    // has been rechecked to contain exactly this marker.
    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot cleanup requires one normal filename component"));
    }

    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot filename"));
    }
    let byte_len = wide.len().checked_mul(mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot filename is too long"))?;

    #[repr(C)]
    struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)]
    union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)]
    struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[repr(C)]
    struct FileDispositionInfoEx { flags: u32 }

    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
    const OBJ_DONT_REPARSE: u32 = 0x0000_1000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_OPEN: u32 = 0x0000_0001;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x0000_0001;

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void,
            desired_access: u32,
            object_attributes: *const ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *const std::ffi::c_void,
            ea_length: u32,
        ) -> i32;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryDirectoryFile(
            file_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut std::ffi::c_void,
            length: u32,
            information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information_class: i32,
            information: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    let mut unicode_name = UnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle,
            DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes,
            &mut io_status,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        // STATUS_OBJECT_NAME_NOT_FOUND is the expected result when a prior
        // cleanup attempt already removed this inventoried entry. Preserve
        // that distinction so the retry-safe orchestrator can continue.
        let kind = if status as u32 == 0xC000_0034 {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::PermissionDenied
        };
        return Err(io::Error::new(
            kind,
            format!("handle-relative delete open failed (NTSTATUS 0x{:08X})", status as u32),
        ));
    }

    let file = unsafe { File::from_raw_handle(raw_handle) };
    if let Some(expected) = expected_file {
        if !same_handle_identity(expected, &file)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot delete handle does not match the identity captured during preflight"));
        }
    }
    let metadata = file.metadata()?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x0000_0400 != 0)
        || metadata.len() != expected_size
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file does not match its expected type or size"));
    }

    let disposition = FileDispositionInfoEx { flags: FILE_DISPOSITION_FLAG_DELETE };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_DISPOSITION_INFO_EX_CLASS,
            &disposition as *const _ as *const std::ffi::c_void,
            mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    Ok(())
}


/// Unix runtime-file deletion is deliberately unavailable until FRIDAY has a
/// race-safe, handle-bound unlink primitive and a reviewed same-user threat
/// model. Never fall back to name-based unlinkat for authenticated runtime data.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_file(
    _parent: &File,
    _name: &OsStr,
    _expected_size: u64,
    _expected_sha256: &str,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated runtime cleanup is not supported on this platform",
    ))
}

#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_file(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
    expected_sha256: &str,
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };

    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime cleanup requires one normal filename component"));
    }

    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid runtime filename"));
    }
    let byte_len = wide.len().checked_mul(mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime filename is too long"))?;

    #[repr(C)]
    struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)]
    union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)]
    struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[repr(C)]
    struct FileDispositionInfoEx { flags: u32 }

    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
    const OBJ_DONT_REPARSE: u32 = 0x0000_1000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_READ_DATA: u32 = 0x0000_0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_OPEN: u32 = 0x0000_0001;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x0000_0001;

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void,
            desired_access: u32,
            object_attributes: *const ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *const std::ffi::c_void,
            ea_length: u32,
        ) -> i32;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryDirectoryFile(
            file_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut std::ffi::c_void,
            length: u32,
            information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void,
            information_class: i32,
            information: *const std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    let mut unicode_name = UnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle,
            DELETE | FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes,
            &mut io_status,
            ptr::null(),
            0,
            FILE_SHARE_READ,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        // STATUS_OBJECT_NAME_NOT_FOUND is the expected result when a prior
        // cleanup attempt already removed this inventoried entry. Preserve
        // that distinction so the retry-safe orchestrator can continue.
        let kind = if status as u32 == 0xC000_0034 {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::PermissionDenied
        };
        return Err(io::Error::new(
            kind,
            format!("handle-relative runtime delete open failed (NTSTATUS 0x{:08X})", status as u32),
        ));
    }

    let file = unsafe { File::from_raw_handle(raw_handle) };
    let metadata = file.metadata()?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x0000_0400 != 0)
        || metadata.len() != expected_size
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime file does not match its expected type or size"));
    }

    // The handle denies concurrent write/delete sharing. Hash the exact bytes
    // through this same handle before applying its delete disposition.
    if expected_sha256.len() != 64
        || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime cleanup requires an exact SHA-256 digest"));
    }
    use std::io::Read;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut remaining = expected_size;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let chunk_len = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "runtime cleanup size is not representable"))?;
        let read = (&file).read(&mut buffer[..chunk_len])?;
        if read == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "runtime payload ended before its authenticated size"));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let mut extra = [0u8; 1];
    if (&file).read(&mut extra)? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime payload exceeds its authenticated size"));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime payload digest does not match its authenticated inventory"));
    }

    let disposition = FileDispositionInfoEx { flags: FILE_DISPOSITION_FLAG_DELETE };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FILE_DISPOSITION_INFO_EX_CLASS,
            &disposition as *const _ as *const std::ffi::c_void,
            mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    Ok(())
}


/// Remove only FRIDAY's authenticated runtime ownership marker. The caller
/// supplies the expected size and SHA-256 from the freshly verified manifest;
/// this wrapper fixes the filename and delegates same-handle content
/// verification to the runtime-file primitive. Ordering (marker last) remains
/// the responsibility of the higher-level immutable cleanup plan.
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_owner_marker(
    parent: &File,
    name: &OsStr,
    expected_size: u64,
    expected_sha256: &str,
) -> io::Result<()> {
    if name != OsStr::new(".friday-runtime-owner.json")
        || expected_size == 0
        || expected_size > 16 * 1024
        || expected_sha256.len() != 64
        || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime owner-marker cleanup requires its exact reserved name and authenticated bounded digest",
        ));
    }
    remove_verified_runtime_file(parent, name, expected_size, expected_sha256)
}

/// Remove one verified empty directory relative to a held parent directory handle.
///
/// Unix rmdir via unlinkat is name-based and retains a same-user rename race
/// after the final identity check. Windows applies delete disposition to the
/// opened directory handle while denying write/delete sharing. This is a
/// test-only primitive and must not be used as production cleanup authority.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn remove_verified_empty_snapshot_directory(
    parent: &File,
    name: &OsStr,
    expected_directory: Option<&File>,
) -> io::Result<()> {
    use std::{ffi::CString, os::unix::{ffi::OsStrExt, io::AsRawFd}};
    if !is_single_normal_component(name)
        || name == OsStr::new(".friday-snapshot-owner.json")
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot cleanup requires a non-marker directory component"));
    }
    let relative = PathBuf::from(name);
    let opened = open_dir_nofollow(parent, &relative)?;
    let opened_metadata = opened.metadata()?;
    if !opened_metadata.is_dir() || is_reparse_or_symlink(&opened_metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot entry is not an ordinary directory"));
    }
    if let Some(expected) = expected_directory {
        if !same_handle_identity(expected, &opened)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot directory no longer matches the inventoried handle"));
        }
    }
    if read_dir(&opened, Path::new("."))?.next().transpose()?.is_some() {
        return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "snapshot directory is not empty"));
    }
    let current = open_dir_nofollow(parent, &relative)?;
    if !same_handle_identity(&opened, &current)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot directory changed before cleanup"));
    }
    let name_c = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "snapshot directory name contains NUL"))?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), libc::AT_REMOVEDIR) };
    if result != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn remove_verified_empty_snapshot_directory(
    parent: &File,
    name: &OsStr,
    expected_directory: Option<&File>,
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };
    if !is_single_normal_component(name)
        || name == OsStr::new(".friday-snapshot-owner.json")
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot cleanup requires a non-marker directory component"));
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot directory name"));
    }
    let byte_len = wide.len().checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot directory name is too long"))?;
    #[repr(C)] struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)] struct ObjectAttributes {
        length: u32, root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString, attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)] union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)] struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[repr(C)] struct FileDispositionInfoEx { flags: u32 }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
            object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64, file_attributes: u32, share_access: u32,
            create_disposition: u32, create_options: u32,
            ea_buffer: *const std::ffi::c_void, ea_length: u32,
        ) -> i32;
        fn NtQueryDirectoryFile(
            file_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut std::ffi::c_void,
            length: u32,
            information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void, information_class: i32,
            information: *const std::ffi::c_void, size: u32,
        ) -> i32;
    }
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const OBJ_DONT_REPARSE: u32 = 0x1000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_OPEN: u32 = 1;
    const FILE_DIRECTORY_FILE: u32 = 1;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 1;

    let mut unicode_name = UnicodeString {
        length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle, DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes, &mut io_status, ptr::null(), 0,
            FILE_SHARE_READ, FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(), 0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            format!("handle-relative directory delete open failed (NTSTATUS 0x{:08X})", status as u32)));
    }
    let directory = unsafe { File::from_raw_handle(raw_handle) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x400 != 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot entry is not an ordinary directory"));
    }
    if let Some(expected) = expected_directory {
        if !same_handle_identity(expected, &directory)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot directory no longer matches the inventoried handle"));
        }
    }
    // Enumerate through this exact restrictive handle. cap_std::read_dir
    // opens a second handle; with FILE_SHARE_READ only that second open gets
    // ERROR_SHARING_VIOLATION and cannot provide an emptiness check.
    let mut directory_buffer = [0u64; 8192];
    let mut restart_scan = 1u8;
    loop {
        let mut query_status = IoStatusBlock {
            status: IoStatusValue { status: 0 }, information: 0,
        };
        let query = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle(),
                ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
                &mut query_status,
                directory_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                mem::size_of_val(&directory_buffer) as u32,
                1, // FileDirectoryInformation
                1, // ReturnSingleEntry
                ptr::null_mut(),
                restart_scan,
            )
        };
        restart_scan = 0;
        if query as u32 == 0x8000_0006 { break; } // STATUS_NO_MORE_FILES
        if query < 0 {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("handle-relative directory enumeration failed (NTSTATUS 0x{:08X})", query as u32)));
        }
        let bytes = query_status.information;
        if bytes < 64 || bytes > mem::size_of_val(&directory_buffer) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "directory enumeration returned an invalid record length"));
        }
        let record = directory_buffer.as_ptr() as *const u8;
        let name_length = unsafe { ptr::read_unaligned(record.add(60) as *const u32) } as usize;
        if name_length % 2 != 0 || 64usize.checked_add(name_length).map_or(true, |end| end > bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "directory enumeration returned an invalid name"));
        }
        let name_ptr = unsafe { record.add(64) as *const u16 };
        let name = unsafe { std::slice::from_raw_parts(name_ptr, name_length / 2) };
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "snapshot directory is not empty"));
        }
    }
    let disposition = FileDispositionInfoEx { flags: FILE_DISPOSITION_FLAG_DELETE };
    if unsafe {
        SetFileInformationByHandle(
            directory.as_raw_handle(), FILE_DISPOSITION_INFO_EX_CLASS,
            &disposition as *const _ as *const std::ffi::c_void,
            mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(directory);
    Ok(())
}


/// Unix runtime-directory deletion remains unavailable until a race-safe,
/// handle-bound removal primitive and reviewed same-user threat model exist.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_empty_directory(
    _parent: &File,
    _name: &OsStr,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated runtime cleanup is not supported on this platform",
    ))
}

#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_empty_directory(
    parent: &File,
    name: &OsStr,
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };
    if !is_single_normal_component(name)
        || name == OsStr::new(".friday-snapshot-owner.json")
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime cleanup requires one normal directory component"));
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid runtime directory name"));
    }
    let byte_len = wide.len().checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime directory name is too long"))?;
    #[repr(C)] struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)] struct ObjectAttributes {
        length: u32, root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString, attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)] union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)] struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[repr(C)] struct FileDispositionInfoEx { flags: u32 }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
            object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64, file_attributes: u32, share_access: u32,
            create_disposition: u32, create_options: u32,
            ea_buffer: *const std::ffi::c_void, ea_length: u32,
        ) -> i32;
        fn NtQueryDirectoryFile(
            file_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut std::ffi::c_void,
            length: u32,
            information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void, information_class: i32,
            information: *const std::ffi::c_void, size: u32,
        ) -> i32;
    }
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const OBJ_DONT_REPARSE: u32 = 0x1000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_OPEN: u32 = 1;
    const FILE_DIRECTORY_FILE: u32 = 1;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 1;

    let mut unicode_name = UnicodeString {
        length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle, DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes, &mut io_status, ptr::null(), 0,
            FILE_SHARE_READ, FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(), 0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            format!("handle-relative runtime directory delete open failed (NTSTATUS 0x{:08X})", status as u32)));
    }
    let directory = unsafe { File::from_raw_handle(raw_handle) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x400 != 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime entry is not an ordinary directory"));
    }
    // Enumerate through this exact restrictive handle. cap_std::read_dir
    // opens a second handle; with FILE_SHARE_READ only that second open gets
    // ERROR_SHARING_VIOLATION and cannot provide an emptiness check.
    let mut directory_buffer = [0u64; 8192];
    let mut restart_scan = 1u8;
    loop {
        let mut query_status = IoStatusBlock {
            status: IoStatusValue { status: 0 }, information: 0,
        };
        let query = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle(),
                ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
                &mut query_status,
                directory_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                mem::size_of_val(&directory_buffer) as u32,
                1, // FileDirectoryInformation
                1, // ReturnSingleEntry
                ptr::null_mut(),
                restart_scan,
            )
        };
        restart_scan = 0;
        if query as u32 == 0x8000_0006 { break; } // STATUS_NO_MORE_FILES
        if query < 0 {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("handle-relative runtime directory enumeration failed (NTSTATUS 0x{:08X})", query as u32)));
        }
        let bytes = query_status.information;
        if bytes < 64 || bytes > mem::size_of_val(&directory_buffer) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime directory enumeration returned an invalid record length"));
        }
        let record = directory_buffer.as_ptr() as *const u8;
        let name_length = unsafe { ptr::read_unaligned(record.add(60) as *const u32) } as usize;
        if name_length % 2 != 0 || 64usize.checked_add(name_length).map_or(true, |end| end > bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime directory enumeration returned an invalid name"));
        }
        let name_ptr = unsafe { record.add(64) as *const u16 };
        let name = unsafe { std::slice::from_raw_parts(name_ptr, name_length / 2) };
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "runtime directory is not empty"));
        }
    }
    let disposition = FileDispositionInfoEx { flags: FILE_DISPOSITION_FLAG_DELETE };
    if unsafe {
        SetFileInformationByHandle(
            directory.as_raw_handle(), FILE_DISPOSITION_INFO_EX_CLASS,
            &disposition as *const _ as *const std::ffi::c_void,
            mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(directory);
    Ok(())
}


/// Remove the staging root only when the held parent and freshly opened
/// root handles match the identities captured by the authenticated cleanup
/// authorization. The directory must be empty; Windows checks and deletes
/// through the same restrictive handle. Unix remains fail-closed.
#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_root_directory(
    _parent: &File,
    name: &OsStr,
    expected_parent_identity: (u64, u64),
    expected_root_identity: (u64, u64),
) -> io::Result<()> {
    if !is_single_normal_component(name)
        || name == OsStr::new(".friday-snapshot-owner.json")
        || expected_parent_identity == (0, 0)
        || expected_root_identity == (0, 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime root cleanup requires a normal name and nonzero authenticated identities"));
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated runtime root cleanup is not supported on this platform",
    ))
}

#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn remove_verified_runtime_root_directory(
    parent: &File,
    name: &OsStr,
    expected_parent_identity: (u64, u64),
    expected_root_identity: (u64, u64),
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };
    if !is_single_normal_component(name)
        || name == OsStr::new(".friday-snapshot-owner.json")
        || expected_parent_identity == (0, 0)
        || expected_root_identity == (0, 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime cleanup requires one normal directory component"));
    }
    let actual_parent_identity = handle_identity(parent)?;
    if (actual_parent_identity.0 as u64, actual_parent_identity.1) != expected_parent_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime cleanup parent handle identity changed"));
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid runtime directory name"));
    }
    let byte_len = wide.len().checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime directory name is too long"))?;
    #[repr(C)] struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)] struct ObjectAttributes {
        length: u32, root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString, attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)] union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)] struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[repr(C)] struct FileDispositionInfoEx { flags: u32 }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
            object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64, file_attributes: u32, share_access: u32,
            create_disposition: u32, create_options: u32,
            ea_buffer: *const std::ffi::c_void, ea_length: u32,
        ) -> i32;
        fn NtQueryDirectoryFile(
            file_handle: *mut std::ffi::c_void,
            event: *mut std::ffi::c_void,
            apc_routine: *mut std::ffi::c_void,
            apc_context: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut std::ffi::c_void,
            length: u32,
            information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileInformationByHandle(
            file: *mut std::ffi::c_void, information_class: i32,
            information: *const std::ffi::c_void, size: u32,
        ) -> i32;
    }
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const OBJ_DONT_REPARSE: u32 = 0x1000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_OPEN: u32 = 1;
    const FILE_DIRECTORY_FILE: u32 = 1;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;
    const FILE_DISPOSITION_FLAG_DELETE: u32 = 1;

    let mut unicode_name = UnicodeString {
        length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle, DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes, &mut io_status, ptr::null(), 0,
            FILE_SHARE_READ, FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(), 0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            format!("handle-relative runtime root delete open failed (NTSTATUS 0x{:08X})", status as u32)));
    }
    let directory = unsafe { File::from_raw_handle(raw_handle) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x400 != 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime entry is not an ordinary directory"));
    }
    let actual_root_identity = handle_identity(&directory)?;
    if (actual_root_identity.0 as u64, actual_root_identity.1) != expected_root_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime cleanup root handle identity changed"));
    }
    // Enumerate through this exact restrictive handle. cap_std::read_dir
    // opens a second handle; with FILE_SHARE_READ only that second open gets
    // ERROR_SHARING_VIOLATION and cannot provide an emptiness check.
    let mut directory_buffer = [0u64; 8192];
    let mut restart_scan = 1u8;
    loop {
        let mut query_status = IoStatusBlock {
            status: IoStatusValue { status: 0 }, information: 0,
        };
        let query = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle(),
                ptr::null_mut(), ptr::null_mut(), ptr::null_mut(),
                &mut query_status,
                directory_buffer.as_mut_ptr() as *mut std::ffi::c_void,
                mem::size_of_val(&directory_buffer) as u32,
                1, // FileDirectoryInformation
                1, // ReturnSingleEntry
                ptr::null_mut(),
                restart_scan,
            )
        };
        restart_scan = 0;
        if query as u32 == 0x8000_0006 { break; } // STATUS_NO_MORE_FILES
        if query < 0 {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("handle-relative runtime directory enumeration failed (NTSTATUS 0x{:08X})", query as u32)));
        }
        let bytes = query_status.information;
        if bytes < 64 || bytes > mem::size_of_val(&directory_buffer) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime directory enumeration returned an invalid record length"));
        }
        let record = directory_buffer.as_ptr() as *const u8;
        let name_length = unsafe { ptr::read_unaligned(record.add(60) as *const u32) } as usize;
        if name_length % 2 != 0 || 64usize.checked_add(name_length).map_or(true, |end| end > bytes) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime directory enumeration returned an invalid name"));
        }
        let name_ptr = unsafe { record.add(64) as *const u16 };
        let name = unsafe { std::slice::from_raw_parts(name_ptr, name_length / 2) };
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "runtime directory is not empty"));
        }
    }
    let disposition = FileDispositionInfoEx { flags: FILE_DISPOSITION_FLAG_DELETE };
    if unsafe {
        SetFileInformationByHandle(
            directory.as_raw_handle(), FILE_DISPOSITION_INFO_EX_CLASS,
            &disposition as *const _ as *const std::ffi::c_void,
            mem::size_of::<FileDispositionInfoEx>() as u32,
        )
    } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(directory);
    Ok(())
}





/// Remove one complete schema-2 snapshot using only its validated inventory.
///
/// Cleanup is retryable while the marker remains: already-removed inventoried
/// files/directories are accepted by the progress preflight. The marker is
/// removed only after every inventoried payload entry is gone, and the root
/// directory is attempted immediately afterwards. If a crash occurs after
/// marker removal but before root removal, the now-unmarked empty directory is
/// deliberately preserved on the next recovery pass.
#[allow(dead_code)]
pub(crate) fn cleanup_owned_snapshot(
    workspace: &File,
    snapshot_id: &str,
    limits: BoundedSnapshotLimits,
) -> io::Result<()> {
    cleanup_owned_snapshot_with_hook(workspace, snapshot_id, limits, |_| Ok(()))
}

/// Open a directory relative to a held parent with delete-compatible sharing.
/// Retained preflight handles must coexist with the later handle-based delete
/// operation on Windows; the open remains no-follow and handle-relative.
#[cfg(windows)]
fn open_snapshot_cleanup_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawHandle, FromRawHandle}},
        ptr,
    };
    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot directory name must be one normal component"));
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    let byte_len = wide.len().checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot directory name is too long"))?;
    #[repr(C)] struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
    #[repr(C)] struct ObjectAttributes {
        length: u32, root_directory: *mut std::ffi::c_void,
        object_name: *mut UnicodeString, attributes: u32,
        security_descriptor: *mut std::ffi::c_void,
        security_quality_of_service: *mut std::ffi::c_void,
    }
    #[repr(C)] union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
    #[repr(C)] struct IoStatusBlock { status: IoStatusValue, information: usize }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
            object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
            allocation_size: *const i64, file_attributes: u32, share_access: u32,
            create_disposition: u32, create_options: u32,
            ea_buffer: *const std::ffi::c_void, ea_length: u32,
        ) -> i32;
    }
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const OBJ_DONT_REPARSE: u32 = 0x1000;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    const FILE_OPEN: u32 = 1;
    const FILE_DIRECTORY_FILE: u32 = 1;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    let mut unicode_name = UnicodeString {
        length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16,
    };
    let attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle(), object_name: &mut unicode_name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: ptr::null_mut(), security_quality_of_service: ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
    let mut raw_handle = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut raw_handle, FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes, &mut io_status, ptr::null(), 0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN, FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(), 0,
        )
    };
    if status < 0 || raw_handle.is_null() {
        let kind = match status as u32 {
            0xC000_0034 | 0xC000_003A | 0xC000_000F => io::ErrorKind::NotFound,
            _ => io::ErrorKind::PermissionDenied,
        };
        return Err(io::Error::new(kind,
            format!("handle-relative snapshot directory open failed (NTSTATUS 0x{:08X})", status as u32)));
    }
    let directory = unsafe { File::from_raw_handle(raw_handle) };
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink()
        || (metadata.file_attributes() & 0x400 != 0)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot entry is not an ordinary directory"));
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_snapshot_cleanup_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    open_dir_nofollow(parent, Path::new(name))
}

fn open_snapshot_cleanup_root(workspace: &File, snapshot_id: &str) -> io::Result<File> {
    open_snapshot_cleanup_directory(workspace, OsStr::new(snapshot_id))
}

fn cleanup_owned_snapshot_with_hook<H>(
    workspace: &File,
    snapshot_id: &str,
    limits: BoundedSnapshotLimits,
    mut after_payload_entry: H,
) -> io::Result<()>
where
    H: FnMut(&str) -> io::Result<()>,
{
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot identifier"));
    }
    let root = match open_snapshot_cleanup_root(workspace, snapshot_id).map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot cleanup root open failed: {}", error))
    }) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut plan = preflight_snapshot_cleanup_plan(&root, snapshot_id, limits).map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot cleanup preflight failed: {}", error))
    })?;
    let manifest = plan.manifest.clone();

    for item in &manifest.files {
        let Some(expected_file) = plan.file_handles.get(&item.path).map(|file| file.try_clone().map_err(|error| {
            io::Error::new(error.kind(), format!("snapshot cleanup file identity handle clone failed for {}: {}", item.path, error))
        })).transpose()? else {
            // A missing inventoried entry is valid during retryable cleanup.
            continue;
        };
        let relative = Path::new(&item.path);
        let Some(parent) = open_existing_snapshot_parent_bound(
            &root, relative.parent().unwrap_or(Path::new("")), &plan.directory_handles,
        )? else {
            continue;
        };
        let result = remove_verified_snapshot_file_bound(&parent, OsStr::new(relative.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "manifest file has no filename")
        })?), item.size, Some(&expected_file)).map_err(|error| {
            io::Error::new(error.kind(), format!("snapshot payload file removal failed for {}: {}", item.path, error))
        });
        match result {
            Ok(()) => {
                drop(expected_file);
                plan.file_handles.remove(&item.path);
                after_payload_entry(&item.path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                drop(expected_file);
                plan.file_handles.remove(&item.path);
            }
            Err(error) => return Err(error),
        }
    }

    let mut directories = manifest.directories.clone();
    directories.sort_by(|left, right| {
        right.matches('/').count().cmp(&left.matches('/').count())
            .then_with(|| right.len().cmp(&left.len()))
    });
    for directory in &directories {
        let relative = Path::new(directory);
        let Some(expected_directory) = plan.directory_handles.get(directory).map(|file| file.try_clone().map_err(|error| {
            io::Error::new(error.kind(), format!("snapshot cleanup directory identity handle clone failed for {}: {}", directory, error))
        })).transpose()? else {
            // It was already absent when retry preflight ran.
            continue;
        };
        let Some(parent) = open_existing_snapshot_parent_bound(
            &root, relative.parent().unwrap_or(Path::new("")), &plan.directory_handles,
        )? else {
            continue;
        };
        let name = relative.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "manifest directory has no filename")
        })?;
        let result = remove_verified_empty_snapshot_directory(&parent, name, Some(&expected_directory)).map_err(|error| {
            io::Error::new(error.kind(), format!("snapshot payload directory removal failed for {}: {}", directory, error))
        });
        match result {
            Ok(()) => {
                drop(expected_directory);
                plan.directory_handles.remove(directory);
                after_payload_entry(directory)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                drop(expected_directory);
                plan.directory_handles.remove(directory);
            }
            Err(error) => return Err(error),
        }
    }

    // The only remaining entry may be the validated ownership marker. Do not
    // remove it if any unexpected entry or residual payload remains.
    let mut remaining = read_dir(&root, Path::new(".")).map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot cleanup final inventory open failed: {}", error))
    })?;
    let first = remaining.next().transpose()?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "snapshot ownership marker disappeared before finalization")
    })?;
    let first_name = first.file_name();
    if first_name != OsStr::new(".friday-snapshot-owner.json")
        || remaining.next().transpose()?.is_some()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains residual or unexpected entries; ownership marker preserved"));
    }
    // cap-primitives' Windows DirEntry can retain a native metadata handle
    // independently of the iterator. Release the entry itself before opening
    // the marker for DELETE access, not merely the ReadDir iterator.
    drop(first);
    // The directory iterator owns a separate Windows directory handle. Release
    // it before opening the root with DELETE access; otherwise its share mode
    // can cause ERROR_ACCESS_DENIED despite the retained identity handle being
    // delete-share compatible.
    drop(remaining);
    let marker = open_file_nofollow(&root, OsStr::new(".friday-snapshot-owner.json")).map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot cleanup ownership marker open failed: {}", error))
    })?;
    if !same_handle_identity(&plan.marker_handle, &marker)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot ownership marker no longer matches the identity captured during preflight"));
    }
    let marker_metadata = marker.metadata()?;
    if !marker_metadata.is_file() || is_reparse_or_symlink(&marker_metadata)
        || marker_metadata.len() == 0 || marker_metadata.len() > 1024 * 1024
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot ownership marker is not a bounded regular file"));
    }
    remove_verified_snapshot_file_bound(
        &root,
        OsStr::new(".friday-snapshot-owner.json"),
        marker_metadata.len(),
        Some(&plan.marker_handle),
    ).map_err(|error| io::Error::new(error.kind(), format!("snapshot cleanup ownership marker removal failed: {}", error)))?;
    drop(marker);
    // The preflight marker handle must also be released before Windows can
    // complete its handle-based delete disposition and permit root removal.
    drop(plan.marker_handle);

    // If this final operation fails, the empty unmarked directory is not
    // eligible for a later recovery pass. That is intentional fail-closed
    // behavior rather than deleting a directory without ownership evidence.
    // Both platforms compare the final name against the still-held inventoried
    // directory handle. On Windows, open_snapshot_cleanup_root deliberately
    // grants FILE_SHARE_DELETE so the final DELETE-access handle can coexist
    // with this retained identity handle.
    remove_verified_empty_snapshot_directory(workspace, OsStr::new(snapshot_id), Some(&root)).map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot root directory removal failed: {}", error))
    })
}

fn open_existing_snapshot_parent(root: &File, relative: &Path) -> io::Result<Option<File>> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest parent path is not normalized"));
        };
        match open_dir_nofollow(&current, Path::new(name)) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(current))
}

fn open_existing_snapshot_parent_bound(
    root: &File,
    relative: &Path,
    expected_directories: &std::collections::BTreeMap<String, File>,
) -> io::Result<Option<File>> {
    let mut current = root.try_clone().map_err(|error| {
        io::Error::new(error.kind(), format!("snapshot cleanup root handle clone failed: {}", error))
    })?;
    let mut walked = PathBuf::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifest parent path is not normalized"));
        };
        walked.push(name);
        let portable = portable_manifest_path(&walked)?;
        let next = match open_snapshot_cleanup_directory(&current, name) {
            Ok(next) => next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io::Error::new(
                error.kind(),
                format!("snapshot cleanup parent directory open failed for {}: {}", portable, error),
            )),
        };
        let expected = expected_directories.get(&portable).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "cleanup parent has no identity captured during preflight")
        })?;
        if !same_handle_identity(expected, &next)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "cleanup parent directory no longer matches its preflight identity"));
        }
        current = next;
    }
    Ok(Some(current))
}

/// Inspect a completed snapshot through held directory handles without mutating it.
///
/// The manifest is treated as an expected inventory, not as cryptographic
/// proof of ownership. Every encountered entry must match the inventory exactly;
/// unknown files, links/reparse points, type changes, and size changes fail closed.
#[allow(dead_code)]
pub(crate) fn preflight_snapshot_tree(
    snapshot_root: &File,
    expected_snapshot_id: &str,
    limits: BoundedSnapshotLimits,
) -> io::Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    let root_metadata = snapshot_root.metadata()?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot root handle is not an ordinary directory"));
    }

    let marker_name = OsStr::new(".friday-snapshot-owner.json");
    let mut marker = open_file_nofollow(snapshot_root, marker_name)?;
    let marker_metadata = marker.metadata()?;
    if !marker_metadata.is_file() || is_reparse_or_symlink(&marker_metadata) || marker_metadata.len() == 0 || marker_metadata.len() > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker has an unsafe type or size"));
    }
    let mut marker_bytes = Vec::with_capacity(marker_metadata.len() as usize);
    (&mut marker).take((1024 * 1024) as u64 + 1).read_to_end(&mut marker_bytes)?;
    if marker_bytes.len() as u64 != marker_metadata.len() || marker_bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker changed while being read"));
    }
    let manifest = validate_snapshot_cleanup_manifest(&marker_bytes, expected_snapshot_id, limits)?;

    let expected_files: BTreeMap<String, u64> = manifest.files.into_iter()
        .map(|item| (item.path, item.size)).collect();
    let expected_directories: BTreeSet<String> = manifest.directories.into_iter().collect();
    if limits.max_entries == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot inventory entry limit must be non-zero"));
    }
    let mut observed_files = BTreeSet::new();
    let mut observed_directories = BTreeSet::new();
    let mut entries_scanned = 0usize;
    inspect_snapshot_directory(
        snapshot_root,
        Path::new(""),
        &expected_files,
        &expected_directories,
        &marker,
        limits.max_entries,
        &mut entries_scanned,
        &mut observed_files,
        &mut observed_directories,
    )?;

    if observed_files != expected_files.keys().cloned().collect()
        || observed_directories != expected_directories
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("snapshot tree does not exactly match its inventory (observed files: {:?}, expected files: {:?}, observed directories: {:?}, expected directories: {:?})", observed_files, expected_files.keys().collect::<Vec<_>>(), observed_directories, expected_directories)));
    }
    Ok(())
}

fn inspect_snapshot_directory(
    parent: &File,
    relative_directory: &Path,
    expected_files: &std::collections::BTreeMap<String, u64>,
    expected_directories: &std::collections::BTreeSet<String>,
    marker_handle: &File,
    max_entries: usize,
    entries_scanned: &mut usize,
    observed_files: &mut std::collections::BTreeSet<String>,
    observed_directories: &mut std::collections::BTreeSet<String>,
) -> io::Result<()> {
    let entries = enumerate_runtime_tree_entries(parent, max_entries)?;
    for entry in entries {
        *entries_scanned = entries_scanned.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory entry count overflowed")
        })?;
        if *entries_scanned > max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory exceeded its entry limit"));
        }
        let name = entry.name;
        if !is_single_normal_component(&name) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains an ambiguous entry name"));
        }
        let name_text = name.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a non-UTF-8 entry name"))?;
        let relative_path = relative_directory.join(&name);
        let portable = portable_manifest_path(&relative_path)?;
        if portable == ".friday-snapshot-owner.json" && relative_directory.as_os_str().is_empty() {
            if !entry.is_file || entry.is_reparse {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker is not a regular file"));
            }
            let current_marker = open_file_nofollow(parent, &name)?;
            if !runtime_tree_handle_matches(entry.identity, &current_marker)?
                || !same_handle_identity(&current_marker, marker_handle)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker changed during preflight"));
            }
            continue;
        }
        if entry.is_reparse {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a symlink or reparse entry"));
        }
        if entry.is_directory {
            if !expected_directories.contains(&portable) || !observed_directories.insert(portable) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains an unknown or duplicate directory"));
            }
            let child = open_dir_nofollow(parent, Path::new(&name))?;
            let metadata = child.metadata()?;
            if !metadata.is_dir() || is_reparse_or_symlink(&metadata)
                || !runtime_tree_handle_matches(entry.identity, &child)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot directory changed identity or type during inspection"));
            }
            inspect_snapshot_directory(&child, &relative_path, expected_files, expected_directories, marker_handle, max_entries, entries_scanned, observed_files, observed_directories)?;
            continue;
        }
        if !entry.is_file {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a special or unsupported entry"));
        }
        let expected_size = expected_files.get(&portable).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("snapshot contains an unknown file: {}", name_text)))?;
        let file = open_file_nofollow(parent, &name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse_or_symlink(&metadata)
            || metadata.len() != *expected_size
            || !runtime_tree_handle_matches(entry.identity, &file)?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file changed identity, type, or size during inspection"));
        }
        if !observed_files.insert(portable) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a duplicate file entry"));
        }
    }
    Ok(())
}

/// Validate a snapshot that may be partway through an authorized cleanup.
///
/// Unlike `preflight_snapshot_tree`, this accepts missing inventoried
/// payload entries so a retry can resume after an interruption. It still rejects
/// every unexpected entry, unsafe type, reparse point, and size mismatch. The
/// ownership marker is mandatory and remains present until all payload cleanup
/// stages have completed. This function performs no mutation.
#[allow(dead_code)]
pub(crate) fn preflight_snapshot_cleanup_progress(
    snapshot_root: &File,
    expected_snapshot_id: &str,
    limits: BoundedSnapshotLimits,
) -> io::Result<SnapshotCleanupManifest> {
    Ok(preflight_snapshot_cleanup_plan(snapshot_root, expected_snapshot_id, limits)?.manifest)
}

fn preflight_snapshot_cleanup_plan(
    snapshot_root: &File,
    expected_snapshot_id: &str,
    limits: BoundedSnapshotLimits,
) -> io::Result<SnapshotCleanupPlan> {
    use std::collections::{BTreeMap, BTreeSet};

    let root_metadata = snapshot_root.metadata()?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot root handle is not an ordinary directory"));
    }

    let marker_name = OsStr::new(".friday-snapshot-owner.json");
    let mut marker = open_file_nofollow(snapshot_root, marker_name)?;
    let marker_metadata = marker.metadata()?;
    if !marker_metadata.is_file() || is_reparse_or_symlink(&marker_metadata)
        || marker_metadata.len() == 0 || marker_metadata.len() > 1024 * 1024
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker has an unsafe type or size"));
    }
    let mut marker_bytes = Vec::with_capacity(marker_metadata.len() as usize);
    (&mut marker).take(1024 * 1024 + 1).read_to_end(&mut marker_bytes)?;
    if marker_bytes.len() as u64 != marker_metadata.len() || marker_bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker changed while being read"));
    }
    let manifest = validate_snapshot_cleanup_manifest(&marker_bytes, expected_snapshot_id, limits)?;

    let expected_files: BTreeMap<String, u64> = manifest.files.iter()
        .map(|item| (item.path.clone(), item.size)).collect();
    let expected_directories: BTreeSet<String> = manifest.directories.iter().cloned().collect();
    if limits.max_entries == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot cleanup entry limit must be non-zero"));
    }
    let mut entries_scanned = 0usize;
    let mut file_handles = BTreeMap::new();
    let mut directory_handles = BTreeMap::new();
    inspect_snapshot_cleanup_progress_directory(
        snapshot_root,
        Path::new(""),
        &expected_files,
        &expected_directories,
        &marker,
        limits.max_entries,
        &mut entries_scanned,
        &mut file_handles,
        &mut directory_handles,
    )?;
    Ok(SnapshotCleanupPlan {
        manifest,
        file_handles,
        directory_handles,
        marker_handle: marker,
    })
}

fn inspect_snapshot_cleanup_progress_directory(
    parent: &File,
    relative_directory: &Path,
    expected_files: &std::collections::BTreeMap<String, u64>,
    expected_directories: &std::collections::BTreeSet<String>,
    marker_handle: &File,
    max_entries: usize,
    entries_scanned: &mut usize,
    file_handles: &mut std::collections::BTreeMap<String, File>,
    directory_handles: &mut std::collections::BTreeMap<String, File>,
) -> io::Result<()> {
    let entries = enumerate_runtime_tree_entries(parent, max_entries)?;
    for entry in entries {
        *entries_scanned = entries_scanned.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "snapshot cleanup entry count overflowed")
        })?;
        if *entries_scanned > max_entries {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot cleanup exceeded its entry limit"));
        }
        let name = entry.name;
        if !is_single_normal_component(&name) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains an ambiguous entry name"));
        }
        let name_text = name.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a non-UTF-8 entry name"))?;
        let relative_path = relative_directory.join(&name);
        let portable = portable_manifest_path(&relative_path)?;
        if portable == ".friday-snapshot-owner.json" && relative_directory.as_os_str().is_empty() {
            if !entry.is_file || entry.is_reparse {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker is not a regular file"));
            }
            let current_marker = open_file_nofollow(parent, &name)?;
            if !runtime_tree_handle_matches(entry.identity, &current_marker)?
                || !same_handle_identity(&current_marker, marker_handle)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot inventory marker changed during cleanup preflight"));
            }
            continue;
        }
        if entry.is_reparse {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a symlink or reparse entry"));
        }
        if entry.is_directory {
            if !expected_directories.contains(&portable) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains an unknown directory"));
            }
            let child = open_snapshot_cleanup_directory(parent, &name)?;
            let metadata = child.metadata()?;
            if !metadata.is_dir() || is_reparse_or_symlink(&metadata)
                || !runtime_tree_handle_matches(entry.identity, &child)?
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot directory changed identity or type during cleanup preflight"));
            }
            directory_handles.insert(portable.clone(), child.try_clone()?);
            inspect_snapshot_cleanup_progress_directory(
                &child, &relative_path, expected_files, expected_directories,
                marker_handle, max_entries, entries_scanned, file_handles, directory_handles,
            )?;
            continue;
        }
        if !entry.is_file {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot contains a special or unsupported entry"));
        }
        let expected_size = expected_files.get(&portable).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format!("snapshot contains an unknown file: {}", name_text))
        })?;
        let file = open_file_nofollow(parent, &name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse_or_symlink(&metadata)
            || metadata.len() != *expected_size
            || !runtime_tree_handle_matches(entry.identity, &file)?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot file changed identity, type, or size during cleanup preflight"));
        }
        if file_handles.insert(portable, file).is_some() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot cleanup preflight observed a duplicate file path"));
        }
    }
    Ok(())
}

/// Materialize a complete, bounded runtime bundle beneath a held parent handle.
///
/// The destination is created exclusively and every directory/file is created
/// relative to an already-held handle. The owner marker is written last; a
/// partial or interrupted staging directory therefore never carries cleanup
/// authority. This helper does not publish or execute the staged bundle.
#[allow(dead_code)]
pub(crate) fn create_bounded_runtime_destination(
    parent: &File,
    staging_name: &OsStr,
    files: &[(PathBuf, Vec<u8>)],
    owner_marker: &[u8],
    max_files: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_depth: usize,
) -> io::Result<()> {
    create_bounded_runtime_destination_with_hook(
        parent, staging_name, files, owner_marker,
        max_files, max_file_bytes, max_total_bytes, max_depth, |_| Ok(()),
    )
}

/// Test seam for deterministic interruption after a fully synced artifact.
/// Production always uses the no-op hook above; an interrupted write leaves
/// an unmarked staging directory that is never treated as a trusted bundle.
fn create_bounded_runtime_destination_with_hook<F>(
    parent: &File,
    staging_name: &OsStr,
    files: &[(PathBuf, Vec<u8>)],
    owner_marker: &[u8],
    max_files: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_depth: usize,
    mut after_file_written: F,
) -> io::Result<()>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    use std::io::Write;

    let parent_metadata = parent.metadata()?;
    if !parent_metadata.is_dir() || is_reparse_or_symlink(&parent_metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime staging parent is not an ordinary directory"));
    }
    if !is_single_normal_component(staging_name)
        || files.is_empty()
        || max_files == 0
        || max_file_bytes == 0
        || max_total_bytes == 0
        || max_depth == 0
        || files.len() > max_files
        || owner_marker.is_empty()
        || owner_marker.len() > 16 * 1024
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime staging request is empty or outside fixed limits"));
    }

    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for (relative_path, contents) in files {
        let components: Vec<&OsStr> = relative_path.components().filter_map(|component| {
            if let Component::Normal(name) = component { Some(name) } else { None }
        }).collect();
        if relative_path.as_os_str().is_empty()
            || components.len() != relative_path.components().count()
            || components.is_empty()
            || components.len().saturating_sub(1) > max_depth
            || components.iter().any(|component| !is_single_normal_component(component))
            || relative_path == Path::new(".friday-runtime-owner.json")
            || contents.len() as u64 > max_file_bytes
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime staging contains an unsafe path or oversized artifact"));
        }
        let key = components.iter()
            .map(|component| component.to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("\\");
        if !seen.insert(key) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime staging contains duplicate case-insensitive paths"));
        }
        total = total.checked_add(contents.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime staging byte count overflowed"))?;
        if total > max_total_bytes {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime staging exceeds its aggregate size limit"));
        }
    }

    // Exclusive creation means an existing or attacker-planted destination
    // is never adopted, merged with, or overwritten.
    let staging_root = create_destination_entry(parent, staging_name, true)?;
    for (relative_path, contents) in files {
        let components: Vec<&OsStr> = relative_path.components().filter_map(|component| {
            if let Component::Normal(name) = component { Some(name) } else { None }
        }).collect();
        let mut destination_parent = staging_root.try_clone()?;
        for component in &components[..components.len() - 1] {
            let next = match open_dir_nofollow(&destination_parent, Path::new(component)) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    create_destination_entry(&destination_parent, component, true)?
                }
                Err(error) => return Err(error),
            };
            destination_parent = next;
        }
        let mut output = create_destination_entry(
            &destination_parent,
            components[components.len() - 1],
            false,
        )?;
        output.write_all(contents)?;
        output.sync_all()?;
        after_file_written(relative_path)?;
    }

    // This marker is the final write. Its presence attests only that bounded
    // staging completed; callers must still authenticate and hash every file.
    let mut marker = create_destination_entry(
        &staging_root,
        OsStr::new(".friday-runtime-owner.json"),
        false,
    )?;
    marker.write_all(owner_marker)?;
    marker.sync_all()?;
    Ok(())
}

/// Open one ordinary child directory relative to a held parent without following links.
#[allow(dead_code)]
/// Atomically rename one verified child directory to another name below the
/// same held parent, refusing to replace any existing destination. The source
/// is reopened relative to the parent without following links and its identity
/// is compared before the native rename. No ambient source/destination path is
/// used by the rename itself.
pub(crate) fn rename_directory_noreplace(
    parent: &File,
    source_name: &OsStr,
    destination_name: &OsStr,
    expected_parent_identity: (u64, u64),
    expected_source_identity: (u64, u64),
) -> io::Result<()> {
    // Normalize the platform-specific volume identifier to the shared
    // (u64, u64) identity shape used by the higher-level preflight.
    let identity = |handle: &File| {
        handle_identity(handle).map(|(volume, file_id)| (volume as u64, file_id))
    };
    if !is_single_normal_component(source_name)
        || !is_single_normal_component(destination_name)
        || source_name == destination_name
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime publication names must be distinct normal components"));
    }
    let parent_metadata = parent.metadata()?;
    if !parent_metadata.is_dir() || is_reparse_or_symlink(&parent_metadata)
        || identity(parent)? != expected_parent_identity
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime publication parent identity changed"));
    }
    let source = open_child_directory_nofollow(parent, source_name)?;
    let source_metadata = source.metadata()?;
    if !source_metadata.is_dir() || is_reparse_or_symlink(&source_metadata)
        || identity(&source)? != expected_source_identity
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime staging source identity changed"));
    }
    drop(source);

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
        let source_c = std::ffi::CString::new(source_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
        let destination_c = std::ffi::CString::new(destination_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL"))?;
        // RENAME_NOREPLACE is an atomic kernel-enforced no-overwrite guarantee.
        const RENAME_NOREPLACE: u32 = 1;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                parent.as_raw_fd(),
                source_c.as_ptr(),
                parent.as_raw_fd(),
                destination_c.as_ptr(),
                RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
        let source_c = std::ffi::CString::new(source_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
        let destination_c = std::ffi::CString::new(destination_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL"))?;
        const RENAME_EXCL: u32 = 0x0000_0004;
        extern "C" {
            fn renameatx_np(from_fd: i32, from: *const std::ffi::c_char, to_fd: i32, to: *const std::ffi::c_char, flags: u32) -> i32;
        }
        let result = unsafe {
            renameatx_np(parent.as_raw_fd(), source_c.as_ptr(), parent.as_raw_fd(), destination_c.as_ptr(), RENAME_EXCL)
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    #[cfg(windows)]
    {
        use std::{
            mem,
            os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}},
            ptr,
        };
        let wide_source: Vec<u16> = source_name.encode_wide().collect();
        let wide_destination: Vec<u16> = destination_name.encode_wide().collect();
        if wide_source.is_empty() || wide_destination.is_empty()
            || wide_source.contains(&0) || wide_destination.contains(&0)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "runtime publication name is invalid"));
        }
        let byte_len = wide_source.len().checked_mul(2)
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime source name is too long"))?;
        #[repr(C)]
        struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
        #[repr(C)]
        struct ObjectAttributes {
            length: u32, root_directory: *mut std::ffi::c_void,
            object_name: *mut UnicodeString, attributes: u32,
            security_descriptor: *mut std::ffi::c_void,
            security_quality_of_service: *mut std::ffi::c_void,
        }
        #[repr(C)]
        union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
        #[repr(C)]
        struct IoStatusBlock { status: IoStatusValue, information: usize }
        #[repr(C)]
        struct FileRenameInformation {
            replace_if_exists: u8,
            root_directory: *mut std::ffi::c_void,
            file_name_length: u32,
            file_name: [u16; 1],
        }
        const OBJ_CASE_INSENSITIVE: u32 = 0x40;
        const OBJ_DONT_REPARSE: u32 = 0x1000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        const FILE_OPEN: u32 = 1;
        const FILE_DIRECTORY_FILE: u32 = 1;
        const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
        const FILE_RENAME_INFORMATION_CLASS: u32 = 10;
        #[link(name = "ntdll")]
        extern "system" {
            fn NtOpenFile(
                file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
                object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
                share_access: u32, open_options: u32,
            ) -> i32;
            fn NtSetInformationFile(
                file_handle: *mut std::ffi::c_void, io_status_block: *mut IoStatusBlock,
                file_information: *mut std::ffi::c_void, length: u32, class: u32,
            ) -> i32;
        }
        let mut source_name = UnicodeString {
            length: byte_len, maximum_length: byte_len, buffer: wide_source.as_ptr() as *mut u16,
        };
        let attributes = ObjectAttributes {
            length: mem::size_of::<ObjectAttributes>() as u32,
            root_directory: parent.as_raw_handle(),
            object_name: &mut source_name,
            attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut status_block = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
        let mut source_handle = ptr::null_mut();
        let status = unsafe {
            NtOpenFile(
                &mut source_handle, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &attributes, &mut status_block,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            )
        };
        if status < 0 || source_handle.is_null() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("handle-relative runtime source open failed (NTSTATUS 0x{:08X})", status as u32)));
        }
        let source = unsafe { File::from_raw_handle(source_handle) };
        if identity(&source)? != expected_source_identity {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime staging source changed before atomic publication"));
        }

        let name_bytes = wide_destination.len().checked_mul(2)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime destination name is too long"))?;
        let name_offset = mem::offset_of!(FileRenameInformation, file_name);
        let total_bytes = name_offset.checked_add(name_bytes as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime rename information size overflow"))?;
        let word_count = total_bytes.checked_add(7)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime rename buffer size overflow"))? / 8;
        let mut storage = vec![0u64; word_count];
        let info = storage.as_mut_ptr() as *mut FileRenameInformation;
        unsafe {
            ptr::write(ptr::addr_of_mut!((*info).replace_if_exists), 0);
            ptr::write(ptr::addr_of_mut!((*info).root_directory), parent.as_raw_handle());
            ptr::write(ptr::addr_of_mut!((*info).file_name_length), name_bytes);
            ptr::copy_nonoverlapping(
                wide_destination.as_ptr(),
                ptr::addr_of_mut!((*info).file_name) as *mut u16,
                wide_destination.len(),
            );
        }
        let mut rename_status = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
        let rename_result = unsafe {
            NtSetInformationFile(
                source.as_raw_handle(), &mut rename_status, info as *mut std::ffi::c_void,
                u32::try_from(total_bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "runtime rename buffer exceeds native limits"))?,
                FILE_RENAME_INFORMATION_CLASS,
            )
        };
        if rename_result < 0 {
            return Err(io::Error::new(io::ErrorKind::Other, format!("atomic no-replace runtime rename failed (NTSTATUS 0x{:08X})", rename_result as u32)));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "atomic no-replace directory publication is unsupported on this platform"));
    }

    // Verify the kernel-visible result through the same pinned parent handle.
    let published = open_child_directory_nofollow(parent, destination_name)?;
    if identity(&published)? != expected_source_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "published runtime directory identity does not match the verified staging directory"));
    }
    match open_child_directory_nofollow(parent, source_name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime staging name still exists after publication")),
        Err(error) => return Err(error),
    }
    if identity(parent)? != expected_parent_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "runtime publication parent identity changed during rename"));
    }
    Ok(())
}

/// Return a stable identity for an already-open ordinary directory.
pub(crate) fn verified_directory_handle_identity(directory: &File) -> io::Result<(u64, u64)> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "parent handle is not an ordinary directory"));
    }
    #[cfg(unix)]
    {
        let (device, inode) = handle_identity(directory)?;
        Ok((device, inode))
    }
    #[cfg(windows)]
    {
        let (volume, file_id) = windows_identity::handle_identity(directory)?;
        Ok((volume as u64, file_id))
    }
}

/// Atomically move one verified regular file to a new name below the same
/// pinned parent directory, refusing to replace an existing destination.
/// The rename itself is relative to the held parent handle on supported OSes.
pub(crate) fn rename_file_noreplace(
    parent: &File,
    source_name: &OsStr,
    destination_name: &OsStr,
    expected_parent_identity: (u64, u64),
    expected_source_identity: RuntimeTreeIdentity,
) -> io::Result<()> {
    if !is_single_normal_component(source_name)
        || !is_single_normal_component(destination_name)
        || source_name == destination_name
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "file rename names must be distinct normal components"));
    }
    if verified_directory_handle_identity(parent)? != expected_parent_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file rename parent identity changed"));
    }

    #[cfg(windows)]
    let source = {
        use std::{mem, os::windows::io::{AsRawHandle, FromRawHandle}, ptr};
        #[repr(C)]
        struct UnicodeString { length: u16, maximum_length: u16, buffer: *mut u16 }
        #[repr(C)]
        struct ObjectAttributes {
            length: u32, root_directory: *mut std::ffi::c_void,
            object_name: *mut UnicodeString, attributes: u32,
            security_descriptor: *mut std::ffi::c_void,
            security_quality_of_service: *mut std::ffi::c_void,
        }
        #[repr(C)]
        union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
        #[repr(C)]
        struct IoStatusBlock { status: IoStatusValue, information: usize }
        const OBJ_CASE_INSENSITIVE: u32 = 0x40;
        const OBJ_DONT_REPARSE: u32 = 0x1000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const SYNCHRONIZE: u32 = 0x0010_0000;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        const FILE_OPEN: u32 = 1;
        const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
        const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
        #[link(name = "ntdll")]
        extern "system" {
            fn NtOpenFile(
                file_handle: *mut *mut std::ffi::c_void, desired_access: u32,
                object_attributes: *const ObjectAttributes, io_status_block: *mut IoStatusBlock,
                share_access: u32, open_options: u32,
            ) -> i32;
        }
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = source_name.encode_wide().collect();
        let byte_len = wide.len().checked_mul(2)
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source filename is too long"))?;
        if wide.is_empty() || wide.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "source filename is invalid"));
        }
        let mut unicode_name = UnicodeString { length: byte_len, maximum_length: byte_len, buffer: wide.as_ptr() as *mut u16 };
        let attributes = ObjectAttributes {
            length: mem::size_of::<ObjectAttributes>() as u32,
            root_directory: parent.as_raw_handle(),
            object_name: &mut unicode_name,
            attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut status_block = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
        let mut handle = ptr::null_mut();
        let status = unsafe {
            NtOpenFile(
                &mut handle, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &attributes, &mut status_block,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            )
        };
        if status < 0 || handle.is_null() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("handle-relative source open failed (NTSTATUS 0x{:08X})", status as u32)));
        }
        let opened = unsafe { File::from_raw_handle(handle) };
        let metadata = opened.metadata()?;
        if !metadata.is_file() || is_reparse_or_symlink(&metadata) || !has_single_hard_link(&opened)
            || !runtime_tree_handle_matches(expected_source_identity, &opened)?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "source file identity, type, or link count changed"));
        }
        opened
    };
    #[cfg(unix)]
    let source = {
        let opened = open_file_nofollow(parent, source_name)?;
        let metadata = opened.metadata()?;
        if !metadata.is_file() || !has_single_hard_link(&opened)
            || !runtime_tree_handle_matches(expected_source_identity, &opened)?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "source file identity, type, or link count changed"));
        }
        opened
    };

    // Re-open the source name relative to the pinned parent immediately before
    // the rename syscall. This narrows the Unix name-substitution window and
    // makes the expected identity check explicit on every supported platform.
    // It is not a rename-by-handle primitive on Unix: a hostile concurrent
    // writer can still race the final name lookup and syscall. Callers must not
    // treat this as complete TOCTOU protection.
    let latest_source = open_file_nofollow(parent, source_name)?;
    let latest_metadata = latest_source.metadata()?;
    if !latest_metadata.is_file()
        || is_reparse_or_symlink(&latest_metadata)
        || !has_single_hard_link(&latest_source)
        || !runtime_tree_handle_matches(expected_source_identity, &latest_source)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData,
            "source name no longer refers to the verified regular file"));
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
        let source_c = std::ffi::CString::new(source_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
        let destination_c = std::ffi::CString::new(destination_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL"))?;
        const RENAME_NOREPLACE: u32 = 1;
        let result = unsafe {
            libc::syscall(libc::SYS_renameat2, parent.as_raw_fd(), source_c.as_ptr(),
                parent.as_raw_fd(), destination_c.as_ptr(), RENAME_NOREPLACE)
        };
        if result != 0 { return Err(io::Error::last_os_error()); }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
        let source_c = std::ffi::CString::new(source_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source name contains NUL"))?;
        let destination_c = std::ffi::CString::new(destination_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination name contains NUL"))?;
        const RENAME_EXCL: u32 = 0x0000_0004;
        extern "C" {
            fn renameatx_np(from_fd: i32, from: *const std::ffi::c_char, to_fd: i32, to: *const std::ffi::c_char, flags: u32) -> i32;
        }
        let result = unsafe {
            renameatx_np(parent.as_raw_fd(), source_c.as_ptr(), parent.as_raw_fd(), destination_c.as_ptr(), RENAME_EXCL)
        };
        if result != 0 { return Err(io::Error::last_os_error()); }
    }
    #[cfg(windows)]
    {
        use std::{mem, os::windows::io::AsRawHandle, ptr};
        #[repr(C)]
        struct FileRenameInformation {
            replace_if_exists: u8,
            root_directory: *mut std::ffi::c_void,
            file_name_length: u32,
            file_name: [u16; 1],
        }
        #[repr(C)]
        union IoStatusValue { status: i32, pointer: *mut std::ffi::c_void }
        #[repr(C)]
        struct IoStatusBlock { status: IoStatusValue, information: usize }
        const FILE_RENAME_INFORMATION_CLASS: u32 = 10;
        #[link(name = "ntdll")]
        extern "system" {
            fn NtSetInformationFile(
                file_handle: *mut std::ffi::c_void, io_status_block: *mut IoStatusBlock,
                file_information: *mut std::ffi::c_void, length: u32, class: u32,
            ) -> i32;
        }
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = destination_name.encode_wide().collect();
        let name_bytes = wide.len().checked_mul(2)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination filename is too long"))?;
        let name_offset = mem::offset_of!(FileRenameInformation, file_name);
        let total_bytes = name_offset.checked_add(name_bytes as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename information size overflow"))?;
        let word_count = total_bytes.checked_add(7)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename buffer size overflow"))? / 8;
        let mut storage = vec![0u64; word_count];
        let info = storage.as_mut_ptr() as *mut FileRenameInformation;
        unsafe {
            ptr::write(ptr::addr_of_mut!((*info).replace_if_exists), 0);
            ptr::write(ptr::addr_of_mut!((*info).root_directory), parent.as_raw_handle());
            ptr::write(ptr::addr_of_mut!((*info).file_name_length), name_bytes);
            ptr::copy_nonoverlapping(wide.as_ptr(), ptr::addr_of_mut!((*info).file_name) as *mut u16, wide.len());
        }
        let mut status_block = IoStatusBlock { status: IoStatusValue { status: 0 }, information: 0 };
        let status = unsafe {
            NtSetInformationFile(source.as_raw_handle(), &mut status_block, info as *mut std::ffi::c_void,
                u32::try_from(total_bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename buffer exceeds native limits"))?,
                FILE_RENAME_INFORMATION_CLASS)
        };
        if status < 0 {
            return Err(io::Error::new(io::ErrorKind::Other, format!("handle-relative no-replace file rename failed (NTSTATUS 0x{:08X})", status as u32)));
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = source;
        return Err(io::Error::new(io::ErrorKind::Unsupported, "handle-relative file rename is unsupported on this platform"));
    }

    drop(source);
    let published = open_result_file_nofollow(parent, destination_name)?;
    if !runtime_tree_handle_matches(expected_source_identity, &published)? || !has_single_hard_link(&published) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "renamed file identity does not match the verified source"));
    }
    match open_result_file_nofollow(parent, source_name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "source name still exists after file rename")),
        Err(error) => return Err(error),
    }
    if verified_directory_handle_identity(parent)? != expected_parent_identity {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file rename parent identity changed during operation"));
    }
    Ok(())
}

/// Create a new regular file relative to a pinned directory handle without
/// following links or replacing an existing entry. Unix callers must retain the
/// returned handle for all writes and verification; never reopen the path by
/// joining it to an ambient project path.
#[cfg(unix)]
pub(crate) fn create_new_file_nofollow(parent: &File, name: &OsStr, mode: u32) -> io::Result<File> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, io::{AsRawFd, FromRawFd}},
    };

    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "file name must be exactly one normal path component"));
    }
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name contains NUL"))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || !has_single_hard_link(&file) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "new file is not an ordinary single-link regular file"));
    }
    Ok(file)
}

pub(crate) fn open_child_directory_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
    if !is_single_normal_component(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "child directory name must be one normal component"));
    }
    let child = open_dir_nofollow(parent, Path::new(name))?;
    let metadata = child.metadata()?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "child directory is not an ordinary directory"));
    }
    Ok(child)
}

/// Read a bounded selected project file by walking from a verified root handle.
/// Every parent is opened relative to its held parent, and the final file must
/// still have the exact identity captured by the project scan.
pub(crate) fn read_verified_project_file(
    root: &File,
    relative_path: &Path,
    expected_identity: RuntimeTreeIdentity,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    if max_bytes == 0 || relative_path.as_os_str().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "selected project path or size limit is invalid"));
    }
    let components = relative_path.components().collect::<Vec<_>>();
    if components.is_empty() || components.len() > 6
        || components.iter().any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "selected project path is not normalized"));
    }
    let mut parent = root.try_clone()?;
    for component in &components[..components.len() - 1] {
        let Component::Normal(name) = component else { unreachable!() };
        parent = open_child_directory_nofollow(&parent, name)?;
    }
    let Component::Normal(file_name) = components[components.len() - 1] else { unreachable!() };
    // Windows denies concurrent write/delete sharing while the read is active.
    let mut file = open_result_file_nofollow(&parent, file_name)?;
    let before = file.metadata()?;
    if !before.is_file() || is_reparse_or_symlink(&before) || !has_single_hard_link(&file)
        || before.len() == 0 || before.len() > max_bytes
        || !runtime_tree_handle_matches(expected_identity, &file)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "selected project file is not the scanned regular file"));
    }
    let expected_length = before.len();
    let capacity = usize::try_from(expected_length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "selected project file size cannot be represented"))?;
    let mut contents = Vec::with_capacity(capacity);
    file.by_ref().take(max_bytes.saturating_add(1)).read_to_end(&mut contents)?;
    let after = file.metadata()?;
    if contents.len() as u64 != expected_length || contents.len() as u64 > max_bytes
        || after.len() != expected_length || !after.is_file() || is_reparse_or_symlink(&after)
        || !has_single_hard_link(&file) || !runtime_tree_handle_matches(expected_identity, &file)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "selected project file changed while being read"));
    }
    Ok(contents)
}

/// Materialize bounded in-memory source bytes below a held private workspace.
/// The ownership marker is written last, so interrupted creation is never
/// falsely advertised as a complete, cleanup-authorized snapshot.
#[allow(dead_code)]
pub(crate) fn create_bounded_snapshot_destination(
    workspace: &File,
    snapshot_id: &str,
    contents: &BoundedSnapshotContents,
    limits: BoundedSnapshotLimits,
) -> io::Result<()> {
    use std::io::Write;
    let workspace_metadata = workspace.metadata()?;
    if !workspace_metadata.is_dir() || is_reparse_or_symlink(&workspace_metadata) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "workspace handle is not an ordinary directory"));
    }
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot identifier"));
    }
    if limits.max_files == 0 || limits.max_entries == 0
        || limits.max_file_bytes == 0 || limits.max_total_bytes == 0
        || contents.truncated || contents.files.is_empty() || contents.files.len() > limits.max_files
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot contents are incomplete or outside policy limits"));
    }
    let mut total = 0u64;
    for item in &contents.files {
        let depth = item.relative_path.components().count().saturating_sub(1);
        if item.relative_path.as_os_str().is_empty()
            || item.relative_path.components().any(|c| !matches!(c, Component::Normal(_)))
            || item.relative_path == Path::new(".friday-snapshot-owner.json")
            || depth > limits.max_depth
            || item.contents.is_empty()
            || item.contents.len() as u64 > limits.max_file_bytes
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot contains an unsafe path or file size"));
        }
        total = total.checked_add(item.contents.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot byte count overflow"))?;
    }
    if total != contents.bytes || total > limits.max_total_bytes {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot total byte count is inconsistent or oversized"));
    }

    // Validate and serialize the complete inventory before creating anything.
    // This also guarantees the marker stays within the parser's hard size bound.
    let manifest = build_snapshot_cleanup_manifest(snapshot_id, contents, limits)?;
    let marker = serde_json::to_vec(&manifest)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "could not serialize snapshot inventory"))?;
    if marker.len() > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot inventory exceeds its size limit"));
    }

    let snapshot_root = create_destination_entry(workspace, OsStr::new(snapshot_id), true)?;
    for item in &contents.files {
        let components: Vec<&OsStr> = item.relative_path.components().filter_map(|c| {
            if let Component::Normal(name) = c { Some(name) } else { None }
        }).collect();
        let mut parent = snapshot_root.try_clone()?;
        for component in &components[..components.len() - 1] {
            let next = match open_dir_nofollow(&parent, Path::new(component)) {
                Ok(dir) => dir,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    create_destination_entry(&parent, component, true)?
                }
                Err(error) => return Err(error),
            };
            parent = next;
        }
        let file_name = components[components.len() - 1];
        let mut output = create_destination_entry(&parent, file_name, false)?;
        output.write_all(&item.contents)?;
        output.sync_all()?;
    }

    let mut marker_file = create_destination_entry(
        &snapshot_root, OsStr::new(".friday-snapshot-owner.json"), false,
    )?;
    marker_file.write_all(&marker)?;
    marker_file.sync_all()?;
    Ok(())
}


/// Verify a completed snapshot's exact ownership inventory before it is mapped
/// into Windows Sandbox. This is a read-only preflight: it never repairs or
/// removes uncertain files. The returned path is the canonical fixed child
/// workspace/<snapshot_id>.
pub(crate) fn verify_snapshot_for_sandbox_mapping(
    workspace_path: &Path,
    snapshot_id: &str,
    limits: (usize, usize, u64, u64),
) -> io::Result<PathBuf> {
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot identifier is invalid"));
    }
    if !workspace_path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot workspace must be absolute"));
    }
    let workspace = open_verified_root_directory(workspace_path)?;
    let canonical_workspace = std::fs::canonicalize(workspace_path)?;
    if !same_path_handle_identity(&canonical_workspace, &workspace)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot workspace identity changed"));
    }
    let snapshot_path = canonical_workspace.join(snapshot_id);
    let snapshot = open_verified_root_directory(&snapshot_path)?;
    let policy = BoundedSnapshotLimits {
        max_depth: limits.0,
        max_files: limits.1,
        // Bound metadata work even when most entries are excluded by policy.
        max_entries: limits.1.saturating_mul(32),
        max_file_bytes: limits.2,
        max_total_bytes: limits.3,
    };
    preflight_snapshot_tree(&snapshot, snapshot_id, policy)?;
    let canonical_snapshot = std::fs::canonicalize(&snapshot_path)?;
    if canonical_snapshot != snapshot_path
        || snapshot_path.parent() != Some(canonical_workspace.as_path())
        || snapshot_path.file_name().and_then(OsStr::to_str) != Some(snapshot_id)
        || !same_path_handle_identity(&canonical_snapshot, &snapshot)?
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot path no longer matches its verified workspace child"));
    }
    Ok(canonical_snapshot)
}

/// Production entry point: collect bounded source bytes through verified
/// directory capabilities and create an exclusive schema-2 snapshot.
/// A truncated collection is rejected; incomplete snapshots are never marked
/// as complete or eligible for automatic cleanup.
pub(crate) fn prepare_snapshot_from_paths<AF, AD>(
    source_path: &Path,
    workspace_path: &Path,
    snapshot_id: &str,
    limits: (usize, usize, u64, u64),
    include_file: AF,
    exclude_directory: AD,
) -> io::Result<(usize, u64)>
where
    AF: Fn(&str) -> bool,
    AD: Fn(&str) -> bool,
{
    prepare_snapshot_from_paths_with_hook(
        source_path, workspace_path, snapshot_id, limits,
        include_file, exclude_directory, || Ok(()),
    )
}

/// Testable publication boundary: after bounded collection, revalidate that
/// both user-selected paths still identify the exact directory handles opened
/// at entry. This prevents a root rename/replacement during a potentially long
/// collection from silently publishing a snapshot sourced from a detached or
/// substituted directory. The hook is private and exists only for race tests.
fn prepare_snapshot_from_paths_with_hook<AF, AD, H>(
    source_path: &Path,
    workspace_path: &Path,
    snapshot_id: &str,
    limits: (usize, usize, u64, u64),
    include_file: AF,
    exclude_directory: AD,
    mut before_publish: H,
) -> io::Result<(usize, u64)>
where
    AF: Fn(&str) -> bool,
    AD: Fn(&str) -> bool,
    H: FnMut() -> io::Result<()>,
{
    let policy = BoundedSnapshotLimits {
        max_depth: limits.0,
        max_files: limits.1,
        // Bound metadata work even when most entries are excluded by policy.
        max_entries: limits.1.saturating_mul(32),
        max_file_bytes: limits.2,
        max_total_bytes: limits.3,
    };
    let source = open_verified_root_directory(source_path)?;
    let workspace = open_verified_root_directory(workspace_path)?;
    let source_identity = verified_directory_identity(source_path, &source)?;
    let workspace_identity = verified_directory_identity(workspace_path, &workspace)?;
    let contents = collect_bounded_snapshot_contents(
        &source, policy, include_file, exclude_directory,
    )?;
    if contents.truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot source exceeded a collection limit; incomplete snapshots are not published",
        ));
    }

    // Deterministic race point for tests. Production supplies a no-op.
    before_publish()?;
    if verified_directory_identity(source_path, &source)? != source_identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot source path no longer identifies the originally approved directory",
        ));
    }
    if verified_directory_identity(workspace_path, &workspace)? != workspace_identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot workspace path no longer identifies the originally approved directory",
        ));
    }

    let file_count = contents.files.len();
    let byte_count = contents.bytes;
    create_bounded_snapshot_destination(&workspace, snapshot_id, &contents, policy)?;
    Ok((file_count, byte_count))
}

/// Production cleanup entry point. Only a complete schema-2 inventory can
/// authorize deletion; schema-1, malformed, missing, partial, and unknown
/// snapshots are intentionally preserved.
pub(crate) fn cleanup_snapshot_from_workspace_handle(
    workspace: &File,
    snapshot_id: &str,
    limits: (usize, usize, u64, u64),
) -> io::Result<()> {
    let policy = BoundedSnapshotLimits {
        max_depth: limits.0,
        max_files: limits.1,
        // Bound metadata work even when most entries are excluded by policy.
        max_entries: limits.1.saturating_mul(32),
        max_file_bytes: limits.2,
        max_total_bytes: limits.3,
    };
    cleanup_owned_snapshot(workspace, snapshot_id, policy)
}

/// Path-based convenience wrapper for callers that do not already hold a
/// verified workspace handle. Recovery should use the handle-based entry point.
pub(crate) fn cleanup_snapshot_from_workspace_path(
    workspace_path: &Path,
    snapshot_id: &str,
    limits: (usize, usize, u64, u64),
) -> io::Result<()> {
    let workspace = open_verified_root_directory(workspace_path)?;
    cleanup_snapshot_from_workspace_handle(&workspace, snapshot_id, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, io::Read};

    fn make_test_directory(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "friday-capability-walk-{}-{}",
            label,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("test root should be created");
        path
    }

    fn open_test_root(path: &Path) -> File {
        cap_primitives::fs::open_ambient_dir(
            path,
            ambient_authority::ambient_authority(),
        )
        .expect("test root should open")
    }


    fn bounded_limits() -> BoundedSnapshotLimits {
        BoundedSnapshotLimits {
            max_depth: 12,
            max_files: 2_000,
            max_entries: 64_000,
            max_file_bytes: 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_new_file_is_exclusive_and_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let root_path = make_test_directory("exclusive-relative-create");
        let root = open_verified_root_directory(&root_path).unwrap();

        let mut created = create_new_file_nofollow(&root, OsStr::new("created.txt"), 0o600).unwrap();
        use std::io::Write;
        created.write_all(b"owned contents").unwrap();
        created.sync_all().unwrap();
        drop(created);

        assert!(create_new_file_nofollow(&root, OsStr::new("created.txt"), 0o600).is_err(),
            "exclusive creation must not replace an existing regular file");

        fs::write(root_path.join("outside.txt"), b"outside").unwrap();
        symlink(root_path.join("outside.txt"), root_path.join("redirect.txt")).unwrap();
        assert!(create_new_file_nofollow(&root, OsStr::new("redirect.txt"), 0o600).is_err(),
            "exclusive creation must not follow or replace a symbolic link");
        assert_eq!(fs::read(root_path.join("outside.txt")).unwrap(), b"outside");

        drop(root);
        fs::remove_dir_all(&root_path).unwrap();
    }

    #[test]
    fn bounded_snapshot_entry_budget_counts_ignored_files_and_directories() {
        let root_path = make_test_directory("snapshot-entry-budget");
        for index in 0..8 {
            fs::write(root_path.join(format!("ignored-{index}.txt")), b"ignored").unwrap();
        }
        let root = open_verified_root_directory(&root_path).unwrap();
        let mut limits = bounded_limits();
        limits.max_entries = 3;

        let result = collect_bounded_snapshot_contents(
            &root,
            limits,
            &|name: &str| name.ends_with(".rs"),
            &|_: &str| false,
        ).unwrap();

        assert!(result.truncated, "ignored entries must still consume the scan budget");
        assert!(result.files.is_empty());
        assert_eq!(result.bytes, 0);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn bounded_snapshot_entry_budget_accepts_exact_boundary() {
        let root_path = make_test_directory("snapshot-entry-budget-boundary");
        fs::write(root_path.join("main.rs"), b"fn main() {}").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let mut limits = bounded_limits();
        limits.max_entries = 1;

        let result = collect_bounded_snapshot_contents(
            &root,
            limits,
            &|name: &str| name == "main.rs",
            &|_: &str| false,
        ).unwrap();

        assert!(!result.truncated, "a tree exactly at the entry limit is complete");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.bytes, b"fn main() {}".len() as u64);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn bounded_snapshot_entry_budget_counts_excluded_directories() {
        let root_path = make_test_directory("snapshot-entry-budget-directories");
        for index in 0..6 {
            fs::create_dir(root_path.join(format!("excluded-{index}"))).unwrap();
        }
        let root = open_verified_root_directory(&root_path).unwrap();
        let mut limits = bounded_limits();
        limits.max_entries = 2;

        let result = collect_bounded_snapshot_contents(
            &root,
            limits,
            &|_: &str| true,
            &|_: &str| true,
        ).unwrap();

        assert!(result.truncated, "excluded directories must consume the scan budget");
        assert!(result.files.is_empty());
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn bounded_relative_runtime_reader_opens_nested_files_from_held_handles() {
        let root_path = make_test_directory("runtime-relative-read");
        fs::create_dir_all(root_path.join("node_modules").join("npm").join("bin")).unwrap();
        fs::write(root_path.join("node_modules").join("npm").join("bin").join("npm-cli.js"), b"trusted npm cli").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let bytes = read_bounded_relative_file(&root, "node_modules\\npm\\bin\\npm-cli.js", 64).unwrap();
        assert_eq!(bytes, b"trusted npm cli");
        assert!(read_bounded_relative_file(&root, "node_modules\\npm\\bin\\npm-cli.js", 4).is_err());
        for invalid in ["", "..\\secret", "node_modules/evil", "node_modules\\..\\secret", "\\rooted", "C:\\outside"] {
            assert!(read_bounded_relative_file(&root, invalid, 64).is_err(), "accepted {invalid:?}");
        }
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_relative_runtime_reader_rejects_symlinked_parent_and_file() {
        use std::os::unix::fs::symlink;
        let root_path = make_test_directory("runtime-relative-links");
        let outside = make_test_directory("runtime-relative-outside");
        fs::write(outside.join("tool.bin"), b"outside bytes").unwrap();
        fs::create_dir(root_path.join("linked-parent")).unwrap();
        symlink(&outside, root_path.join("linked-parent").join("nested")).unwrap();
        symlink(outside.join("tool.bin"), root_path.join("linked-file")).unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        assert!(read_bounded_relative_file(&root, "linked-parent\\nested\\tool.bin", 64).is_err());
        assert!(read_bounded_relative_file(&root, "linked-file", 64).is_err());
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    fn bounded_single_file_reader_accepts_only_the_exact_regular_file() {
        let directory = make_test_directory("bounded-result-reader");
        fs::write(directory.join("result.json"), br#"{"status":"ok"}"#).unwrap();
        let bytes = read_single_bounded_file_from_directory(
            &directory, OsStr::new("result.json"), 128,
        ).unwrap();
        assert_eq!(bytes, br#"{"status":"ok"}"#);

        fs::write(directory.join("extra.txt"), b"unexpected").unwrap();
        assert!(read_single_bounded_file_from_directory(
            &directory, OsStr::new("result.json"), 128,
        ).is_err());
        fs::remove_file(directory.join("extra.txt")).unwrap();
        assert!(read_single_bounded_file_from_directory(
            &directory, OsStr::new("different.json"), 128,
        ).is_err());
        assert!(read_single_bounded_file_from_directory(
            &directory, OsStr::new("result.json"), 4,
        ).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_single_file_reader_rejects_replaced_directory_path() {
        let parent = make_test_directory("bounded-result-root-replacement");
        let original = parent.join("result-root");
        let moved = parent.join("moved-result-root");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("result.json"), b"original").unwrap();
        let held = open_verified_root_directory(&original).unwrap();

        fs::rename(&original, &moved).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("result.json"), b"replacement").unwrap();

        assert!(
            read_single_bounded_file_from_handle(
                &original, &held, OsStr::new("result.json"), 128,
            ).is_err(),
            "a path that now names a replacement directory must not be accepted"
        );
        assert_eq!(fs::read(moved.join("result.json")).unwrap(), b"original");
        assert_eq!(fs::read(original.join("result.json")).unwrap(), b"replacement");

        drop(held);
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn result_file_handle_blocks_concurrent_writers_and_deleters() {
        use std::fs::OpenOptions;

        let parent = make_test_directory("bounded-result-exclusive-read");
        let result_dir = parent.join("result");
        fs::create_dir(&result_dir).unwrap();
        let result_path = result_dir.join("result.json");
        fs::write(&result_path, br#"{"status":"ok"}"#).unwrap();
        let directory = open_verified_root_directory(&result_dir).unwrap();

        // An already-open writer must prevent the exclusive result reader from
        // acquiring a handle; collecting while the guest can still write fails closed.
        let writer = OpenOptions::new().write(true).open(&result_path).unwrap();
        assert!(
            open_result_file_nofollow(&directory, OsStr::new("result.json")).is_err(),
            "result collection must not race an already-open writer"
        );
        drop(writer);

        let reader = open_result_file_nofollow(&directory, OsStr::new("result.json")).unwrap();
        assert!(
            OpenOptions::new().write(true).open(&result_path).is_err(),
            "a retained result handle must deny new writers"
        );
        assert!(
            fs::remove_file(&result_path).is_err(),
            "a retained result handle must deny deletion/replacement"
        );

        drop(reader);
        drop(directory);
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn bounded_single_file_reader_rejects_hard_linked_result_file() {
        let parent = make_test_directory("bounded-result-hard-link");
        let result_dir = parent.join("result");
        let external = parent.join("outside.json");
        fs::create_dir(&result_dir).unwrap();
        fs::write(&external, b"shared-result-bytes").unwrap();
        fs::hard_link(&external, result_dir.join("result.json"))
            .expect("the test filesystem should support hard links");

        assert!(
            read_single_bounded_file_from_directory(
                &result_dir, OsStr::new("result.json"), 128,
            ).is_err(),
            "a result file with another filesystem link must be rejected"
        );
        assert_eq!(fs::read(&external).unwrap(), b"shared-result-bytes");
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_single_file_reader_rejects_file_and_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let parent = make_test_directory("bounded-result-links");
        let outside = parent.join("outside");
        let linked_directory = parent.join("linked-directory");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("result.json"), b"outside").unwrap();
        symlink(&outside, &linked_directory).unwrap();
        assert!(read_single_bounded_file_from_directory(
            &linked_directory, OsStr::new("result.json"), 128,
        ).is_err());

        let local = parent.join("local");
        fs::create_dir(&local).unwrap();
        symlink(outside.join("result.json"), local.join("result.json")).unwrap();
        assert!(read_single_bounded_file_from_directory(
            &local, OsStr::new("result.json"), 128,
        ).is_err());
        fs::remove_file(local.join("result.json")).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn bounded_single_file_reader_rejects_windows_junction_result_directory() {
        use std::process::Command;

        let parent = make_test_directory("bounded-result-junction");
        let outside = parent.join("outside");
        let linked = parent.join("linked");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("result.json"), b"outside").unwrap();
        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&linked)
            .arg(&outside)
            .output()
            .expect("cmd.exe should be available on Windows");
        assert!(
            output.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(read_single_bounded_file_from_directory(
            &linked, OsStr::new("result.json"), 128,
        ).is_err(), "a junction must never be accepted as a result directory");
        fs::remove_dir(&linked).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }


    #[test]
    fn bounded_snapshot_reads_nested_files_through_held_handles() {
        let root_path = make_test_directory("bounded-snapshot-nested");
        fs::create_dir_all(root_path.join("src").join("nested")).unwrap();
        fs::write(root_path.join("Cargo.toml"), b"[package]").unwrap();
        fs::write(root_path.join("src").join("lib.rs"), b"pub fn ok() {}").unwrap();
        fs::write(root_path.join("src").join("nested").join("mod.rs"), b"pub mod nested;").unwrap();

        let root = open_verified_root_directory(&root_path).unwrap();
        let result = collect_bounded_snapshot_contents(
            &root,
            bounded_limits(),
            |name| name.ends_with(".rs") || name == "Cargo.toml",
            |_| false,
        ).unwrap();

        assert_eq!(result.files.len(), 3);
        assert_eq!(result.bytes, b"[package]".len() as u64
            + b"pub fn ok() {}".len() as u64
            + b"pub mod nested;".len() as u64);
        assert!(!result.truncated);
        assert!(result.files.iter().any(|file| {
            file.relative_path == Path::new("src/lib.rs")
                && file.contents == b"pub fn ok() {}"
        }));

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn bounded_snapshot_enforces_file_count_and_total_byte_limits() {
        let root_path = make_test_directory("bounded-snapshot-limits");
        fs::write(root_path.join("a.txt"), b"12345").unwrap();
        fs::write(root_path.join("b.txt"), b"67890").unwrap();
        fs::write(root_path.join("c.txt"), b"abcde").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();

        let mut limits = bounded_limits();
        limits.max_files = 2;
        limits.max_total_bytes = 8;
        let result = collect_bounded_snapshot_contents(
            &root, limits, |name| name.ends_with(".txt"), |_| false,
        ).unwrap();

        assert!(result.files.len() <= 2);
        assert!(result.bytes <= 8);
        assert!(result.truncated);

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_snapshot_never_reads_file_replaced_by_symlink_after_enumeration() {
        use std::os::unix::fs::symlink;

        let root_path = make_test_directory("bounded-snapshot-file-swap");
        let outside = make_test_directory("bounded-snapshot-file-swap-outside");
        fs::write(root_path.join("replace.txt"), b"authorized").unwrap();
        fs::write(outside.join("secret.txt"), b"outside secret").unwrap();
        let root = open_verified_root_directory(&root_path);
        let mut swapped = false;

        let result = collect_bounded_snapshot_contents_with_hook(
            &root.unwrap(),
            bounded_limits(),
            &|name| name == "replace.txt",
            &|_: &str| false,
            |relative| {
                if relative == Path::new("replace.txt") && !swapped {
                    fs::remove_file(root_path.join("replace.txt"))?;
                    symlink(outside.join("secret.txt"), root_path.join("replace.txt"))?;
                    swapped = true;
                }
                Ok(())
            },
        ).unwrap();

        assert!(swapped);
        assert!(result.files.is_empty(), "the replacement target must never be copied");
        assert!(result.truncated, "a raced file open must be reported as partial");
        fs::remove_file(root_path.join("replace.txt")).unwrap();
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_snapshot_uses_original_held_parent_after_parent_path_replacement() {
        use std::os::unix::fs::symlink;

        let base = make_test_directory("bounded-snapshot-parent-race");
        let root_path = base.join("project");
        let replacement = base.join("replacement");
        let displaced = base.join("displaced");
        fs::create_dir(&root_path).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(root_path.join("source.rs"), b"authorized").unwrap();
        fs::write(replacement.join("source.rs"), b"outside").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();

        fs::rename(&root_path, &displaced).unwrap();
        symlink(&replacement, &root_path).unwrap();
        let result = collect_bounded_snapshot_contents(
            &root,
            bounded_limits(),
            |name| name == "source.rs",
            |_| false,
        ).unwrap();

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].contents, b"authorized");
        drop(root);
        fs::remove_file(&root_path).unwrap();
        fs::remove_dir_all(displaced).unwrap();
        fs::remove_dir_all(replacement).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn bounded_snapshot_rejects_zero_limits() {
        let root_path = make_test_directory("bounded-snapshot-zero-limit");
        let root = open_verified_root_directory(&root_path).unwrap();
        let mut limits = bounded_limits();
        limits.max_total_bytes = 0;
        assert!(collect_bounded_snapshot_contents(
            &root, limits, |_| true, |_| false,
        ).is_err());
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn verified_root_handle_matches_the_authorized_directory_identity() {
        let root_path = make_test_directory("verified-root");
        fs::write(root_path.join("safe.rs"), b"safe").unwrap();

        let root = open_verified_root_directory(&root_path).unwrap();
        let opened = root.metadata().unwrap();
        assert!(opened.is_dir());
        assert!(!is_reparse_or_symlink(&opened));
        assert!(same_path_handle_identity(&root_path, &root).unwrap());

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn verified_root_handle_rejects_root_replaced_after_identity_capture() {
        let root_path = make_test_directory("verified-root-swap");
        let replacement_path = root_path.with_file_name(format!(
            "friday-capability-walk-replacement-{}",
            std::process::id()
        ));
        let displaced_path = root_path.with_file_name(format!(
            "friday-capability-walk-displaced-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&replacement_path);
        let _ = fs::remove_dir_all(&displaced_path);
        fs::create_dir_all(&replacement_path).unwrap();
        fs::write(root_path.join("original.txt"), b"authorized original").unwrap();
        fs::write(replacement_path.join("replacement.txt"), b"untrusted replacement").unwrap();

        let mut replaced = false;
        let result = open_verified_root_directory_with_hook(&root_path, || {
            fs::rename(&root_path, &displaced_path)?;
            fs::rename(&replacement_path, &root_path)?;
            replaced = true;
            Ok(())
        });

        assert!(replaced, "test must replace the root after identity capture");
        assert!(
            result.is_err(),
            "root acquisition must reject a different ordinary directory at the same path"
        );

        drop(result);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(displaced_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_root_handle_rejects_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let real_parent = make_test_directory("ancestor-real");
        let link_parent = real_parent.with_file_name(format!(
            "friday-capability-walk-ancestor-link-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&link_parent);
        fs::create_dir(real_parent.join("project")).unwrap();
        symlink(&real_parent, &link_parent).unwrap();

        let result = open_verified_root_directory(&link_parent.join("project"));
        assert!(result.is_err(), "a symlinked ancestor must be rejected");

        fs::remove_file(link_parent).unwrap();
        fs::remove_dir_all(real_parent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verified_root_handle_accepts_canonical_drive_path() {
        let root_path = make_test_directory("canonical-drive-root");
        fs::write(root_path.join("safe.rs"), b"safe").unwrap();
        let canonical = fs::canonicalize(&root_path).unwrap();

        let root = open_verified_root_directory(&canonical).unwrap();
        assert!(root.metadata().unwrap().is_dir());
        assert!(same_path_handle_identity(&canonical, &root).unwrap());

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verified_root_handle_rejects_unc_paths_until_share_roots_are_reviewed() {
        let unc_path = Path::new(r"\\server\share\project");
        let result = open_directory_from_filesystem_root(unc_path);
        assert!(
            matches!(result, Err(ref error) if error.kind() == io::ErrorKind::Unsupported),
            "UNC roots must fail closed until their root-handle semantics are reviewed"
        );
    }

    #[cfg(windows)]
    #[test]
    fn verified_root_handle_rejects_junction_ancestor() {
        use std::process::Command;

        let real_parent = make_test_directory("ancestor-real");
        let link_parent = real_parent.with_file_name(format!(
            "friday-capability-walk-ancestor-junction-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&link_parent);
        fs::create_dir(real_parent.join("project")).unwrap();

        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link_parent)
            .arg(&real_parent)
            .output()
            .expect("cmd.exe should be available on Windows");
        assert!(
            output.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let result = open_verified_root_directory(&link_parent.join("project"));
        assert!(result.is_err(), "a junction ancestor must be rejected");

        fs::remove_dir(&link_parent).unwrap();
        fs::remove_dir_all(real_parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_root_handle_rejects_ancestor_replaced_after_its_handle_is_opened() {
        use std::os::unix::fs::symlink;

        let base = make_test_directory("ancestor-held-handle-race");
        let ancestor = base.join("ancestor");
        let replacement = base.join("replacement");
        let displaced = base.join("displaced");
        fs::create_dir_all(ancestor.join("project")).unwrap();
        fs::create_dir_all(replacement.join("project")).unwrap();
        fs::write(ancestor.join("project").join("authorized.txt"), b"original").unwrap();
        fs::write(replacement.join("project").join("outside.txt"), b"outside").unwrap();

        let root_path = ancestor.join("project");
        let mut replaced = false;
        let result = open_verified_root_directory_with_hooks(
            &root_path,
            || Ok(()),
            |opened| {
                if opened == ancestor && !replaced {
                    // The walker already holds the original ancestor handle.
                    // Replacing its pathname must not redirect the next open.
                    fs::rename(&ancestor, &displaced)?;
                    symlink(&replacement, &ancestor)?;
                    replaced = true;
                }
                Ok(())
            },
        );

        assert!(replaced, "the ancestor must be replaced after its handle is opened");
        assert!(
            result.is_err(),
            "root acquisition must reject a path that changed during handle-relative descent"
        );

        fs::remove_file(&ancestor).unwrap();
        fs::remove_dir_all(displaced).unwrap();
        fs::remove_dir_all(replacement).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_root_handle_rejects_ancestor_replaced_after_validation() {
        use std::os::unix::fs::symlink;

        let ancestor = make_test_directory("ancestor-race-root");
        let replacement_parent = ancestor.with_file_name(format!(
            "friday-capability-walk-ancestor-race-replacement-{}",
            std::process::id()
        ));
        let displaced_parent = ancestor.with_file_name(format!(
            "friday-capability-walk-ancestor-race-displaced-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&replacement_parent);
        let _ = fs::remove_dir_all(&displaced_parent);
        fs::create_dir_all(ancestor.join("project")).unwrap();
        fs::create_dir_all(replacement_parent.join("project")).unwrap();
        fs::write(ancestor.join("project").join("authorized.txt"), b"original").unwrap();
        fs::write(replacement_parent.join("project").join("replacement.txt"), b"replacement").unwrap();

        let root_path = ancestor.join("project");
        let mut replaced = false;
        let result = open_verified_root_directory_with_hook(&root_path, || {
            fs::rename(&ancestor, &displaced_parent)?;
            symlink(&replacement_parent, &ancestor)?;
            replaced = true;
            Ok(())
        });

        assert!(replaced, "test must replace the ancestor after validation");
        assert!(
            result.is_err(),
            "root acquisition must reject a different directory reached through a replaced ancestor"
        );

        drop(result);
        fs::remove_file(&ancestor).unwrap();
        fs::remove_dir_all(displaced_parent).unwrap();
        fs::remove_dir_all(replacement_parent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verified_root_handle_rejects_ancestor_replaced_after_validation() {
        use std::process::Command;

        let ancestor = make_test_directory("ancestor-race-root");
        let replacement_parent = ancestor.with_file_name(format!(
            "friday-capability-walk-ancestor-race-replacement-{}",
            std::process::id()
        ));
        let displaced_parent = ancestor.with_file_name(format!(
            "friday-capability-walk-ancestor-race-displaced-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&replacement_parent);
        let _ = fs::remove_dir_all(&displaced_parent);
        fs::create_dir_all(ancestor.join("project")).unwrap();
        fs::create_dir_all(replacement_parent.join("project")).unwrap();
        fs::write(ancestor.join("project").join("authorized.txt"), b"original").unwrap();
        fs::write(replacement_parent.join("project").join("replacement.txt"), b"replacement").unwrap();

        let root_path = ancestor.join("project");
        let mut replaced = false;
        let result = open_verified_root_directory_with_hook(&root_path, || {
            fs::rename(&ancestor, &displaced_parent)?;
            let output = Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&ancestor)
                .arg(&replacement_parent)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("junction creation failed: {}", String::from_utf8_lossy(&output.stderr)),
                ));
            }
            replaced = true;
            Ok(())
        });

        assert!(replaced, "test must replace the ancestor after validation");
        assert!(
            result.is_err(),
            "root acquisition must reject a different directory reached through a replaced ancestor"
        );

        drop(result);
        fs::remove_dir(&ancestor).unwrap();
        fs::remove_dir_all(displaced_parent).unwrap();
        fs::remove_dir_all(replacement_parent).unwrap();
    }

    #[test]
    fn verified_root_handle_rejects_non_absolute_paths() {
        assert!(open_verified_root_directory(Path::new(".")).is_err());
    }

    #[test]
    fn capability_walk_lists_nested_regular_files_without_ambient_child_paths() {
        let root_path = make_test_directory("nested");
        fs::create_dir_all(root_path.join("src").join("nested")).unwrap();
        fs::write(root_path.join("Cargo.toml"), b"[package]").unwrap();
        fs::write(root_path.join("src").join("lib.rs"), b"pub fn ok() {}").unwrap();
        fs::write(
            root_path.join("src").join("nested").join("mod.rs"),
            b"pub mod nested;",
        )
        .unwrap();

        let root = open_test_root(&root_path);
        let mut files = collect_regular_file_paths(&root, 12, 64_000).unwrap();
        files.sort();

        assert_eq!(
            files,
            vec![
                PathBuf::from("Cargo.toml"),
                PathBuf::from("src/lib.rs"),
                PathBuf::from("src/nested/mod.rs"),
            ]
        );
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn capability_walk_skips_symlinked_directories_and_files() {
        let root_path = make_test_directory("symlinks");
        let outside = make_test_directory("outside");
        fs::write(outside.join("secret.rs"), b"must not be listed").unwrap();
        std::os::unix::fs::symlink(&outside, root_path.join("linked-dir")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("secret.rs"),
            root_path.join("linked-file.rs"),
        )
        .unwrap();
        fs::write(root_path.join("safe.rs"), b"safe").unwrap();

        let root = open_test_root(&root_path);
        let files = collect_regular_file_paths(&root, 12, 64_000).unwrap();
        assert_eq!(files, vec![PathBuf::from("safe.rs")]);

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn capability_walk_skips_windows_symlinked_directories_and_files() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let root_path = make_test_directory("windows-symlinks");
        let outside = make_test_directory("windows-outside");
        fs::write(outside.join("secret.rs"), b"must not be listed").unwrap();
        symlink_dir(&outside, root_path.join("linked-dir")).unwrap();
        symlink_file(
            outside.join("secret.rs"),
            root_path.join("linked-file.rs"),
        )
        .unwrap();
        fs::write(root_path.join("safe.rs"), b"safe").unwrap();

        let root = open_test_root(&root_path);
        let files = collect_regular_file_paths(&root, 12, 64_000).unwrap();
        assert_eq!(files, vec![PathBuf::from("safe.rs")]);

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn capability_walk_rejects_directory_replaced_by_symlink_after_enumeration() {
        let root_path = make_test_directory("directory-swap");
        let outside = make_test_directory("directory-swap-outside");
        fs::create_dir(root_path.join("replace-me")).unwrap();
        fs::write(outside.join("secret.rs"), b"must not be reached").unwrap();

        let root = open_test_root(&root_path);
        let mut replaced = false;
        let result = {
            let mut hook = |relative: &Path| {
                if relative == Path::new("replace-me") && !replaced {
                    fs::remove_dir(root_path.join("replace-me"))?;
                    std::os::unix::fs::symlink(&outside, root_path.join("replace-me"))?;
                    replaced = true;
                }
                Ok(())
            };
            let mut files = Vec::new();
            let mut entries_scanned = 0usize;
            walk_directory(
                &root,
                Path::new(""),
                0,
                12,
                64_000,
                &mut entries_scanned,
                &mut files,
                &mut hook,
            )
        };

        assert!(replaced, "test must replace the directory after enumeration");
        assert!(result.is_err(), "no-follow directory open must fail closed");
        drop(root);
        fs::remove_file(root_path.join("replace-me")).unwrap();
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn capability_walk_never_traverses_windows_junctions() {
        use std::process::Command;

        let root_path = make_test_directory("windows-junction-root");
        let outside = make_test_directory("windows-junction-outside");
        fs::write(outside.join("secret.rs"), b"must never be listed").unwrap();
        fs::write(root_path.join("safe.rs"), b"safe").unwrap();

        let junction_path = root_path.join("junction");
        // Pass each mklink argument separately. A pre-quoted command string
        // is vulnerable to cmd.exe /C quote-stripping, especially on paths
        // containing spaces in hosted Windows runner temp directories.
        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction_path)
            .arg(&outside)
            .output()
            .expect("cmd.exe should be available on Windows");
        assert!(
            output.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let root = open_test_root(&root_path);
        let result = collect_regular_file_paths(&root, 12, 64_000);
        match result {
            Ok(files) => {
                assert!(files.contains(&PathBuf::from("safe.rs")));
                assert!(
                    !files.iter().any(|path| path.starts_with("junction")),
                    "junction contents must never be traversed"
                );
            }
            Err(_) => {
                // Fail-closed rejection of an unrecognised reparse point is safe.
            }
        }

        drop(root);
        fs::remove_dir(&junction_path).unwrap();
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn handle_relative_file_open_reads_regular_file_and_rejects_symlinks() {
        use std::io::Read;
        use std::os::windows::fs::symlink_file;

        let root_path = make_test_directory("handle-relative-file");
        let outside = make_test_directory("handle-relative-file-outside");
        fs::write(root_path.join("safe.txt"), b"safe content").unwrap();
        fs::write(outside.join("secret.txt"), b"outside secret").unwrap();
        symlink_file(outside.join("secret.txt"), root_path.join("linked.txt")).unwrap();

        let root = open_test_root(&root_path);
        let mut safe = open_file_nofollow(&root, OsStr::new("safe.txt")).unwrap();
        let mut contents = Vec::new();
        safe.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"safe content");

        let missing = open_file_nofollow(&root, OsStr::new("missing.txt")).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound,
            "a missing child must remain distinguishable from a denied/reparse-point open");
        let missing_bounded = read_bounded_relative_file(&root, "missing.txt", 4096).unwrap_err();
        assert_eq!(missing_bounded.kind(), std::io::ErrorKind::NotFound,
            "bounded handle-relative reads must recognize an absent generation");

        let linked = open_file_nofollow(&root, OsStr::new("linked.txt"));
        assert!(linked.is_err(), "file symlink must never be followed or accepted");
        assert!(open_file_nofollow(&root, OsStr::new("../secret.txt")).is_err());

        drop(safe);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn handle_relative_file_open_rejects_file_replaced_by_symlink_after_enumeration() {
        use std::os::windows::fs::symlink_file;

        let root_path = make_test_directory("windows-file-swap");
        let outside = make_test_directory("windows-file-swap-outside");
        fs::write(root_path.join("replace-me.txt"), b"original").unwrap();
        fs::write(outside.join("secret.txt"), b"outside secret").unwrap();

        let root = open_test_root(&root_path);
        // Deterministically model the gap between observing a regular entry
        // and opening it: replace the name with a link before the handle-relative
        // open. The open must fail rather than reach the outside target.
        fs::remove_file(root_path.join("replace-me.txt")).unwrap();
        symlink_file(outside.join("secret.txt"), root_path.join("replace-me.txt")).unwrap();
        assert!(open_file_nofollow(&root, OsStr::new("replace-me.txt")).is_err());

        drop(root);
        fs::remove_file(root_path.join("replace-me.txt")).unwrap();
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn created_destination_directory_handle_supports_exact_object_permissions() {
        use std::os::unix::{fs::PermissionsExt, io::AsRawFd};
        let root_path = make_test_directory("destination-directory-fchmod");
        let root = open_verified_root_directory(&root_path).unwrap();
        let child = create_destination_entry(&root, OsStr::new("private-child"), true).unwrap();
        assert!(child.metadata().unwrap().is_dir());
        assert_eq!(unsafe { libc::fchmod(child.as_raw_fd(), 0o700) }, 0);
        assert_eq!(child.metadata().unwrap().permissions().mode() & 0o077, 0);
        drop(child);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_open_file_nofollow_reads_regular_file_relative_to_parent_handle() {
        use std::ffi::OsStr;

        let root_path = make_test_directory("unix-relative-file-open");
        fs::write(root_path.join("safe.txt"), b"safe contents").unwrap();
        let root = open_test_root(&root_path);

        let mut file = open_file_nofollow(&root, OsStr::new("safe.txt")).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"safe contents");

        drop(file);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_open_file_nofollow_rejects_symlink_and_non_normal_names() {
        use std::{ffi::OsStr, os::unix::fs::symlink};

        let root_path = make_test_directory("unix-relative-file-reject");
        let outside = make_test_directory("unix-relative-file-outside");
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        symlink(outside.join("secret.txt"), root_path.join("linked.txt")).unwrap();
        let root = open_test_root(&root_path);

        assert!(open_file_nofollow(&root, OsStr::new("linked.txt")).is_err());
        assert!(open_file_nofollow(&root, OsStr::new("../secret.txt")).is_err());
        assert!(open_file_nofollow(&root, OsStr::new("")).is_err());

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn handle_relative_file_rename_moves_only_the_verified_file_without_overwrite() {
        let root_path = make_test_directory("handle-relative-file-rename");
        fs::write(root_path.join("source.txt"), b"verified source").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let parent_identity = verified_directory_handle_identity(&root).unwrap();
        let source_identity = enumerate_runtime_tree_entries(&root, 16).unwrap()
            .into_iter()
            .find(|entry| entry.name == OsStr::new("source.txt"))
            .expect("source should be inventoried")
            .identity;

        rename_file_noreplace(
            &root, OsStr::new("source.txt"), OsStr::new("published.txt"),
            parent_identity, source_identity,
        ).unwrap();

        let mut published = open_result_file_nofollow(&root, OsStr::new("published.txt")).unwrap();
        let mut contents = Vec::new();
        published.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"verified source");
        assert_eq!(
            open_result_file_nofollow(&root, OsStr::new("source.txt")).unwrap_err().kind(),
            io::ErrorKind::NotFound,
        );

        drop(published);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn handle_relative_file_rename_refuses_existing_destination() {
        let root_path = make_test_directory("handle-relative-file-rename-no-replace");
        fs::write(root_path.join("source.txt"), b"source").unwrap();
        fs::write(root_path.join("destination.txt"), b"keep destination").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let parent_identity = verified_directory_handle_identity(&root).unwrap();
        let source_identity = enumerate_runtime_tree_entries(&root, 16).unwrap()
            .into_iter()
            .find(|entry| entry.name == OsStr::new("source.txt"))
            .expect("source should be inventoried")
            .identity;

        let result = rename_file_noreplace(
            &root, OsStr::new("source.txt"), OsStr::new("destination.txt"),
            parent_identity, source_identity,
        );
        assert!(result.is_err(), "a no-replace rename must not overwrite an existing entry");

        let source = fs::read(root_path.join("source.txt")).unwrap();
        let destination = fs::read(root_path.join("destination.txt")).unwrap();
        assert_eq!(source, b"source");
        assert_eq!(destination, b"keep destination");

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn handle_relative_file_rename_rejects_source_replaced_after_inventory() {
        let root_path = make_test_directory("handle-relative-file-rename-source-identity");
        let source_path = root_path.join("source.txt");
        let displaced_path = root_path.join("displaced.txt");
        fs::write(&source_path, b"approved source").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let parent_identity = verified_directory_handle_identity(&root).unwrap();
        let source_identity = enumerate_runtime_tree_entries(&root, 16).unwrap()
            .into_iter().find(|entry| entry.name == OsStr::new("source.txt"))
            .expect("source should be inventoried").identity;
        fs::rename(&source_path, &displaced_path).unwrap();
        fs::write(&source_path, b"unapproved replacement").unwrap();
        let result = rename_file_noreplace(&root, OsStr::new("source.txt"),
            OsStr::new("published.txt"), parent_identity, source_identity);
        assert!(result.is_err(), "a rebound source name must be rejected");
        assert_eq!(fs::read(&source_path).unwrap(), b"unapproved replacement");
        assert_eq!(fs::read(&displaced_path).unwrap(), b"approved source");
        assert!(!root_path.join("published.txt").exists());
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn handle_relative_file_rename_rejects_changed_parent_identity() {
        let root_path = make_test_directory("handle-relative-file-rename-parent-identity");
        fs::write(root_path.join("source.txt"), b"source").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let source_identity = enumerate_runtime_tree_entries(&root, 16).unwrap()
            .into_iter()
            .find(|entry| entry.name == OsStr::new("source.txt"))
            .expect("source should be inventoried")
            .identity;

        let result = rename_file_noreplace(
            &root, OsStr::new("source.txt"), OsStr::new("published.txt"),
            (0, 0), source_identity,
        );
        assert!(result.is_err(), "a changed parent identity must fail closed");
        assert!(root_path.join("source.txt").exists());
        assert!(!root_path.join("published.txt").exists());

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_open_file_nofollow_stays_with_held_parent_after_rename() {
        use std::{ffi::OsStr, os::unix::fs::symlink};

        let base = make_test_directory("unix-relative-file-parent-race");
        let parent_path = base.join("parent");
        let replacement = base.join("replacement");
        let displaced = base.join("displaced");
        fs::create_dir(&parent_path).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(parent_path.join("data.txt"), b"authorized").unwrap();
        fs::write(replacement.join("data.txt"), b"outside").unwrap();

        let parent = open_test_root(&parent_path);
        fs::rename(&parent_path, &displaced).unwrap();
        symlink(&replacement, &parent_path).unwrap();

        let mut file = open_file_nofollow(&parent, OsStr::new("data.txt")).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(
            contents,
            b"authorized",
            "a replaced parent path must not redirect an open relative to its held handle"
        );

        drop(file);
        drop(parent);
        fs::remove_file(&parent_path).unwrap();
        fs::remove_dir_all(displaced).unwrap();
        fs::remove_dir_all(replacement).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_open_directory_handle_prevents_rename_until_closed() {
        let root_path = make_test_directory("windows-handle-sharing");
        let child_path = root_path.join("child");
        let renamed_path = root_path.join("renamed");
        fs::create_dir(&child_path).unwrap();

        let root = open_test_root(&root_path);
        let child = open_dir_nofollow(&root, Path::new("child")).unwrap();

        // Windows directory handles used for safe traversal must not grant
        // FILE_SHARE_DELETE; otherwise an attacker may rename/replace the
        // directory while FRIDAY is traversing through the held handle.
        let rename_while_open = fs::rename(&child_path, &renamed_path);
        drop(child);

        if rename_while_open.is_ok() {
            fs::rename(&renamed_path, &child_path).unwrap();
            drop(root);
            fs::remove_dir_all(root_path).unwrap();
            panic!("open directory handle allowed rename; FILE_SHARE_DELETE must be denied");
        }

        // Once the handle is closed, the same rename should work normally.
        fs::rename(&child_path, &renamed_path).unwrap();
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn capability_walk_rejects_directory_replaced_by_windows_symlink_after_enumeration() {
        use std::os::windows::fs::symlink_dir;

        let root_path = make_test_directory("windows-directory-swap");
        let outside = make_test_directory("windows-directory-swap-outside");
        fs::create_dir(root_path.join("replace-me")).unwrap();
        fs::write(outside.join("secret.rs"), b"must not be reached").unwrap();

        let root = open_test_root(&root_path);
        let mut replaced = false;
        let result = {
            let mut hook = |relative: &Path| {
                if relative == Path::new("replace-me") && !replaced {
                    fs::remove_dir(root_path.join("replace-me"))?;
                    symlink_dir(&outside, root_path.join("replace-me"))?;
                    replaced = true;
                }
                Ok(())
            };
            let mut files = Vec::new();
            let mut entries_scanned = 0usize;
            walk_directory(
                &root,
                Path::new(""),
                0,
                12,
                64_000,
                &mut entries_scanned,
                &mut files,
                &mut hook,
            )
        };

        assert!(replaced, "test must replace the directory after enumeration");
        assert!(result.is_err(), "no-follow directory open must reject the replacement");
        drop(root);
        // Windows removes directory symlinks with remove_dir, not remove_file.
        fs::remove_dir(root_path.join("replace-me")).unwrap();
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn cleanup_manifest_round_trips_exact_payload_inventory() {
        let contents = BoundedSnapshotContents { files: vec![
            BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"abc".to_vec() },
            BoundedSnapshotFile { relative_path: PathBuf::from("src/nested/lib.rs"), contents: b"12345".to_vec() },
        ], bytes: 8, truncated: false };
        let id = "a".repeat(32);
        let manifest = build_snapshot_cleanup_manifest(&id, &contents, bounded_limits()).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let parsed = validate_snapshot_cleanup_manifest(&bytes, &id, bounded_limits()).unwrap();
        assert_eq!(parsed.directories, vec!["src", "src/nested"]);
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[1].path, "src/nested/lib.rs");
        assert_eq!(parsed.files[1].size, 5);
    }

    #[test]
    fn cleanup_manifest_rejects_legacy_unknown_malformed_and_mismatched_markers() {
        let id = "b".repeat(32);
        let legacy = br#"{"schemaVersion":1,"snapshotId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","owner":"FRIDAY-CodingAssistant"}"#;
        assert!(validate_snapshot_cleanup_manifest(legacy, &id, bounded_limits()).is_err());
        let unknown = br#"{"schemaVersion":99,"snapshotId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","owner":"FRIDAY-CodingAssistant","directories":[],"files":[]}"#;
        assert!(validate_snapshot_cleanup_manifest(unknown, &id, bounded_limits()).is_err());
        assert!(validate_snapshot_cleanup_manifest(b"{malformed", &id, bounded_limits()).is_err());
        let contents = BoundedSnapshotContents { files: vec![BoundedSnapshotFile { relative_path: PathBuf::from("file.txt"), contents: b"x".to_vec() }], bytes: 1, truncated: false };
        let manifest = build_snapshot_cleanup_manifest(&id, &contents, bounded_limits()).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        assert!(validate_snapshot_cleanup_manifest(&bytes, &"c".repeat(32), bounded_limits()).is_err());
    }

    #[test]
    fn cleanup_manifest_rejects_duplicate_paths_unknown_fields_and_traversal() {
        let id = "d".repeat(32);
        let duplicate = format!(r#"{{"schemaVersion":2,"snapshotId":"{id}","owner":"FRIDAY-CodingAssistant","directories":[],"files":[{{"path":"same.txt","size":1}},{{"path":"same.txt","size":1}}]}}"#);
        assert!(validate_snapshot_cleanup_manifest(duplicate.as_bytes(), &id, bounded_limits()).is_err());
        let unknown_field = format!(r#"{{"schemaVersion":2,"snapshotId":"{id}","owner":"FRIDAY-CodingAssistant","directories":[],"files":[],"deleteEverything":true}}"#);
        assert!(validate_snapshot_cleanup_manifest(unknown_field.as_bytes(), &id, bounded_limits()).is_err());
        let traversal = format!(r#"{{"schemaVersion":2,"snapshotId":"{id}","owner":"FRIDAY-CodingAssistant","directories":[],"files":[{{"path":"../outside.txt","size":1}}]}}"#);
        assert!(validate_snapshot_cleanup_manifest(traversal.as_bytes(), &id, bounded_limits()).is_err());
    }

    #[test]
    fn destination_snapshot_creates_nested_files_exclusively_and_marks_only_complete_snapshot() {
        let workspace_path = make_test_directory("destination-create");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("src/lib.rs"), contents: b"pub fn friday() {}".to_vec() },
            ],
            bytes: b"[package]".len() as u64 + b"pub fn friday() {}".len() as u64,
            truncated: false,
        };
        let id = "a".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        assert_eq!(fs::read(workspace_path.join(&id).join("Cargo.toml")).unwrap(), b"[package]");
        assert_eq!(fs::read(workspace_path.join(&id).join("src/lib.rs")).unwrap(), b"pub fn friday() {}");
        let marker = fs::read_to_string(workspace_path.join(&id).join(".friday-snapshot-owner.json")).unwrap();
        assert!(marker.contains(&id));
        assert!(marker.contains("FRIDAY-CodingAssistant"));
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn bounded_directory_entry_name_collection_accepts_boundary_and_rejects_overflow() {
        let path = make_test_directory("bounded-directory-entry-names");
        fs::write(path.join("one"), b"1").unwrap();
        fs::write(path.join("two"), b"2").unwrap();
        let root = open_verified_root_directory(&path).unwrap();

        assert_eq!(collect_bounded_directory_entry_names(&root, 2).unwrap().len(), 2);
        let error = collect_bounded_directory_entry_names(&root, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let error = collect_bounded_directory_entry_names(&root, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        drop(root);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn snapshot_preflight_accepts_exact_inventory_and_rejects_unknown_entries() {
        let workspace_path = make_test_directory("preflight-exact");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("src/main.rs"), contents: b"fn main() {}".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
            ],
            bytes: b"fn main() {}".len() as u64 + b"[package]".len() as u64,
            truncated: false,
        };
        let id = "e".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();
        preflight_snapshot_tree(&root, &id, bounded_limits()).unwrap();

        fs::write(workspace_path.join(&id).join("unexpected.txt"), b"do not delete").unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("unexpected.txt")).unwrap(), b"do not delete");
        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_inventory_and_cleanup_preflight_enforce_shared_nested_entry_budget() {
        let workspace_path = make_test_directory("snapshot-entry-budget-preflight");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("src/main.rs"), contents: b"fn main() {}".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
            ],
            bytes: b"fn main() {}".len() as u64 + b"[package]".len() as u64,
            truncated: false,
        };
        let id = "f".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();

        // The exact tree has four entries across two levels: marker, src,
        // Cargo.toml, and src/main.rs. The shared budget must not reset in src.
        let mut exact = bounded_limits();
        exact.max_entries = 4;
        preflight_snapshot_tree(&root, &id, exact).unwrap();
        preflight_snapshot_cleanup_progress(&root, &id, exact).unwrap();

        let mut too_small = exact;
        too_small.max_entries = 3;
        assert!(preflight_snapshot_tree(&root, &id, too_small).is_err());
        assert!(preflight_snapshot_cleanup_progress(&root, &id, too_small).is_err());

        // Cleanup must complete its entire bounded preflight before deleting
        // anything; an over-budget tree remains byte-for-byte available.
        assert!(cleanup_owned_snapshot(&workspace, &id, too_small).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("Cargo.toml")).unwrap(), b"[package]");
        assert_eq!(fs::read(workspace_path.join(&id).join("src/main.rs")).unwrap(), b"fn main() {}");
        assert!(workspace_path.join(&id).join(".friday-snapshot-owner.json").exists());

        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_file_cleanup_removes_only_matching_regular_files() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("cleanup-file-prototype");
        fs::write(workspace_path.join("known.txt"), b"known payload").unwrap();
        fs::write(workspace_path.join("unknown.txt"), b"keep me").unwrap();
        let parent = open_test_root(&workspace_path);

        assert!(remove_verified_snapshot_file(&parent, OsStr::new("known.txt"), 999).is_err());
        assert!(workspace_path.join("known.txt").exists(), "size mismatch must preserve the file");
        remove_verified_snapshot_file(&parent, OsStr::new("known.txt"), b"known payload".len() as u64).unwrap();
        assert!(!workspace_path.join("known.txt").exists());
        assert_eq!(fs::read(workspace_path.join("unknown.txt")).unwrap(), b"keep me");
        assert!(remove_verified_snapshot_file(&parent, OsStr::new("unknown.txt"), 1).is_err());

        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_snapshot_file_cleanup_deletes_only_matching_regular_files_by_handle() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("windows-cleanup-file-prototype");
        fs::write(workspace_path.join("known.txt"), b"known payload").unwrap();
        fs::write(workspace_path.join("unknown.txt"), b"keep me").unwrap();
        let parent = open_test_root(&workspace_path);

        assert!(remove_verified_snapshot_file(&parent, OsStr::new("known.txt"), 999).is_err());
        assert_eq!(fs::read(workspace_path.join("known.txt")).unwrap(), b"known payload");
        remove_verified_snapshot_file(&parent, OsStr::new("known.txt"), b"known payload".len() as u64).unwrap();
        assert!(!workspace_path.join("known.txt").exists());
        assert_eq!(fs::read(workspace_path.join("unknown.txt")).unwrap(), b"keep me");
        assert!(remove_verified_snapshot_file(&parent, OsStr::new("unknown.txt"), 1).is_err());

        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_file_cleanup_fails_closed_without_race_safe_unlink() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("runtime-cleanup-unix-fail-closed");
        fs::write(workspace_path.join("payload.bin"), b"authenticated payload").unwrap();
        let parent = open_test_root(&workspace_path);
        let error = remove_verified_runtime_file(
            &parent, OsStr::new("payload.bin"), 21,
            "d2f75693596e4fea04f110826bed8e76b2514463ca783f9a5150f7901c2bf92a",
        ).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(fs::read(workspace_path.join("payload.bin")).unwrap(), b"authenticated payload");
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_runtime_file_cleanup_requires_exact_size_and_sha256() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("windows-runtime-cleanup-digest");
        fs::write(workspace_path.join("payload.bin"), b"authenticated payload").unwrap();
        let parent = open_test_root(&workspace_path);
        let digest = "d2f75693596e4fea04f110826bed8e76b2514463ca783f9a5150f7901c2bf92a";
        assert!(remove_verified_runtime_file(&parent, OsStr::new("payload.bin"), 1, digest).is_err());
        assert!(remove_verified_runtime_file(&parent, OsStr::new("payload.bin"), 21, "0".repeat(64).as_str()).is_err());
        assert_eq!(fs::read(workspace_path.join("payload.bin")).unwrap(), b"authenticated payload");
        remove_verified_runtime_file(&parent, OsStr::new("payload.bin"), 21, digest).unwrap();
        assert!(!workspace_path.join("payload.bin").exists());
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_owner_marker_cleanup_rejects_substitution_and_fails_closed() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("runtime-owner-marker-unix");
        let marker = br#"{\"schemaVersion\":1,\"bundleId\":\"fixture\"}"#;
        fs::write(workspace_path.join(".friday-runtime-owner.json"), marker).unwrap();
        let parent = open_test_root(&workspace_path);
        assert!(remove_verified_runtime_owner_marker(
            &parent, OsStr::new("other.json"), marker.len() as u64,
            "0".repeat(64).as_str(),
        ).is_err());
        let error = remove_verified_runtime_owner_marker(
            &parent, OsStr::new(".friday-runtime-owner.json"), marker.len() as u64,
            "0".repeat(64).as_str(),
        ).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(fs::read(workspace_path.join(".friday-runtime-owner.json")).unwrap(), marker);
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_runtime_owner_marker_cleanup_requires_exact_reserved_name_size_and_digest() {
        use std::ffi::OsStr;
        use sha2::{Digest, Sha256};
        let workspace_path = make_test_directory("windows-runtime-owner-marker");
        let marker = br#"{\"schemaVersion\":1,\"bundleId\":\"fixture\"}"#;
        fs::write(workspace_path.join(".friday-runtime-owner.json"), marker).unwrap();
        let parent = open_test_root(&workspace_path);
        let digest = format!("{:x}", Sha256::digest(marker));
        assert!(remove_verified_runtime_owner_marker(
            &parent, OsStr::new("other.json"), marker.len() as u64, &digest,
        ).is_err());
        assert!(remove_verified_runtime_owner_marker(
            &parent, OsStr::new(".friday-runtime-owner.json"), marker.len() as u64 + 1, &digest,
        ).is_err());
        assert_eq!(fs::read(workspace_path.join(".friday-runtime-owner.json")).unwrap(), marker);
        remove_verified_runtime_owner_marker(
            &parent, OsStr::new(".friday-runtime-owner.json"), marker.len() as u64, &digest,
        ).unwrap();
        assert!(!workspace_path.join(".friday-runtime-owner.json").exists());
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_root_cleanup_fails_closed_and_requires_authenticated_identities() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("runtime-root-unix-fail-closed");
        fs::create_dir(workspace_path.join("staging-root")).unwrap();
        let parent = open_test_root(&workspace_path);
        let error = remove_verified_runtime_root_directory(
            &parent, OsStr::new("staging-root"), (1, 1), (2, 2),
        ).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(workspace_path.join("staging-root").is_dir());
        assert!(remove_verified_runtime_root_directory(
            &parent, OsStr::new("../outside"), (1, 1), (2, 2),
        ).is_err());
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_runtime_root_cleanup_binds_parent_root_identity_and_emptiness() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("windows-runtime-root-cleanup");
        fs::create_dir(workspace_path.join("staging-root")).unwrap();
        fs::create_dir(workspace_path.join("nonempty-root")).unwrap();
        fs::write(workspace_path.join("nonempty-root").join("keep.bin"), b"keep").unwrap();
        let parent = open_test_root(&workspace_path);
        let parent_id = handle_identity(&parent).unwrap();
        let root = open_child_directory_nofollow(&parent, OsStr::new("staging-root")).unwrap();
        let root_id = handle_identity(&root).unwrap();
        drop(root);

        assert!(remove_verified_runtime_root_directory(
            &parent, OsStr::new("staging-root"), (parent_id.0 as u64, parent_id.1),
            (root_id.0 as u64 + 1, root_id.1),
        ).is_err());
        assert!(workspace_path.join("staging-root").is_dir());
        assert!(remove_verified_runtime_root_directory(
            &parent, OsStr::new("staging-root"), (parent_id.0 as u64 + 1, parent_id.1),
            (root_id.0 as u64, root_id.1),
        ).is_err());
        assert!(workspace_path.join("staging-root").is_dir());
        assert!(remove_verified_runtime_root_directory(
            &parent, OsStr::new("nonempty-root"), (parent_id.0 as u64, parent_id.1),
            (root_id.0 as u64, root_id.1),
        ).is_err());
        assert!(workspace_path.join("nonempty-root").join("keep.bin").exists());
        remove_verified_runtime_root_directory(
            &parent, OsStr::new("staging-root"), (parent_id.0 as u64, parent_id.1),
            (root_id.0 as u64, root_id.1),
        ).unwrap();
        assert!(!workspace_path.join("staging-root").exists());
        assert!(workspace_path.join("nonempty-root").is_dir());
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_cleanup_fails_closed_without_race_safe_unlink() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("runtime-cleanup-directory-unix");
        fs::create_dir(workspace_path.join("empty")).unwrap();
        let parent = open_test_root(&workspace_path);
        let error = remove_verified_runtime_empty_directory(&parent, OsStr::new("empty")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(workspace_path.join("empty").is_dir());
        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_runtime_directory_cleanup_removes_only_verified_empty_directories() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("windows-runtime-cleanup-directory");
        fs::create_dir(workspace_path.join("empty")).unwrap();
        fs::create_dir(workspace_path.join("nonempty")).unwrap();
        fs::write(workspace_path.join("nonempty").join("keep.txt"), b"keep").unwrap();
        let parent = open_test_root(&workspace_path);

        assert!(remove_verified_runtime_empty_directory(&parent, OsStr::new("nonempty")).is_err());
        assert!(workspace_path.join("nonempty").join("keep.txt").exists());
        remove_verified_runtime_empty_directory(&parent, OsStr::new("empty")).unwrap();
        assert!(!workspace_path.join("empty").exists());
        assert!(workspace_path.join("nonempty").is_dir());
        assert!(remove_verified_runtime_empty_directory(&parent, OsStr::new("..")).is_err());

        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_directory_cleanup_removes_only_verified_empty_directory() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("cleanup-empty-directory");
        fs::create_dir(workspace_path.join("empty")).unwrap();
        fs::create_dir(workspace_path.join("nonempty")).unwrap();
        fs::write(workspace_path.join("nonempty").join("keep.txt"), b"keep").unwrap();
        let parent = open_test_root(&workspace_path);

        assert!(remove_verified_empty_snapshot_directory(&parent, OsStr::new("nonempty"), None).is_err());
        assert!(workspace_path.join("nonempty").join("keep.txt").exists());
        remove_verified_empty_snapshot_directory(&parent, OsStr::new("empty"), None).unwrap();
        assert!(!workspace_path.join("empty").exists());
        assert!(workspace_path.join("nonempty").exists());
        assert!(remove_verified_empty_snapshot_directory(&parent, OsStr::new(".."), None).is_err());

        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_snapshot_directory_cleanup_removes_only_verified_empty_directory_by_handle() {
        use std::ffi::OsStr;
        let workspace_path = make_test_directory("windows-cleanup-empty-directory");
        fs::create_dir(workspace_path.join("empty")).unwrap();
        fs::create_dir(workspace_path.join("nonempty")).unwrap();
        fs::write(workspace_path.join("nonempty").join("keep.txt"), b"keep").unwrap();
        let parent = open_test_root(&workspace_path);

        assert!(remove_verified_empty_snapshot_directory(&parent, OsStr::new("nonempty"), None).is_err());
        assert!(workspace_path.join("nonempty").join("keep.txt").exists());
        remove_verified_empty_snapshot_directory(&parent, OsStr::new("empty"), None).unwrap();
        assert!(!workspace_path.join("empty").exists());
        assert!(workspace_path.join("nonempty").exists());
        assert!(remove_verified_empty_snapshot_directory(&parent, OsStr::new(".."), None).is_err());

        drop(parent);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_preflight_rejects_symlink_entries() {
        use std::os::unix::fs::symlink;
        let workspace_path = make_test_directory("preflight-symlink");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile { relative_path: PathBuf::from("known.txt"), contents: b"known".to_vec() }],
            bytes: 5,
            truncated: false,
        };
        let id = "f".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();
        symlink(workspace_path.join("outside.txt"), workspace_path.join(&id).join("unexpected-link")).unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());
        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn destination_snapshot_never_overwrites_preexisting_directory() {
        let workspace_path = make_test_directory("destination-no-overwrite");
        let id = "b".repeat(32);
        fs::create_dir(workspace_path.join(&id)).unwrap();
        fs::write(workspace_path.join(&id).join("keep.txt"), b"unrelated").unwrap();
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile { relative_path: PathBuf::from("file.txt"), contents: b"new".to_vec() }],
            bytes: 3,
            truncated: false,
        };
        assert!(create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("keep.txt")).unwrap(), b"unrelated");
        assert!(!workspace_path.join(&id).join(".friday-snapshot-owner.json").exists());
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn destination_snapshot_rejects_traversal_before_creating_anything() {
        let workspace_path = make_test_directory("destination-traversal");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile { relative_path: PathBuf::from("../escape.txt"), contents: b"no".to_vec() }],
            bytes: 2,
            truncated: false,
        };
        let id = "c".repeat(32);
        assert!(create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).is_err());
        assert!(!workspace_path.join(&id).exists());
        assert!(!workspace_path.join("escape.txt").exists());
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn destination_snapshot_rejects_duplicate_paths_before_creating_anything() {
        let workspace_path = make_test_directory("destination-duplicate");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("same.txt"), contents: b"first".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("same.txt"), contents: b"second".to_vec() },
            ],
            bytes: 11,
            truncated: false,
        };
        let id = "d".repeat(32);
        assert!(create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).is_err());
        // Duplicate paths are rejected during inventory validation, before the
        // snapshot directory or either payload file can be created.
        assert!(!workspace_path.join(&id).exists());
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn capability_walk_rejects_non_single_component_names() {
        assert!(is_single_normal_component(OsStr::new("source.rs")));
        assert!(!is_single_normal_component(OsStr::new("..")));
        assert!(!is_single_normal_component(OsStr::new(".")));
        assert!(!is_single_normal_component(OsStr::new("../outside")));
        assert!(!is_single_normal_component(OsStr::new("nested/source.rs")));
        assert!(!is_single_normal_component(OsStr::new("/absolute/path")));
        #[cfg(windows)]
        assert!(!is_single_normal_component(OsStr::new("C:\\outside")));
    }

    #[test]
    fn snapshot_preflight_rejects_missing_and_modified_payload_files() {
        let workspace_path = make_test_directory("preflight-missing-modified");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("src/main.rs"), contents: b"fn main() {}".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
            ],
            bytes: b"fn main() {}".len() as u64 + b"[package]".len() as u64,
            truncated: false,
        };
        let id = "1".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();

        // Missing inventoried files must fail closed.
        fs::remove_file(workspace_path.join(&id).join("Cargo.toml")).unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());

        // Restore the expected name with the wrong size: it must still fail.
        fs::write(workspace_path.join(&id).join("Cargo.toml"), b"tampered-size").unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());

        // Restore the exact expected bytes; the same held root can be retried.
        fs::write(workspace_path.join(&id).join("Cargo.toml"), b"[package]").unwrap();
        preflight_snapshot_tree(&root, &id, bounded_limits()).unwrap();

        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_preflight_preserves_legacy_and_malformed_inventory_markers() {
        let workspace_path = make_test_directory("preflight-invalid-marker");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile { relative_path: PathBuf::from("keep.txt"), contents: b"preserve me".to_vec() }],
            bytes: b"preserve me".len() as u64,
            truncated: false,
        };
        let id = "2".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();
        let marker_path = workspace_path.join(&id).join(".friday-snapshot-owner.json");

        // A schema-1 marker is legacy evidence, not authorization under schema 2.
        fs::write(&marker_path, format!(
            r#"{{"schemaVersion":1,"snapshotId":"{}","owner":"FRIDAY-CodingAssistant"}}"#,
            id
        )).unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("keep.txt")).unwrap(), b"preserve me");

        // Malformed marker bytes must also fail closed without touching payloads.
        fs::write(&marker_path, b"{malformed").unwrap();
        assert!(preflight_snapshot_tree(&root, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("keep.txt")).unwrap(), b"preserve me");

        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }


    #[test]
    fn snapshot_cleanup_progress_preflight_accepts_only_missing_inventoried_entries() {
        let workspace_path = make_test_directory("cleanup-progress-preflight");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("src/main.rs"), contents: b"fn main() {}".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
            ],
            bytes: b"fn main() {}".len() as u64 + b"[package]".len() as u64,
            truncated: false,
        };
        let id = "3".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();

        // A crash may leave a subset of expected files/directories already gone.
        fs::remove_file(workspace_path.join(&id).join("src/main.rs")).unwrap();
        fs::remove_dir(workspace_path.join(&id).join("src")).unwrap();
        preflight_snapshot_cleanup_progress(&root, &id, bounded_limits()).unwrap();

        // Missing entries are tolerated, but new or modified entries are not.
        fs::write(workspace_path.join(&id).join("unexpected.txt"), b"preserve").unwrap();
        assert!(preflight_snapshot_cleanup_progress(&root, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(workspace_path.join(&id).join("unexpected.txt")).unwrap(), b"preserve");

        drop(root);
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }


    #[cfg(any(unix, windows))]
    #[test]
    fn snapshot_cleanup_refuses_replacement_at_final_directory_removal() {
        let workspace_path = make_test_directory("cleanup-final-root-replacement");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile {
                relative_path: PathBuf::from("main.rs"),
                contents: b"fn main() {}".to_vec(),
            }],
            bytes: b"fn main() {}".len() as u64,
            truncated: false,
        };
        let id = "a".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();

        let original_path = workspace_path.join(&id);
        let displaced_path = workspace_path.join(format!("{id}-displaced"));
        let mut hook_entered = false;
        let mut replaced = false;
        let mut rename_blocked = false;
        let result = cleanup_owned_snapshot_with_hook(&workspace, &id, bounded_limits(), |_| {
            hook_entered = true;
            if !replaced && !rename_blocked {
                match fs::rename(&original_path, &displaced_path) {
                    Ok(()) => {
                        fs::create_dir(&original_path).map_err(|error| {
                            io::Error::new(error.kind(), format!("final-root race hook replacement creation failed: {}", error))
                        })?;
                        replaced = true;
                    }
                    Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => {
                        // Windows correctly refuses to rename a directory while
                        // cleanup retains identity-bound handles that do not
                        // permit the rename. Treat that OS-level lock as the
                        // expected race outcome; never weaken those handles.
                        rename_blocked = true;
                    }
                    Err(error) => {
                        return Err(io::Error::new(error.kind(), format!("final-root race hook rename failed: {}", error)));
                    }
                }
            }
            Ok(())
        });

        assert!(hook_entered, "final-root race hook was never entered; cleanup result: {:?}", result);
        if cfg!(windows) {
            assert!(rename_blocked, "Windows should deny replacing the root while cleanup identity handles are retained");
            assert!(!replaced, "a replacement must not be created when Windows denies the rename");
            assert!(result.is_ok(), "cleanup should finish after the replacement attempt is safely denied: {:?}", result);
            assert!(!original_path.exists(), "the original snapshot should be removed after safe cleanup");
            assert!(!displaced_path.exists(), "a displaced snapshot should not exist when rename was denied");
        } else {
            assert!(result.is_err(), "cleanup must not remove a replacement at the original snapshot name");
            assert!(replaced, "the race hook should replace the root on platforms that permit the rename");
            assert!(original_path.is_dir(), "replacement directory must be preserved");
            assert!(displaced_path.is_dir(), "the inventoried directory must remain available for inspection");
        }
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_cleanup_rejects_file_replacement_after_preflight() {
        let workspace_path = make_test_directory("cleanup-file-identity-race");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("a.txt"), contents: b"first".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("b.txt"), contents: b"original".to_vec() },
            ],
            bytes: 13,
            truncated: false,
        };
        let id = "6".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = workspace_path.join(&id);
        let displaced = root.join("b.displaced");
        let mut replaced = false;

        let result = cleanup_owned_snapshot_with_hook(&workspace, &id, bounded_limits(), |path| {
            if path == "a.txt" && !replaced {
                fs::rename(root.join("b.txt"), &displaced)?;
                fs::write(root.join("b.txt"), b"replaced")?;
                replaced = true;
            }
            Ok(())
        });
        assert!(replaced, "the replacement must occur after preflight and the first deletion");
        assert!(matches!(result, Err(ref error) if error.kind() == io::ErrorKind::InvalidData));
        assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"replaced");
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
        assert!(root.join(".friday-snapshot-owner.json").exists());

        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_cleanup_rejects_parent_directory_replacement_after_preflight() {
        let workspace_path = make_test_directory("cleanup-parent-identity-race");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("a/first.txt"), contents: b"first".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("b/second.txt"), contents: b"original".to_vec() },
            ],
            bytes: 13,
            truncated: false,
        };
        let id = "7".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root = workspace_path.join(&id);
        let displaced = root.join("b.displaced");
        let mut hook_entered = false;
        let mut replaced = false;
        let mut rename_blocked = false;

        let result = cleanup_owned_snapshot_with_hook(&workspace, &id, bounded_limits(), |path| {
            if path == "a/first.txt" && !replaced && !rename_blocked {
                hook_entered = true;
                match fs::rename(root.join("b"), &displaced) {
                    Ok(()) => {
                        fs::create_dir(root.join("b")).map_err(|error| {
                            io::Error::new(error.kind(), format!("parent-directory race hook replacement creation failed: {}", error))
                        })?;
                        fs::write(root.join("b/second.txt"), b"replaced").map_err(|error| {
                            io::Error::new(error.kind(), format!("parent-directory race hook file creation failed: {}", error))
                        })?;
                        replaced = true;
                    }
                    Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied => {
                        // Retained identity handles intentionally prevent a
                        // Windows directory rename during the cleanup window.
                        rename_blocked = true;
                    }
                    Err(error) => {
                        return Err(io::Error::new(error.kind(), format!("parent-directory race hook rename failed: {}", error)));
                    }
                }
            }
            Ok(())
        });

        assert!(hook_entered, "parent replacement hook was never entered; cleanup result: {:?}", result);
        if cfg!(windows) {
            assert!(rename_blocked, "Windows should deny replacing a parent while its preflight identity handle is retained");
            assert!(!replaced, "a replacement must not be created when Windows denies the rename");
            assert!(result.is_ok(), "cleanup should finish after the replacement attempt is safely denied: {:?}", result);
            assert!(!root.exists(), "the original snapshot should be removed after safe cleanup");
            assert!(!displaced.exists(), "a displaced parent should not exist when rename was denied");
        } else {
            assert!(replaced, "the replacement must occur after preflight and the first deletion");
            assert!(matches!(result, Err(ref error) if error.kind() == io::ErrorKind::InvalidData));
            assert_eq!(fs::read(root.join("b/second.txt")).unwrap(), b"replaced");
            assert_eq!(fs::read(displaced.join("second.txt")).unwrap(), b"original");
            assert!(root.join(".friday-snapshot-owner.json").exists());
        }

        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_cleanup_resumes_after_interruption_and_removes_marker_last() {
        let workspace_path = make_test_directory("cleanup-retry");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![
                BoundedSnapshotFile { relative_path: PathBuf::from("src/main.rs"), contents: b"fn main() {}".to_vec() },
                BoundedSnapshotFile { relative_path: PathBuf::from("Cargo.toml"), contents: b"[package]".to_vec() },
            ],
            bytes: b"fn main() {}".len() as u64 + b"[package]".len() as u64,
            truncated: false,
        };
        let id = "4".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();

        let mut interrupted = false;
        let first = cleanup_owned_snapshot_with_hook(&workspace, &id, bounded_limits(), |_| {
            if !interrupted {
                interrupted = true;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "simulated interruption"));
            }
            Ok(())
        });
        assert_eq!(first.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let root = open_dir_nofollow(&workspace, Path::new(&id)).unwrap();
        assert!(preflight_snapshot_cleanup_progress(&root, &id, bounded_limits()).is_ok());
        drop(root);

        cleanup_owned_snapshot(&workspace, &id, bounded_limits()).unwrap();
        assert!(open_dir_nofollow(&workspace, Path::new(&id)).is_err());
        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn snapshot_cleanup_preserves_unexpected_entries_and_legacy_markers() {
        let workspace_path = make_test_directory("cleanup-preserve");
        let workspace = open_verified_root_directory(&workspace_path).unwrap();
        let contents = BoundedSnapshotContents {
            files: vec![BoundedSnapshotFile {
                relative_path: PathBuf::from("main.rs"),
                contents: b"fn main() {}".to_vec(),
            }],
            bytes: b"fn main() {}".len() as u64,
            truncated: false,
        };
        let id = "5".repeat(32);
        create_bounded_snapshot_destination(&workspace, &id, &contents, bounded_limits()).unwrap();
        let root_path = workspace_path.join(&id);
        let marker_path = root_path.join(".friday-snapshot-owner.json");
        let valid_marker = fs::read(&marker_path).unwrap();
        fs::write(&marker_path, format!(
            r#"{{"schemaVersion":1,"snapshotId":"{}","owner":"FRIDAY-CodingAssistant"}}"#,
            id
        )).unwrap();
        assert!(cleanup_owned_snapshot(&workspace, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(root_path.join("main.rs")).unwrap(), b"fn main() {}");
        fs::write(&marker_path, valid_marker).unwrap();
        fs::write(root_path.join("unrecognized.txt"), b"keep").unwrap();
        assert!(cleanup_owned_snapshot(&workspace, &id, bounded_limits()).is_err());
        assert_eq!(fs::read(root_path.join("main.rs")).unwrap(), b"fn main() {}");
        assert_eq!(fs::read(root_path.join("unrecognized.txt")).unwrap(), b"keep");

        drop(workspace);
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn sandbox_mapping_preflight_accepts_only_exact_complete_owned_snapshot() {
        let base = make_test_directory("sandbox-map-preflight");
        let source = base.join("source");
        let workspace = base.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(source.join("main.rs"), b"fn main() {}").unwrap();
        let snapshot_id = "0123456789abcdef0123456789abcdef";
        prepare_snapshot_from_paths(
            &source, &workspace, snapshot_id,
            (12, 2000, 1_048_576, 32 * 1024 * 1024),
            |_| true, |_| false,
        ).unwrap();
        let verified = verify_snapshot_for_sandbox_mapping(
            &workspace, snapshot_id, (12, 2000, 1_048_576, 32 * 1024 * 1024),
        ).unwrap();
        assert_eq!(verified, fs::canonicalize(workspace.join(snapshot_id)).unwrap());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_mapping_preflight_rejects_tampered_or_unexpected_snapshot_entries() {
        let base = make_test_directory("sandbox-map-preflight-tamper");
        let source = base.join("source");
        let workspace = base.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(source.join("main.rs"), b"fn main() {}").unwrap();
        let snapshot_id = "fedcba9876543210fedcba9876543210";
        prepare_snapshot_from_paths(
            &source, &workspace, snapshot_id,
            (12, 2000, 1_048_576, 32 * 1024 * 1024),
            |_| true, |_| false,
        ).unwrap();
        let snapshot = workspace.join(snapshot_id);
        fs::write(snapshot.join("main.rs"), b"tampered").unwrap();
        assert!(verify_snapshot_for_sandbox_mapping(
            &workspace, snapshot_id, (12, 2000, 1_048_576, 32 * 1024 * 1024),
        ).is_err());
        fs::remove_file(snapshot.join("main.rs")).unwrap();
        fs::write(snapshot.join("unexpected.txt"), b"unexpected").unwrap();
        assert!(verify_snapshot_for_sandbox_mapping(
            &workspace, snapshot_id, (12, 2000, 1_048_576, 32 * 1024 * 1024),
        ).is_err());
        assert!(verify_snapshot_for_sandbox_mapping(
            &workspace, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", (12, 2000, 1_048_576, 32 * 1024 * 1024),
        ).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn handle_based_cleanup_cannot_be_redirected_by_workspace_path_replacement() {
        let base = make_test_directory("cleanup-held-workspace-replacement");
        let workspace_path = base.join("workspace");
        let moved_workspace_path = base.join("moved-workspace");
        let source_path = base.join("source");
        fs::create_dir(&workspace_path).unwrap();
        fs::create_dir(&source_path).unwrap();
        fs::write(source_path.join("main.rs"), b"fn main() {}").unwrap();

        let id = "7".repeat(32);
        let limits = (12, 2_000, 1024 * 1024, 32 * 1024 * 1024);
        prepare_snapshot_from_paths(
            &source_path, &workspace_path, &id, limits,
            |name| name == "main.rs", |_| false,
        ).unwrap();

        let held_workspace = open_verified_root_directory(&workspace_path).unwrap();
        fs::rename(&workspace_path, &moved_workspace_path).unwrap();
        fs::create_dir(&workspace_path).unwrap();
        fs::create_dir(workspace_path.join(&id)).unwrap();
        fs::write(workspace_path.join(&id).join("keep.txt"), b"replacement data").unwrap();

        cleanup_snapshot_from_workspace_handle(&held_workspace, &id, limits).unwrap();

        assert!(!moved_workspace_path.join(&id).exists(),
            "cleanup must target the snapshot beneath the originally held workspace");
        assert_eq!(
            fs::read(workspace_path.join(&id).join("keep.txt")).unwrap(),
            b"replacement data",
            "a replacement workspace at the old path must remain untouched",
        );

        drop(held_workspace);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn production_snapshot_entrypoints_prepare_and_remove_complete_inventory() {
        let source_path = make_test_directory("production-lifecycle-source");
        let workspace_path = make_test_directory("production-lifecycle-workspace");
        fs::create_dir_all(source_path.join("src")).unwrap();
        fs::write(source_path.join("Cargo.toml"), b"[package]").unwrap();
        fs::write(source_path.join("src").join("main.rs"), b"fn main() {}").unwrap();

        let id = "6".repeat(32);
        let limits = (12, 2_000, 1024 * 1024, 32 * 1024 * 1024);
        let (files, bytes) = prepare_snapshot_from_paths(
            &source_path,
            &workspace_path,
            &id,
            limits,
            |name| name.ends_with(".rs") || name == "Cargo.toml",
            |_| false,
        ).unwrap();

        assert_eq!(files, 2);
        assert_eq!(bytes, b"[package]".len() as u64 + b"fn main() {}".len() as u64);
        let snapshot = workspace_path.join(&id);
        assert_eq!(fs::read(snapshot.join("Cargo.toml")).unwrap(), b"[package]");
        assert_eq!(fs::read(snapshot.join("src").join("main.rs")).unwrap(), b"fn main() {}");
        let marker: serde_json::Value = serde_json::from_slice(
            &fs::read(snapshot.join(".friday-snapshot-owner.json")).unwrap()
        ).unwrap();
        assert_eq!(marker["schemaVersion"], 2);
        assert_eq!(marker["snapshotId"], id);

        cleanup_snapshot_from_workspace_path(&workspace_path, &id, limits).unwrap();
        assert!(!snapshot.exists());
        fs::remove_dir_all(source_path).unwrap();
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn production_snapshot_rejects_source_file_with_external_hard_link() {
        let base = make_test_directory("production-source-hardlink");
        let source = base.join("source");
        let workspace = base.join("workspace");
        let external = base.join("outside.rs");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(&external, b"content outside selected project").unwrap();
        fs::hard_link(&external, source.join("linked.rs")).unwrap();

        let id = "b".repeat(32);
        let result = prepare_snapshot_from_paths(
            &source,
            &workspace,
            &id,
            (12, 2_000, 1024 * 1024, 32 * 1024 * 1024),
            |_| true,
            |_| false,
        );

        assert!(
            result.is_err(),
            "a source file with an external hard-link alias must be rejected"
        );
        assert!(
            !workspace.join(&id).exists(),
            "a rejected hard-linked source must never be published"
        );
        assert_eq!(fs::read(&external).unwrap(), b"content outside selected project");
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn bounded_snapshot_rejects_same_name_file_replacement_after_enumeration() {
        let root_path = make_test_directory("snapshot-source-identity-swap");
        let file_path = root_path.join("payload.txt");
        let held_path = root_path.join("held-original.txt");
        fs::write(&file_path, b"original").unwrap();
        let root = open_test_root(&root_path);
        let mut original_handle = None;

        let result = collect_bounded_snapshot_contents_with_hook(
            &root,
            bounded_limits(),
            &|_| true,
            &|_| false,
            |relative| {
                if relative == Path::new("payload.txt") {
                    // Keep the enumerated object alive so its filesystem
                    // identity cannot be recycled before the replacement opens.
                    let original = open_file_nofollow(&root, OsStr::new("payload.txt"))?;
                    fs::rename(&file_path, &held_path)?;
                    fs::write(&file_path, b"replacement")?;
                    original_handle = Some(original);
                }
                Ok(())
            },
        );

        assert!(
            result.is_err(),
            "snapshot collection must reject a regular file substituted after enumeration"
        );
        assert!(original_handle.is_some());
        drop(original_handle);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn bounded_snapshot_rejects_hard_link_added_after_enumeration() {
        let base = make_test_directory("bounded-snapshot-hardlink-race");
        let source = base.join("source");
        let external = base.join("outside.rs");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("raced.rs"), b"selected project content").unwrap();
        let root = open_verified_root_directory(&source).unwrap();
        let mut linked = false;

        // The collector has already observed a regular directory entry when
        // this hook runs. Create a second name before its handle-relative open:
        // the opened inode must be rejected based on its live link count.
        let result = collect_bounded_snapshot_contents_with_hook(
            &root,
            bounded_limits(),
            &|name| name == "raced.rs",
            &|_: &str| false,
            |relative| {
                if relative == Path::new("raced.rs") && !linked {
                    fs::hard_link(source.join("raced.rs"), &external)?;
                    linked = true;
                }
                Ok(())
            },
        );

        assert!(linked, "the hard-link race must occur after enumeration");
        assert!(
            matches!(result, Err(ref error) if error.kind() == io::ErrorKind::InvalidData),
            "a hard link introduced between enumeration and open must fail closed"
        );
        assert_eq!(fs::read(&external).unwrap(), b"selected project content");

        drop(root);
        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn production_snapshot_aborts_if_source_root_is_replaced_after_collection() {
        let base = make_test_directory("production-source-root-race");
        let source = base.join("source");
        let replacement = base.join("replacement");
        let displaced = base.join("displaced");
        let workspace = base.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(source.join("approved.rs"), b"approved source").unwrap();
        fs::write(replacement.join("outside.rs"), b"replacement source").unwrap();

        let result = prepare_snapshot_from_paths_with_hook(
            &source, &workspace, "9".repeat(32).as_str(),
            (12, 2_000, 1024 * 1024, 32 * 1024 * 1024),
            |_| true, |_| false,
            || {
                fs::rename(&source, &displaced)?;
                fs::rename(&replacement, &source)?;
                Ok(())
            },
        );
        assert!(result.is_err(), "a replaced source root must invalidate snapshot publication");
        assert!(!workspace.join("9".repeat(32)).exists());
        assert_eq!(fs::read(displaced.join("approved.rs")).unwrap(), b"approved source");
        assert_eq!(fs::read(source.join("outside.rs")).unwrap(), b"replacement source");
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn production_snapshot_aborts_if_workspace_root_is_replaced_after_collection() {
        let base = make_test_directory("production-workspace-root-race");
        let source = base.join("source");
        let workspace = base.join("workspace");
        let replacement = base.join("replacement-workspace");
        let displaced = base.join("displaced-workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(source.join("approved.rs"), b"approved source").unwrap();

        let id = "a".repeat(32);
        let result = prepare_snapshot_from_paths_with_hook(
            &source, &workspace, &id,
            (12, 2_000, 1024 * 1024, 32 * 1024 * 1024),
            |_| true, |_| false,
            || {
                fs::rename(&workspace, &displaced)?;
                fs::rename(&replacement, &workspace)?;
                Ok(())
            },
        );
        assert!(result.is_err(), "a replaced workspace root must invalidate snapshot publication");
        assert!(!workspace.join(&id).exists());
        assert!(fs::read_dir(&displaced).unwrap().next().is_none());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn production_snapshot_entrypoint_never_publishes_truncated_collection() {
        let source_path = make_test_directory("production-truncated-source");
        let workspace_path = make_test_directory("production-truncated-workspace");
        fs::write(source_path.join("a.rs"), b"fn a() {}").unwrap();
        fs::write(source_path.join("b.rs"), b"fn b() {}").unwrap();

        let id = "7".repeat(32);
        let result = prepare_snapshot_from_paths(
            &source_path,
            &workspace_path,
            &id,
            (12, 1, 1024, 1024),
            |name| name.ends_with(".rs"),
            |_| false,
        );
        assert!(result.is_err());
        assert!(!workspace_path.join(&id).exists(),
            "a truncated collection must fail before creating a destination");
        fs::remove_dir_all(source_path).unwrap();
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn production_cleanup_preserves_unmarked_interrupted_snapshot_data() {
        let workspace_path = make_test_directory("production-unmarked-preserve");
        let id = "8".repeat(32);
        let partial = workspace_path.join(&id);
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("partial.rs"), b"preserve after interrupted creation").unwrap();

        let result = cleanup_snapshot_from_workspace_path(
            &workspace_path,
            &id,
            (12, 2_000, 1024 * 1024, 32 * 1024 * 1024),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(partial.join("partial.rs")).unwrap(),
            b"preserve after interrupted creation",
            "unmarked data must never be treated as FRIDAY-owned cleanup authority"
        );
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn production_cleanup_rejects_invalid_snapshot_identifiers() {
        let workspace_path = make_test_directory("production-invalid-id");
        let result = cleanup_snapshot_from_workspace_path(
            &workspace_path,
            "../outside",
            (12, 2_000, 1024 * 1024, 32 * 1024 * 1024),
        );
        assert!(result.is_err());
        assert!(fs::read_dir(&workspace_path).unwrap().next().is_none());
        fs::remove_dir_all(workspace_path).unwrap();
    }

    #[test]
    fn runtime_staging_materializes_exact_nested_tree_and_writes_marker_last() {
        let root_path = make_test_directory("runtime-stage-success");
        let parent = open_verified_root_directory(&root_path).unwrap();
        let files = vec![
            (PathBuf::from("bin/cargo.exe"), b"cargo-bytes".to_vec()),
            (PathBuf::from("lib/rustlib/manifest"), b"rustlib".to_vec()),
        ];
        let marker = br#"{"schemaVersion":1,"bundleId":"fixture"}"#;
        create_bounded_runtime_destination(
            &parent, OsStr::new(".friday-fixture.staging"), &files, marker,
            8, 1024, 4096, 8,
        ).unwrap();

        let stage_path = root_path.join(".friday-fixture.staging");
        let stage = open_verified_root_directory(&stage_path).unwrap();
        let (actual_files, actual_dirs) =
            collect_runtime_tree_inventory(&stage, 8, 16).unwrap();
        assert_eq!(actual_files.len(), 3);
        assert!(actual_files.iter().any(|path| path == "bin\\cargo.exe"));
        assert!(actual_files.iter().any(|path| path == "lib\\rustlib\\manifest"));
        assert!(actual_files.iter().any(|path| path == ".friday-runtime-owner.json"));
        assert!(actual_dirs.iter().any(|path| path == "lib\\rustlib"));
        assert_eq!(
            read_bounded_relative_file(&stage, ".friday-runtime-owner.json", 1024).unwrap(),
            marker,
        );
        assert_eq!(
            read_bounded_relative_file(&stage, "bin\\cargo.exe", 1024).unwrap(),
            b"cargo-bytes",
        );
        drop(stage);
        drop(parent);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn runtime_staging_rejects_traversal_duplicates_and_existing_destinations() {
        let root_path = make_test_directory("runtime-stage-rejections");
        let parent = open_verified_root_directory(&root_path);
        let marker = b"owner";
        let traversal = vec![(PathBuf::from("../escape.bin"), b"bad".to_vec())];
        assert!(create_bounded_runtime_destination(
            &parent.as_ref().unwrap(), OsStr::new(".friday-bad.staging"), &traversal,
            marker, 8, 1024, 4096, 8,
        ).is_err());
        assert!(!root_path.join(".friday-bad.staging").exists());
        assert!(!root_path.join("escape.bin").exists());

        let duplicates = vec![
            (PathBuf::from("Bin/tool.exe"), b"one".to_vec()),
            (PathBuf::from("bin/TOOL.exe"), b"two".to_vec()),
        ];
        assert!(create_bounded_runtime_destination(
            &parent.as_ref().unwrap(), OsStr::new(".friday-duplicate.staging"), &duplicates,
            marker, 8, 1024, 4096, 8,
        ).is_err());
        assert!(!root_path.join(".friday-duplicate.staging").exists());

        fs::create_dir(root_path.join(".friday-existing.staging")).unwrap();
        fs::write(root_path.join(".friday-existing.staging").join("keep"), b"preserve").unwrap();
        let one = vec![(PathBuf::from("tool.exe"), b"new".to_vec())];
        assert!(create_bounded_runtime_destination(
            &parent.as_ref().unwrap(), OsStr::new(".friday-existing.staging"), &one,
            marker, 8, 1024, 4096, 8,
        ).is_err());
        assert_eq!(
            fs::read(root_path.join(".friday-existing.staging").join("keep")).unwrap(),
            b"preserve",
        );
        drop(parent);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn runtime_staging_interruption_leaves_unmarked_partial_data_untrusted_and_preserved() {
        let root_path = make_test_directory("runtime-stage-interrupted");
        let parent = open_verified_root_directory(&root_path).unwrap();
        let staging_name = OsStr::new(".friday-interrupted.staging");
        let files = vec![
            (PathBuf::from("bin/cargo.exe"), b"first-complete-artifact".to_vec()),
            (PathBuf::from("lib/rustlib/manifest"), b"second-artifact".to_vec()),
        ];
        let marker = br#"{"schemaVersion":1,"bundleId":"fixture"}"#;
        let mut completed = 0usize;

        let result = create_bounded_runtime_destination_with_hook(
            &parent, staging_name, &files, marker, 8, 1024, 4096, 8,
            |_| {
                completed += 1;
                if completed == 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "simulated interruption after first synced artifact",
                    ));
                }
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);

        let stage_path = root_path.join(staging_name);
        assert!(stage_path.is_dir(), "partial staging data is retained for safe recovery");
        assert_eq!(
            fs::read(stage_path.join("bin").join("cargo.exe")).unwrap(),
            b"first-complete-artifact",
        );
        assert!(!stage_path.join("lib").exists());
        assert!(
            !stage_path.join(".friday-runtime-owner.json").exists(),
            "the owner marker must never be written after an interrupted artifact sequence",
        );

        // No cleanup authority is inferred from a familiar staging name.
        // Preserve the unmarked data rather than deleting/adopting it.
        assert_eq!(
            fs::read(stage_path.join("bin").join("cargo.exe")).unwrap(),
            b"first-complete-artifact",
        );
        drop(parent);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn runtime_staging_rejects_size_and_marker_limits_before_creation() {
        let root_path = make_test_directory("runtime-stage-limits");
        let parent = open_verified_root_directory(&root_path).unwrap();
        let files = vec![(PathBuf::from("tool.exe"), b"12345".to_vec())];
        assert!(create_bounded_runtime_destination(
            &parent, OsStr::new(".friday-too-large.staging"), &files,
            b"owner", 8, 4, 4096, 8,
        ).is_err());
        assert!(create_bounded_runtime_destination(
            &parent, OsStr::new(".friday-marker-too-large.staging"), &files,
            &vec![b'x'; 16 * 1024 + 1], 8, 1024, 4096, 8,
        ).is_err());
        assert!(!root_path.join(".friday-too-large.staging").exists());
        assert!(!root_path.join(".friday-marker-too-large.staging").exists());
        drop(parent);
        fs::remove_dir_all(root_path).unwrap();
    }


    #[test]
    fn runtime_tree_inventory_accepts_exact_bounded_tree() {
        let root_path = make_test_directory("runtime-inventory-exact");
        fs::create_dir(root_path.join("bin")).unwrap();
        fs::create_dir(root_path.join("lib")).unwrap();
        fs::write(root_path.join("bin").join("runner.exe"), b"runner").unwrap();
        fs::write(root_path.join("lib").join("support.dll"), b"support").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();

        let (files, directories) = collect_runtime_tree_inventory(&root, 1, 4).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"bin\\runner.exe".to_owned()));
        assert!(files.contains(&"lib\\support.dll".to_owned()));
        assert_eq!(directories.len(), 2);
        assert!(directories.contains(&"bin".to_owned()));
        assert!(directories.contains(&"lib".to_owned()));

        assert!(
            collect_runtime_tree_inventory(&root, 1, 3).is_err(),
            "the entry budget must include directories and regular files"
        );
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn runtime_tree_inventory_rejects_same_name_file_replacement_after_enumeration() {
        let root_path = make_test_directory("runtime-inventory-windows-identity-swap");
        let file_path = root_path.join("tool.exe");
        fs::write(&file_path, b"original").unwrap();
        let root = open_test_root(&root_path);

        let entries = enumerate_runtime_tree_entries(&root, 8).unwrap();
        let observed = entries.iter()
            .find(|entry| entry.name == OsStr::new("tool.exe"))
            .expect("native enumeration should return the original file")
            .identity;

        // Keep the original object alive so the filesystem cannot recycle its
        // file ID while the directory name is replaced with a new object.
        let original = open_file_nofollow(&root, OsStr::new("tool.exe")).unwrap();
        fs::remove_file(&file_path).unwrap();
        fs::write(&file_path, b"replacement").unwrap();
        let replacement = open_file_nofollow(&root, OsStr::new("tool.exe")).unwrap();

        assert!(
            !runtime_tree_handle_matches(observed, &replacement).unwrap(),
            "the replacement handle must not match the file ID captured by native directory enumeration"
        );
        assert!(runtime_tree_handle_matches(observed, &original).unwrap());

        drop(replacement);
        drop(original);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn snapshot_inventory_rejects_same_name_file_replacement_after_enumeration() {
        let root_path = make_test_directory("snapshot-inventory-identity-swap");
        let file_path = root_path.join("payload.txt");
        fs::write(&file_path, b"original").unwrap();
        let root = open_test_root(&root_path);

        let entries = enumerate_runtime_tree_entries(&root, 8).unwrap();
        let observed = entries.iter()
            .find(|entry| entry.name == OsStr::new("payload.txt"))
            .expect("native enumeration should return the original file")
            .identity;

        // Keep the original file handle alive to prevent file-ID reuse.
        let original = open_file_nofollow(&root, OsStr::new("payload.txt")).unwrap();
        fs::remove_file(&file_path).unwrap();
        fs::write(&file_path, b"replacement").unwrap();
        let replacement = open_file_nofollow(&root, OsStr::new("payload.txt")).unwrap();

        assert!(!runtime_tree_handle_matches(observed, &replacement).unwrap());
        assert!(runtime_tree_handle_matches(observed, &original).unwrap());

        drop(replacement);
        drop(original);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn runtime_tree_inventory_rejects_excessive_depth_and_invalid_roots() {
        let root_path = make_test_directory("runtime-inventory-depth");
        fs::create_dir_all(root_path.join("one").join("two")).unwrap();
        fs::write(root_path.join("one").join("two").join("tool.exe"), b"tool").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();

        assert!(
            collect_runtime_tree_inventory(&root, 1, 8).is_err(),
            "nested directories beyond the configured depth must fail closed"
        );
        assert!(collect_runtime_tree_inventory(&root, 0, 8).is_err());
        assert!(collect_runtime_tree_inventory(&root, 8, 0).is_err());

        let file_path = root_path.join("not-a-directory");
        fs::write(&file_path, b"file").unwrap();
        let file = open_file_nofollow(&root, OsStr::new("not-a-directory")).unwrap();
        assert!(collect_runtime_tree_inventory(&file, 8, 8).is_err());

        drop(file);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[test]
    fn verified_project_file_reader_reads_only_the_scanned_file_identity() {
        let root_path = make_test_directory("project-read-identity");
        fs::create_dir(root_path.join("src")).unwrap();
        fs::write(root_path.join("src").join("main.rs"), b"fn main() {}\\n").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let entries = enumerate_runtime_tree_entries(&root, 16).unwrap();
        let src = entries.iter().find(|entry| entry.name == OsStr::new("src")).unwrap();
        let child = open_child_directory_nofollow(&root, &src.name).unwrap();
        let files = enumerate_runtime_tree_entries(&child, 16).unwrap();
        let selected = files.iter().find(|entry| entry.name == OsStr::new("main.rs")).unwrap();
        let bytes = read_verified_project_file(&root, Path::new("src/main.rs"), selected.identity, 128 * 1024).unwrap();
        assert_eq!(bytes, b"fn main() {}\\n");
        drop(child);
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_project_file_reader_rejects_replacement_after_enumeration() {
        let root_path = make_test_directory("project-read-replacement");
        let displaced = root_path.with_extension("displaced");
        fs::write(root_path.join("main.rs"), b"original source").unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();
        let entries = enumerate_runtime_tree_entries(&root, 16).unwrap();
        let selected = entries.iter().find(|entry| entry.name == OsStr::new("main.rs")).unwrap();
        fs::rename(root_path.join("main.rs"), &displaced).unwrap();
        fs::write(root_path.join("main.rs"), b"replacement source").unwrap();
        assert!(read_verified_project_file(&root, Path::new("main.rs"), selected.identity, 128 * 1024).is_err());
        assert_eq!(fs::read(&displaced).unwrap(), b"original source");
        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_file(displaced).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_tree_inventory_rejects_symlinked_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root_path = make_test_directory("runtime-inventory-links");
        let outside = make_test_directory("runtime-inventory-outside");
        fs::write(outside.join("tool.exe"), b"outside").unwrap();
        symlink(outside.join("tool.exe"), root_path.join("linked-file")).unwrap();
        let root = open_verified_root_directory(&root_path).unwrap();

        assert!(
            collect_runtime_tree_inventory(&root, 4, 8).is_err(),
            "runtime inventories must not accept a symlinked file"
        );
        fs::remove_file(root_path.join("linked-file")).unwrap();
        symlink(&outside, root_path.join("linked-directory")).unwrap();
        assert!(
            collect_runtime_tree_inventory(&root, 4, 8).is_err(),
            "runtime inventories must not accept a symlinked directory"
        );

        drop(root);
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

}
