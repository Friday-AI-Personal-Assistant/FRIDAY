//! Experimental handle-relative traversal for Coding Assistant snapshots.
//!
//! This module is intentionally not wired into snapshot creation yet. It only
//! enumerates candidate regular-file paths; it does not open or copy file data.
//! The caller must supply an already-open root directory handle.

use cap_primitives::fs::{open_dir_nofollow, read_dir};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

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
) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_directory(root, Path::new(""), 0, max_depth, &mut files, &mut |_| Ok(()))?;
    Ok(files)
}

/// Open one regular file relative to an already-open directory handle on Windows.
///
/// This is deliberately test-only. NtCreateFile's RootDirectory pins the
/// parent handle; OBJ_DONT_REPARSE and FILE_OPEN_REPARSE_POINT prevent
/// reparse traversal. If the native call is unavailable or rejects the
/// request, the caller must fail closed rather than fall back to a path open.
#[cfg(windows)]
fn open_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
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
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &object_attributes,
            &mut io_status,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };

    // NT_SUCCESS(status) is status >= 0. Do not retry with weaker flags.
    if status < 0 || handle.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
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

/// Open one regular file relative to an already-open directory handle on Unix.
///
/// O_NOFOLLOW rejects a final symlink and openat pins resolution to the held
/// parent directory. O_NONBLOCK prevents a raced-in FIFO from blocking before
/// the opened handle can be checked as a regular file.
#[cfg(unix)]
fn open_file_nofollow(parent: &File, name: &OsStr) -> io::Result<File> {
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
///
/// This remains a test-only prototype. The caller must still ensure the
/// selected root path has been canonicalized and authorized by the UI/backend.
#[allow(dead_code)]
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

fn is_single_normal_component(name: &OsStr) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn walk_directory<F>(
    parent: &File,
    relative_directory: &Path,
    depth: usize,
    max_depth: usize,
    files: &mut Vec<PathBuf>,
    before_directory_open: &mut F,
) -> io::Result<()>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    for entry in read_dir(parent, Path::new("."))? {
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
    if limits.max_files == 0 || limits.max_file_bytes == 0 || limits.max_total_bytes == 0 {
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
    collect_bounded_directory(
        root,
        Path::new(""),
        0,
        limits,
        include_file,
        exclude_directory,
        &mut before_file_open,
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

    for entry in read_dir(parent, Path::new("."))? {
        if result.files.len() >= limits.max_files
            || result.bytes >= limits.max_total_bytes
        {
            result.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        let name = entry.file_name();
        if !is_single_normal_component(&name) {
            result.truncated = true;
            continue;
        }
        let Some(name_text) = name.to_str() else {
            result.truncated = true;
            continue;
        };
        let entry_type = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if entry_type.is_symlink() {
            continue;
        }

        let relative_path = relative_directory.join(&name);
        if entry_type.is_dir() {
            if exclude_directory(name_text) {
                continue;
            }
            if depth >= limits.max_depth {
                result.truncated = true;
                continue;
            }
            // Never recurse through an ambient path. If an enumerated directory
            // was replaced by a link/reparse point, this no-follow open fails.
            let child = open_dir_nofollow(parent, Path::new(&name))?;
            let child_metadata = child.metadata()?;
            if !child_metadata.is_dir() || is_reparse_or_symlink(&child_metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "opened snapshot child is not an ordinary directory",
                ));
            }
            collect_bounded_directory(
                &child,
                &relative_path,
                depth + 1,
                limits,
                include_file,
                exclude_directory,
                before_file_open,
                result,
            )?;
            if result.truncated
                && (result.files.len() >= limits.max_files
                    || result.bytes >= limits.max_total_bytes)
            {
                break;
            }
            continue;
        }

        if !entry_type.is_file() || !include_file(name_text) {
            continue;
        }

        before_file_open(&relative_path)?;
        // The filename is a single component and the parent is the held handle.
        // No source path assembled from the displayed relative path is opened.
        let mut file = match open_file_nofollow(parent, &name) {
            Ok(file) => file,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        let before = file.metadata()?;
        if !before.is_file() || is_reparse_or_symlink(&before)
            || before.len() == 0 || before.len() > limits.max_file_bytes
        {
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
            || after.len() != before.len()
            || after.modified()? != before.modified()?
            || contents.len() as u64 != before.len()
        {
            result.truncated = true;
            continue;
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
fn create_destination_entry(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
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
        return open_dir_nofollow(parent, &relative_name);
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
fn create_destination_entry(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
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
    const FILE_WRITE_DATA: u32 = 0x2;
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_ADD_FILE: u32 = 0x2;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
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
        FILE_LIST_DIRECTORY | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE
    } else {
        FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE
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
    if snapshot_id.len() != 32 || !snapshot_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid snapshot identifier"));
    }
    if limits.max_files == 0 || limits.max_file_bytes == 0 || limits.max_total_bytes == 0
        || contents.truncated || contents.files.is_empty() || contents.files.len() > limits.max_files
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot contents are incomplete or outside policy limits"));
    }
    let mut total = 0u64;
    for item in &contents.files {
        let depth = item.relative_path.components().count().saturating_sub(1);
        if item.relative_path.as_os_str().is_empty()
            || item.relative_path.components().any(|c| !matches!(c, Component::Normal(_)))
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

    let snapshot_root = create_destination_entry(workspace, OsStr::new(snapshot_id), true)?;
    for item in &contents.files {
        let components: Vec<&OsStr> = item.relative_path.components().filter_map(|c| {
            if let Component::Normal(name) = c { Some(name) } else { None }
        }).collect();
        let mut parent = snapshot_root.try_clone()?;
        for component in &components[..components.len() - 1] {
            let next = match open_dir_nofollow(&parent, Path::new(component)) {
                Ok(dir) => dir,
                Err(_) => create_destination_entry(&parent, component, true)?,
            };
            parent = next;
        }
        let file_name = components[components.len() - 1];
        let mut output = create_destination_entry(&parent, file_name, false)?;
        output.write_all(&item.contents)?;
        output.sync_all()?;
    }

    let marker = format!(
        "{{\"schemaVersion\":1,\"snapshotId\":\"{}\",\"owner\":\"FRIDAY-CodingAssistant\"}}",
        snapshot_id
    );
    let mut marker_file = create_destination_entry(
        &snapshot_root, OsStr::new(".friday-snapshot-owner.json"), false,
    )?;
    marker_file.write_all(marker.as_bytes())?;
    marker_file.sync_all()?;
    Ok(())
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
            max_file_bytes: 1024 * 1024,
            max_total_bytes: 32 * 1024 * 1024,
        }
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
            &|_| false,
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
        let mut files = collect_regular_file_paths(&root, 12).unwrap();
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
        let files = collect_regular_file_paths(&root, 12).unwrap();
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
        let files = collect_regular_file_paths(&root, 12).unwrap();
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
            walk_directory(
                &root,
                Path::new(""),
                0,
                12,
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
        let result = collect_regular_file_paths(&root, 12);
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
            walk_directory(
                &root,
                Path::new(""),
                0,
                12,
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
}
