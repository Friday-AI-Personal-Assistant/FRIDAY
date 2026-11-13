//! Experimental handle-relative traversal for Coding Assistant snapshots.
//!
//! This module is intentionally not wired into snapshot creation yet. It only
//! enumerates candidate regular-file paths; it does not open or copy file data.
//! The caller must supply an already-open root directory handle.

use cap_primitives::fs::{open_dir_nofollow, read_dir};
use std::{
    ffi::OsStr,
    fs::File,
    io,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

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
