use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Mutex,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use rand::RngCore;
use tauri::{AppHandle, Manager};

const MAX_SNAPSHOT_DEPTH: usize = 12;
const MAX_SNAPSHOT_FILES: usize = 2_000;
const MAX_SNAPSHOT_FILE_BYTES: u64 = 1_048_576;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const SNAPSHOT_OWNER_MARKER: &str = ".friday-snapshot-owner.json";
const SNAPSHOT_OWNER_SCHEMA: u32 = 1;

const MAX_GUEST_RESULT_BYTES: usize = 1024 * 1024;
const MAX_SANDBOX_CONFIG_BYTES: usize = 16 * 1024;
const SANDBOX_CONFIG_FILE_NAME: &str = "sandbox.wsb";
const MAX_GUEST_RESULT_DURATION_MS: u64 = 120_000;

const MAX_GUEST_REQUEST_DURATION_MS: u64 = 120_000;
const MAX_GUEST_REQUEST_OUTPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestRunRequest {
    schema_version: u32,
    run_id: String,
    snapshot_id: String,
    operation: GuestRunOperation,
    max_duration_ms: u64,
    max_output_bytes: u64,
}

fn validate_guest_run_request(request: GuestRunRequest) -> Result<GuestRunRequest, String> {
    if request.schema_version != 1
        || !valid_snapshot_id(&request.run_id)
        || !valid_snapshot_id(&request.snapshot_id)
        || request.max_duration_ms == 0
        || request.max_duration_ms > MAX_GUEST_REQUEST_DURATION_MS
        || request.max_output_bytes == 0
        || request.max_output_bytes > MAX_GUEST_REQUEST_OUTPUT_BYTES
    {
        return Err("The guest run request is invalid or exceeds FRIDAY's fixed policy limits.".into());
    }
    Ok(request)
}

fn parse_guest_run_request(bytes: &[u8]) -> Result<GuestRunRequest, String> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("The guest run request is empty or exceeds its size limit.".into());
    }
    let request: GuestRunRequest = serde_json::from_slice(bytes)
        .map_err(|_| "The guest run request is malformed or contains unsupported fields.".to_string())?;
    validate_guest_run_request(request)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum GuestRunOperation {
    RustCargoTest,
    NodeNpmTest,
    PythonUnittest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GuestOperationCommand {
    program: &'static str,
    arguments: &'static [&'static str],
}

/// Maps a typed operation to a fixed command contract. The guest request never
/// carries an executable path, argument vector, shell fragment, or environment.
fn guest_operation_command(operation: GuestRunOperation) -> GuestOperationCommand {
    match operation {
        GuestRunOperation::RustCargoTest => GuestOperationCommand {
            program: "cargo",
            arguments: &["test", "--locked", "--offline"],
        },
        GuestRunOperation::NodeNpmTest => GuestOperationCommand {
            program: "npm",
            arguments: &["test"],
        },
        GuestRunOperation::PythonUnittest => GuestOperationCommand {
            program: "python",
            arguments: &["-m", "unittest", "discover", "-v"],
        },
    }
}


/// Fixed guest-side toolchain layout for a future runner. These paths are
/// image-owned constants, never request or project inputs. The profiles are
/// descriptive launch plans only; they do not prove the toolchains are
/// provisioned or execute any process.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimeLaunchPlan {
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

const GUEST_WORKSPACE_ROOT: &str = r"C:\FRIDAY\Workspace";

fn build_guest_runtime_launch_plan(
    operation: GuestRunOperation,
) -> GuestRuntimeLaunchPlan {
    let working_directory = PathBuf::from(GUEST_WORKSPACE_ROOT);
    match operation {
        GuestRunOperation::RustCargoTest => GuestRuntimeLaunchPlan {
            program: PathBuf::from(r"C:\FRIDAY\Toolchains\Rust\bin\cargo.exe"),
            arguments: vec!["test".into(), "--locked".into(), "--offline".into()],
            working_directory,
        },
        GuestRunOperation::NodeNpmTest => GuestRuntimeLaunchPlan {
            // Invoke node.exe directly; never resolve or execute npm.cmd.
            program: PathBuf::from(r"C:\FRIDAY\Toolchains\Node\node.exe"),
            arguments: vec![
                r"C:\FRIDAY\Toolchains\Node\node_modules\npm\bin\npm-cli.js".into(),
                "test".into(),
            ],
            working_directory,
        },
        GuestRunOperation::PythonUnittest => GuestRuntimeLaunchPlan {
            program: PathBuf::from(r"C:\FRIDAY\Toolchains\Python\python.exe"),
            arguments: vec!["-m".into(), "unittest".into(), "discover".into(), "-v".into()],
            working_directory,
        },
    }
}

/// Revalidates a runtime launch plan against the single trusted profile for
/// its typed operation. This rejects substituted executables, arguments, or
/// working directories before a future process-creation boundary.
#[allow(dead_code)]
fn validate_guest_runtime_launch_plan(
    operation: GuestRunOperation,
    plan: &GuestRuntimeLaunchPlan,
) -> Result<(), String> {
    if plan != &build_guest_runtime_launch_plan(operation) {
        return Err("The guest runtime launch plan differs from FRIDAY's pinned operation profile.".into());
    }
    Ok(())
}

/// Explicit guest process environment. This is built from fixed image-owned
/// values only; it must never be merged with the host or project environment.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimeEnvironmentPlan {
    variables: BTreeMap<String, String>,
}

const GUEST_WINDOWS_ROOT: &str = r"C:\Windows";
const GUEST_TEMP_ROOT: &str = r"C:\FRIDAY\Temp";
const GUEST_RUST_HOME: &str = r"C:\FRIDAY\Toolchains\Rust\CargoHome";
const GUEST_RUSTUP_HOME: &str = r"C:\FRIDAY\Toolchains\Rust\RustupHome";
const GUEST_NODE_CACHE: &str = r"C:\FRIDAY\Toolchains\Node\NpmCache";
const GUEST_PYTHON_HOME: &str = r"C:\FRIDAY\Toolchains\Python";

fn build_guest_runtime_environment_plan(
    operation: GuestRunOperation,
) -> GuestRuntimeEnvironmentPlan {
    let mut variables = BTreeMap::from([
        ("SystemRoot".into(), GUEST_WINDOWS_ROOT.into()),
        ("WINDIR".into(), GUEST_WINDOWS_ROOT.into()),
        ("TEMP".into(), GUEST_TEMP_ROOT.into()),
        ("TMP".into(), GUEST_TEMP_ROOT.into()),
        (
            "PATH".into(),
            format!(
                r"C:\FRIDAY\Toolchains\Rust\bin;C:\FRIDAY\Toolchains\Node;C:\FRIDAY\Toolchains\Python;{}\System32",
                GUEST_WINDOWS_ROOT
            ),
        ),
    ]);
    match operation {
        GuestRunOperation::RustCargoTest => {
            variables.insert("CARGO_HOME".into(), GUEST_RUST_HOME.into());
            variables.insert("RUSTUP_HOME".into(), GUEST_RUSTUP_HOME.into());
        }
        GuestRunOperation::NodeNpmTest => {
            variables.insert("npm_config_cache".into(), GUEST_NODE_CACHE.into());
        }
        GuestRunOperation::PythonUnittest => {
            variables.insert("PYTHONHOME".into(), GUEST_PYTHON_HOME.into());
            variables.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        }
    }
    GuestRuntimeEnvironmentPlan { variables }
}

/// Accepts only the complete fixed environment for this operation. A caller
/// cannot add variables, alter paths, or smuggle inherited host values in.
#[allow(dead_code)]
fn validate_guest_runtime_environment_plan(
    operation: GuestRunOperation,
    plan: &GuestRuntimeEnvironmentPlan,
) -> Result<(), String> {
    if plan != &build_guest_runtime_environment_plan(operation) {
        return Err("The guest runtime environment differs from FRIDAY's fixed per-operation policy.".into());
    }
    if plan.variables.keys().any(|key| {
        key.is_empty() || !key.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
    }) || plan.variables.values().any(|value| value.contains('\0') || value.contains('\n') || value.contains('\r')) {
        return Err("The guest runtime environment contains an invalid variable name or value.".into());
    }
    Ok(())
}

/// Serializes the exact trusted environment into a Windows UTF-16 environment
/// block. Entries are sorted case-insensitively as required by CreateProcessW;
/// the block is double-NUL terminated and never inherits host variables.
#[allow(dead_code)]
fn serialize_guest_runtime_environment_block(
    operation: GuestRunOperation,
    plan: &GuestRuntimeEnvironmentPlan,
) -> Result<Vec<u16>, String> {
    validate_guest_runtime_environment_plan(operation, plan)?;
    let mut entries = plan.variables.iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| {
        left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())
    });
    for pair in entries.windows(2) {
        if pair[0].0.eq_ignore_ascii_case(pair[1].0) {
            return Err("The guest environment contains duplicate case-insensitive variable names.".into());
        }
    }

    let mut block = Vec::new();
    for (key, value) in entries {
        let entry = format!("{key}={value}");
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    block.push(0);
    if block.len() < 2 || block.len() > 32_767 {
        return Err("The serialized guest environment block is empty or exceeds Windows limits.".into());
    }
    Ok(block)
}

const MAX_GUEST_RUNTIME_MANIFEST_ENTRIES: usize = 256;
const MAX_GUEST_RUNTIME_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GUEST_RUNTIME_MANIFEST_BYTES: u64 = 512 * 1024 * 1024;
const MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES: usize = 255;
const MAX_GUEST_RUNTIME_ARTIFACT_PATH_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct GuestRuntimeArtifactSpec {
    relative_path: String,
    sha256: String,
    max_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct GuestRuntimeArtifactManifest {
    schema_version: u32,
    operation: GuestRunOperation,
    artifacts: Vec<GuestRuntimeArtifactSpec>,
}

/// This trust anchor must be constructed only from immutable FRIDAY build
/// metadata. In production its digest is compiled into the signed application;
/// it must never be loaded from the guest, project, model, or mutable settings.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimeManifestTrustAnchor {
    schema_version: u32,
    operation: GuestRunOperation,
    bundle_id: String,
    toolchain_version: String,
    platform: String,
    architecture: String,
    manifest_sha256: String,
}

/// Authenticate a complete runtime attestation against its separately pinned build-time digest.
/// Sorting entries before serialization makes the digest independent of the
/// manifest's input ordering while retaining every path, limit, and file hash.
/// This is a trust-boundary primitive only: no production trust anchors or
/// runtime bundles are provisioned by this function.
#[allow(dead_code)]
fn authenticate_guest_runtime_manifest(
    operation: GuestRunOperation,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
) -> Result<(), String> {
    validate_guest_runtime_artifact_manifest(operation, manifest)?;
    if anchor.schema_version != 1
        || anchor.operation != operation
        || anchor.bundle_id.is_empty()
        || anchor.bundle_id.len() > 96
        || !anchor.bundle_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || anchor.toolchain_version.is_empty()
        || anchor.toolchain_version.len() > 64
        || !anchor.toolchain_version.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        || anchor.platform != "windows"
        || anchor.architecture != "x86_64"
        || anchor.manifest_sha256.len() != 64
        || !anchor.manifest_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("The guest runtime trust anchor is invalid or does not match this platform and operation.".into());
    }

    let actual = canonical_guest_runtime_bundle_sha256(manifest, anchor)?;
    if !actual.eq_ignore_ascii_case(&anchor.manifest_sha256) {
        return Err("The guest runtime bundle metadata and manifest do not match FRIDAY's pinned build-time digest.".into());
    }
    Ok(())
}

/// Canonical SHA-256 for the complete runtime attestation. Artifact ordering is
/// normalized case-insensitively; bundle identity, toolchain version, platform,
/// architecture, operation, schema, and every artifact record are all covered.
fn canonical_guest_runtime_bundle_sha256(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
) -> Result<String, String> {
    let mut canonical = manifest.clone();
    canonical.artifacts.sort_by(|left, right| {
        left.relative_path.to_ascii_lowercase().cmp(&right.relative_path.to_ascii_lowercase())
    });
    let attestation = (
        anchor.schema_version,
        anchor.operation,
        &anchor.bundle_id,
        &anchor.toolchain_version,
        &anchor.platform,
        &anchor.architecture,
        canonical,
    );
    let encoded = serde_json::to_vec(&attestation)
        .map_err(|_| "The trusted guest runtime attestation could not be encoded.".to_string())?;
    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(&encoded).iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A deterministic, immutable description of where one authenticated runtime
/// bundle may be staged and eventually published. This is planning only:
/// it does not download, write, replace, or execute runtime files.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimeProvisioningPlan {
    operation: GuestRunOperation,
    bundle_id: String,
    toolchain_version: String,
    platform: String,
    architecture: String,
    manifest_sha256: String,
    artifact_count: usize,
    declared_max_bytes: u64,
    staging_root: PathBuf,
    final_root: PathBuf,
}

fn build_guest_runtime_provisioning_plan(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
) -> Result<GuestRuntimeProvisioningPlan, String> {
    build_guest_runtime_provisioning_plan_with_final_root(
        manifest, anchor, PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn build_guest_runtime_provisioning_plan_with_final_root(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    final_root: PathBuf,
) -> Result<GuestRuntimeProvisioningPlan, String> {
    authenticate_guest_runtime_manifest(anchor.operation, manifest, anchor)?;
    let mut declared_max_bytes = 0u64;
    for artifact in &manifest.artifacts {
        declared_max_bytes = declared_max_bytes.checked_add(artifact.max_bytes)
            .ok_or("The runtime bundle's declared size overflowed.")?;
    }
    if declared_max_bytes == 0 || declared_max_bytes > MAX_GUEST_RUNTIME_MANIFEST_BYTES {
        return Err("The runtime bundle exceeds FRIDAY's total provisioning size limit.".into());
    }
    let parent = final_root.parent()
        .ok_or("The fixed runtime installation root has no parent directory.")?;
    let staging_root = parent.join(format!(".friday-{}.staging", anchor.bundle_id));
    if staging_root == final_root || staging_root.file_name().is_none() {
        return Err("The runtime staging location is invalid.".into());
    }
    Ok(GuestRuntimeProvisioningPlan {
        operation: anchor.operation,
        bundle_id: anchor.bundle_id.clone(),
        toolchain_version: anchor.toolchain_version.clone(),
        platform: anchor.platform.clone(),
        architecture: anchor.architecture.clone(),
        manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
        artifact_count: manifest.artifacts.len(),
        declared_max_bytes,
        staging_root,
        final_root,
    })
}

/// Rebuild the provisioning plan from authenticated inputs and require exact
/// equality. Callers cannot redirect staging/publication or loosen size bounds.
#[allow(dead_code)]
fn validate_guest_runtime_provisioning_plan(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    plan: &GuestRuntimeProvisioningPlan,
) -> Result<(), String> {
    validate_guest_runtime_provisioning_plan_with_final_root(
        manifest, anchor, plan, PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn validate_guest_runtime_provisioning_plan_with_final_root(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    plan: &GuestRuntimeProvisioningPlan,
    final_root: PathBuf,
) -> Result<(), String> {
    if plan != &build_guest_runtime_provisioning_plan_with_final_root(manifest, anchor, final_root)? {
        return Err("The runtime provisioning plan differs from FRIDAY's authenticated bundle policy.".into());
    }
    Ok(())
}


/// Convert a caller-supplied source path into the single portable identity
/// used by the Windows runtime manifest. Reject lossy, non-ASCII, ambiguous,
/// traversal, reserved-device, and overlong paths before retaining any bytes.
fn normalize_guest_runtime_source_path(path: &Path) -> Result<String, String> {
    let raw = path.to_str()
        .ok_or("A runtime source path is not valid UTF-8.")?;
    let portable = raw.replace('/', "\\");
    if portable.is_empty()
        || portable.len() > MAX_GUEST_RUNTIME_ARTIFACT_PATH_BYTES
        || portable.starts_with('\\')
        || portable.ends_with('\\')
    {
        return Err("A runtime source path is empty, rooted, or exceeds FRIDAY's path limit.".into());
    }
    let components = portable.split('\\').collect::<Vec<_>>();
    if components.is_empty() || components.len() > 32 || components.iter().any(|part| {
        part.is_empty()
            || part.len() > MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES
            || *part == "."
            || *part == ".."
            || is_reserved_windows_runtime_component(part)
            || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        return Err("A runtime source path contains an unsafe or non-portable component.".into());
    }
    Ok(components.join("\\").to_ascii_lowercase())
}

/// Materialize a complete runtime bundle only from bytes whose exact paths and
/// SHA-256 digests are authenticated by FRIDAY's immutable manifest anchor.
/// The staging marker is written last by the filesystem helper; the returned
/// proof is issued only after reopening and verifying the complete staged tree.
/// This function does not publish or execute the runtime.
#[allow(dead_code)]
fn provision_authenticated_guest_runtime_bundle_from_bytes(
    parent_path: &Path,
    parent_handle: &File,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    plan: &GuestRuntimeProvisioningPlan,
    source_files: &[(PathBuf, Vec<u8>)],
) -> Result<VerifiedGuestRuntimeStaging, String> {
    validate_guest_runtime_provisioning_plan_with_final_root(manifest, anchor, plan, plan.final_root.clone())?;
    if plan.staging_root.parent() != Some(parent_path)
        || source_files.len() != manifest.artifacts.len()
    {
        return Err("The runtime source set or staging parent differs from the authenticated provisioning plan.".into());
    }
    let parent_identity = crate::coding_sandbox_fs::verified_directory_identity(parent_path, parent_handle)
        .map_err(|_| "The runtime staging parent identity could not be verified.".to_string())?;

    // Keep references to caller-owned buffers until every path, digest, and
    // aggregate bound has been checked. This avoids cloning untrusted payloads
    // before the exact manifest closure is established.
    let mut supplied = std::collections::BTreeMap::<String, &[u8]>::new();
    let mut actual_total = 0u64;
    for (path, bytes) in source_files {
        let key = normalize_guest_runtime_source_path(path)?;
        if supplied.insert(key, bytes.as_slice()).is_some() {
            return Err("The runtime source set contains duplicate case-insensitive artifact paths.".into());
        }
        actual_total = actual_total.checked_add(bytes.len() as u64)
            .ok_or("The runtime source byte count overflowed.")?;
        if actual_total > plan.declared_max_bytes {
            return Err("The runtime source set exceeds its authenticated aggregate size bound.".into());
        }
    }
    if supplied.len() != manifest.artifacts.len() {
        return Err("The runtime source set is missing or contains unlisted artifacts.".into());
    }

    let mut materialized = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let key = artifact.relative_path.to_ascii_lowercase();
        let bytes = supplied.get(&key)
            .ok_or("A required authenticated runtime artifact is missing from the source set.")?;
        verify_guest_runtime_artifact_bytes(artifact, bytes)?;
        materialized.push((
            PathBuf::from(artifact.relative_path.replace('\\', "/")),
            bytes.to_vec(),
        ));
    }

    let marker = build_guest_runtime_staging_owner_marker(manifest, anchor)?;
    let staging_name = plan.staging_root.file_name()
        .ok_or("The authenticated runtime staging path has no final component.")?;
    crate::coding_sandbox_fs::create_bounded_runtime_destination(
        parent_handle,
        staging_name,
        &materialized,
        &marker,
        MAX_GUEST_RUNTIME_MANIFEST_ENTRIES,
        MAX_GUEST_RUNTIME_ARTIFACT_BYTES,
        MAX_GUEST_RUNTIME_MANIFEST_BYTES,
        32,
    ).map_err(|_| "FRIDAY could not safely create the authenticated runtime staging directory.".to_string())?;

    let staging_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(parent_handle, staging_name)
        .map_err(|_| "The newly staged runtime directory could not be safely reopened.".to_string())?;
    let proof = verify_staged_guest_runtime_bundle_with_final_root(
        &plan.staging_root, &staging_handle, manifest, anchor, plan, plan.final_root.clone(),
    )?;
    let final_parent_identity = crate::coding_sandbox_fs::verified_directory_identity(parent_path, parent_handle)
        .map_err(|_| "The runtime staging parent identity changed during provisioning.".to_string())?;
    if final_parent_identity != parent_identity {
        return Err("The runtime staging parent identity changed during provisioning.".into());
    }
    Ok(proof)
}

/// Ownership metadata is deterministic and bound to the authenticated bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestRuntimeStagingOwnerMarker {
    schema_version: u32,
    operation: GuestRunOperation,
    bundle_id: String,
    manifest_sha256: String,
    artifact_count: usize,
}

fn build_guest_runtime_staging_owner_marker(
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
) -> Result<Vec<u8>, String> {
    authenticate_guest_runtime_manifest(anchor.operation, manifest, anchor)?;
    let marker = GuestRuntimeStagingOwnerMarker {
        schema_version: 1,
        operation: anchor.operation,
        bundle_id: anchor.bundle_id.clone(),
        manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
        artifact_count: manifest.artifacts.len(),
    };
    serde_json::to_vec(&marker)
        .map_err(|_| "The runtime staging ownership marker could not be encoded.".to_string())
}

/// A staging verification result is deliberately distinct from the final
/// executable-bundle proof. It cannot authorize a runtime launch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedGuestRuntimeStaging {
    operation: GuestRunOperation,
    staging_root: PathBuf,
    root_identity: (u64, u64),
    bundle_id: String,
    manifest_sha256: String,
    artifact_count: usize,
}

#[allow(dead_code)]
fn verify_staged_guest_runtime_bundle(
    staging_root_path: &Path,
    staging_root: &File,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    plan: &GuestRuntimeProvisioningPlan,
) -> Result<VerifiedGuestRuntimeStaging, String> {
    verify_staged_guest_runtime_bundle_with_final_root(
        staging_root_path, staging_root, manifest, anchor, plan,
        PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn verify_staged_guest_runtime_bundle_with_final_root(
    staging_root_path: &Path,
    staging_root: &File,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    plan: &GuestRuntimeProvisioningPlan,
    final_root: PathBuf,
) -> Result<VerifiedGuestRuntimeStaging, String> {
    validate_guest_runtime_provisioning_plan_with_final_root(manifest, anchor, plan, final_root)?;
    if staging_root_path != plan.staging_root {
        return Err("The staged runtime directory differs from the authenticated staging plan.".into());
    }
    let root_identity = crate::coding_sandbox_fs::verified_directory_identity(staging_root_path, staging_root)
        .map_err(|_| "The runtime staging directory identity could not be checked.".to_string())?;

    let expected_marker = build_guest_runtime_staging_owner_marker(manifest, anchor)?;
    let actual_marker = crate::coding_sandbox_fs::read_bounded_relative_file(
        staging_root, ".friday-runtime-owner.json", 16 * 1024,
    ).map_err(|_| "The runtime staging ownership marker is missing or unsafe.".to_string())?;
    if actual_marker != expected_marker {
        return Err("The runtime staging ownership marker does not match the authenticated bundle.".into());
    }

    validate_guest_runtime_artifact_manifest(anchor.operation, manifest)?;
    let (mut actual_files, mut actual_directories) =
        crate::coding_sandbox_fs::collect_runtime_tree_inventory(staging_root, 32, 1281)
            .map_err(|_| "The staged runtime contains unsafe, unexpected, or excessive entries.".to_string())?;
    let marker_name = ".friday-runtime-owner.json";
    let marker_positions = actual_files.iter().enumerate()
        .filter(|(_, path)| path.eq_ignore_ascii_case(marker_name))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if marker_positions.len() != 1 {
        return Err("The staged runtime must contain exactly one ownership marker.".into());
    }
    actual_files.remove(marker_positions[0]);

    // The inventory deliberately emits Windows-style relative separators
    // on every host. Keep the authenticated manifest in that same canonical
    // representation when comparing exact file and directory closure.
    let mut expected_files = manifest.artifacts.iter()
        .map(|artifact| artifact.relative_path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut expected_directories = std::collections::BTreeSet::new();
    for artifact in &manifest.artifacts {
        let components = artifact.relative_path.split('\\').collect::<Vec<_>>();
        for depth in 1..components.len() {
            expected_directories.insert(components[..depth].join("\\").to_ascii_lowercase());
        }
    }
    actual_files.iter_mut().for_each(|path| *path = path.to_ascii_lowercase());
    actual_directories.iter_mut().for_each(|path| *path = path.to_ascii_lowercase());
    expected_files.sort();
    actual_files.sort();
    let mut expected_directories = expected_directories.into_iter().collect::<Vec<_>>();
    expected_directories.sort();
    actual_directories.sort();
    if actual_files != expected_files || actual_directories != expected_directories {
        return Err("The staged runtime does not exactly match the authenticated artifact inventory.".into());
    }
    for artifact in &manifest.artifacts {
        verify_guest_runtime_artifact_from_root(staging_root, artifact)?;
    }
    let final_identity = crate::coding_sandbox_fs::verified_directory_identity(staging_root_path, staging_root)
        .map_err(|_| "The runtime staging directory identity could not be rechecked.".to_string())?;
    if final_identity != root_identity {
        return Err("The runtime staging directory identity changed during verification.".into());
    }

    Ok(VerifiedGuestRuntimeStaging {
        operation: anchor.operation,
        staging_root: staging_root_path.to_path_buf(),
        root_identity,
        bundle_id: anchor.bundle_id.clone(),
        manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
        artifact_count: manifest.artifacts.len(),
    })
}

/// A non-executing, no-overwrite publication intent derived only from a
/// verified staging proof and the authenticated provisioning plan. This is
/// not a filesystem rename and does not make the staged bundle executable.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimePublicationPlan {
    operation: GuestRunOperation,
    bundle_id: String,
    manifest_sha256: String,
    artifact_count: usize,
    staging_root: PathBuf,
    staging_root_identity: (u64, u64),
    final_root: PathBuf,
}

#[allow(dead_code)]
fn build_guest_runtime_publication_plan(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
) -> Result<GuestRuntimePublicationPlan, String> {
    build_guest_runtime_publication_plan_with_final_root(
        staging, manifest, anchor, provisioning,
        PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn build_guest_runtime_publication_plan_with_final_root(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    final_root: PathBuf,
) -> Result<GuestRuntimePublicationPlan, String> {
    validate_guest_runtime_provisioning_plan_with_final_root(
        manifest, anchor, provisioning, final_root.clone(),
    )?;
    authenticate_guest_runtime_manifest(anchor.operation, manifest, anchor)?;
    if staging.operation != anchor.operation
        || staging.staging_root != provisioning.staging_root
        || staging.bundle_id != anchor.bundle_id
        || staging.manifest_sha256 != anchor.manifest_sha256.to_ascii_lowercase()
        || staging.artifact_count != manifest.artifacts.len()
        || staging.root_identity == (0, 0)
    {
        return Err("The verified staging proof does not match the authenticated publication plan.".into());
    }
    if staging.staging_root == final_root
        || staging.staging_root.parent() != final_root.parent()
    {
        return Err("Runtime publication must remain within the planned same-volume parent.".into());
    }
    Ok(GuestRuntimePublicationPlan {
        operation: anchor.operation,
        bundle_id: anchor.bundle_id.clone(),
        manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
        artifact_count: manifest.artifacts.len(),
        staging_root: staging.staging_root.clone(),
        staging_root_identity: staging.root_identity,
        final_root,
    })
}

/// Rebuild the exact intent from the authenticated inputs and private staging
/// proof. A modified destination, identity, or bundle binding is rejected.
#[allow(dead_code)]
fn validate_guest_runtime_publication_plan(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    publication: &GuestRuntimePublicationPlan,
) -> Result<(), String> {
    let expected = build_guest_runtime_publication_plan(staging, manifest, anchor, provisioning)?;
    if publication != &expected {
        return Err("The runtime publication plan differs from FRIDAY's verified staging authorization.".into());
    }
    Ok(())
}


/// A short-lived preflight result for the future atomic publication boundary.
/// It records the exact held parent/staging identities and an observed-absent
/// destination. It is NOT itself permission to rename: the eventual publisher
/// must recheck these facts and perform an atomic no-replace operation.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRuntimePublicationPreflight {
    operation: GuestRunOperation,
    staging_root: PathBuf,
    staging_root_identity: (u64, u64),
    parent_path: PathBuf,
    parent_identity: (u64, u64),
    final_root: PathBuf,
    bundle_id: String,
    manifest_sha256: String,
    artifact_count: usize,
}

#[allow(dead_code)]
fn authorize_guest_runtime_publication_preflight(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    publication: &GuestRuntimePublicationPlan,
    parent_path: &Path,
    parent_handle: &File,
    staging_handle: &File,
) -> Result<GuestRuntimePublicationPreflight, String> {
    authorize_guest_runtime_publication_preflight_with_final_root(
        staging, manifest, anchor, provisioning, publication,
        parent_path, parent_handle, staging_handle,
        PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn authorize_guest_runtime_publication_preflight_with_final_root(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    publication: &GuestRuntimePublicationPlan,
    parent_path: &Path,
    parent_handle: &File,
    staging_handle: &File,
    final_root: PathBuf,
) -> Result<GuestRuntimePublicationPreflight, String> {
    let expected_publication = build_guest_runtime_publication_plan_with_final_root(
        staging, manifest, anchor, provisioning, final_root.clone(),
    )?;
    if publication != &expected_publication {
        return Err("The runtime publication intent changed before preflight.".into());
    }
    let expected_parent = final_root.parent()
        .ok_or("The runtime publication destination has no parent directory.")?;
    if !parent_path.is_absolute()
        || parent_path != expected_parent
        || staging.staging_root.parent() != Some(parent_path)
    {
        return Err("The runtime publication parent differs from the authenticated sibling layout.".into());
    }

    let parent_identity = crate::coding_sandbox_fs::verified_directory_identity(
        parent_path, parent_handle,
    ).map_err(|_| "The runtime publication parent identity could not be verified.".to_string())?;
    let staging_identity = crate::coding_sandbox_fs::verified_directory_identity(
        &staging.staging_root, staging_handle,
    ).map_err(|_| "The runtime staging directory identity could not be revalidated.".to_string())?;
    if parent_identity.0 != staging_identity.0
        || staging_identity != staging.root_identity
        || staging_identity != publication.staging_root_identity
    {
        return Err("The runtime staging directory or same-volume parent identity changed.".into());
    }

    match fs::symlink_metadata(&final_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err("The runtime publication destination already exists; FRIDAY will not overwrite it.".into()),
        Err(_) => return Err("The runtime publication destination could not be safely inspected.".into()),
    }

    // Recheck both held identities after the destination observation. This
    // narrows the preflight race; the future rename must still be atomic/no-replace.
    let parent_after = crate::coding_sandbox_fs::verified_directory_identity(
        parent_path, parent_handle,
    ).map_err(|_| "The runtime publication parent changed during preflight.".to_string())?;
    let staging_after = crate::coding_sandbox_fs::verified_directory_identity(
        &staging.staging_root, staging_handle,
    ).map_err(|_| "The runtime staging directory changed during preflight.".to_string())?;
    if parent_after != parent_identity || staging_after != staging_identity {
        return Err("A runtime publication directory identity changed during preflight.".into());
    }

    Ok(GuestRuntimePublicationPreflight {
        operation: publication.operation,
        staging_root: publication.staging_root.clone(),
        staging_root_identity: staging_identity,
        parent_path: parent_path.to_path_buf(),
        parent_identity,
        final_root,
        bundle_id: publication.bundle_id.clone(),
        manifest_sha256: publication.manifest_sha256.clone(),
        artifact_count: publication.artifact_count,
    })
}

/// Complete the authenticated staging-to-publication transaction. The staged
/// proof is re-bound to the provisioning plan, the destination is atomically
/// published without replacement, and the final tree is hashed again at its
/// fixed destination before a launch-capable proof is returned. This still
/// does not launch a process or start Windows Sandbox.
#[allow(dead_code)]
fn publish_authenticated_guest_runtime_bundle(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    parent_path: &Path,
    parent_handle: &File,
    staging_handle: File,
) -> Result<VerifiedGuestRuntimeBundle, String> {
    publish_authenticated_guest_runtime_bundle_with_final_root(
        staging, manifest, anchor, provisioning, parent_path, parent_handle,
        staging_handle, PathBuf::from(fixed_guest_runtime_root(anchor.operation)),
    )
}

fn publish_authenticated_guest_runtime_bundle_with_final_root(
    staging: &VerifiedGuestRuntimeStaging,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
    provisioning: &GuestRuntimeProvisioningPlan,
    parent_path: &Path,
    parent_handle: &File,
    staging_handle: File,
    final_root: PathBuf,
) -> Result<VerifiedGuestRuntimeBundle, String> {
    validate_guest_runtime_provisioning_plan_with_final_root(
        manifest, anchor, provisioning, final_root.clone(),
    )?;
    let publication = build_guest_runtime_publication_plan_with_final_root(
        staging, manifest, anchor, provisioning, final_root.clone(),
    )?;
    let preflight = authorize_guest_runtime_publication_preflight_with_final_root(
        staging, manifest, anchor, provisioning, &publication,
        parent_path, parent_handle, &staging_handle, final_root.clone(),
    )?;
    let published = publish_guest_runtime_bundle_atomically(
        &preflight, parent_handle, staging_handle,
    )?;
    if published.operation != anchor.operation
        || published.root_path != final_root
        || published.bundle_id != anchor.bundle_id
        || published.manifest_sha256 != anchor.manifest_sha256.to_ascii_lowercase()
        || published.artifact_count != manifest.artifacts.len()
    {
        return Err("The atomically published runtime evidence differs from its authenticated manifest.".into());
    }

    let destination_name = final_root.file_name()
        .ok_or("The published runtime destination name is missing.")?;
    let final_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(
        parent_handle, destination_name,
    ).map_err(|_| "The published runtime directory could not be reopened without following links.".to_string())?;
    let verified = verify_authenticated_guest_runtime_bundle(
        &final_root, &final_handle, anchor.operation, manifest, anchor,
    )?;
    if verified.root_identity != published.root_identity
        || verified.root_identity != staging.root_identity
        || verified.bundle_id != published.bundle_id
        || verified.manifest_sha256 != published.manifest_sha256
        || verified.artifact_count != published.artifact_count
    {
        return Err("The final runtime tree does not match the atomically published staging identity and manifest.".into());
    }
    Ok(verified)
}

/// Evidence that an authenticated runtime staging directory was atomically
/// published at its fixed destination without replacing an existing entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PublishedGuestRuntimeBundle {
    operation: GuestRunOperation,
    root_path: PathBuf,
    root_identity: (u64, u64),
    bundle_id: String,
    manifest_sha256: String,
    artifact_count: usize,
}

#[allow(dead_code)]
fn publish_guest_runtime_bundle_atomically(
    preflight: &GuestRuntimePublicationPreflight,
    parent_handle: &File,
    staging_handle: File,
) -> Result<PublishedGuestRuntimeBundle, String> {
    let expected_parent = preflight.final_root.parent()
        .ok_or("The runtime publication destination has no parent directory.")?;
    if preflight.parent_path != expected_parent
        || preflight.staging_root.parent() != Some(expected_parent)
        || preflight.staging_root.file_name().is_none()
        || preflight.final_root.file_name().is_none()
        || preflight.bundle_id.is_empty()
        || preflight.manifest_sha256.len() != 64
    {
        return Err("The runtime publication preflight no longer describes an exact sibling destination.".into());
    }

    let parent_before = crate::coding_sandbox_fs::verified_directory_identity(
        &preflight.parent_path, parent_handle,
    ).map_err(|_| "The runtime publication parent handle could not be revalidated.".to_string())?;
    let staging_before = crate::coding_sandbox_fs::verified_directory_identity(
        &preflight.staging_root, &staging_handle,
    ).map_err(|_| "The runtime staging handle could not be revalidated.".to_string())?;
    if parent_before != preflight.parent_identity
        || staging_before != preflight.staging_root_identity
        || parent_before.0 != staging_before.0
    {
        return Err("The runtime publication parent or staging identity changed after preflight.".into());
    }
    match fs::symlink_metadata(&preflight.final_root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err("The runtime publication destination already exists; FRIDAY will not overwrite it.".into()),
        Err(_) => return Err("The runtime publication destination could not be safely inspected.".into()),
    }
    let parent_after = crate::coding_sandbox_fs::verified_directory_identity(
        &preflight.parent_path, parent_handle,
    ).map_err(|_| "The runtime publication parent changed immediately before rename.".to_string())?;
    let staging_after = crate::coding_sandbox_fs::verified_directory_identity(
        &preflight.staging_root, &staging_handle,
    ).map_err(|_| "The runtime staging directory changed immediately before rename.".to_string())?;
    if parent_after != parent_before || staging_after != staging_before {
        return Err("A runtime publication directory identity changed immediately before rename.".into());
    }

    let source_name = preflight.staging_root.file_name()
        .ok_or("The runtime staging directory name is missing.")?;
    let destination_name = preflight.final_root.file_name()
        .ok_or("The runtime destination directory name is missing.")?;
    // Consume the staging handle before rename. On Windows the ordinary
    // no-follow directory handle intentionally denies delete sharing, which
    // prevents rename while it remains open. The filesystem helper reopens
    // the source by name relative to the pinned parent, verifies its identity,
    // then renames that exact opened object by handle.
    drop(staging_handle);
    crate::coding_sandbox_fs::rename_directory_noreplace(
        parent_handle, source_name, destination_name, parent_before, staging_before,
    ).map_err(|error| format!("FRIDAY could not atomically publish the verified runtime without replacing an existing destination: {error}"))?;

    let published_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(
        parent_handle, destination_name,
    ).map_err(|_| "The runtime was moved but its published directory could not be safely reopened.".to_string())?;
    let published_identity = crate::coding_sandbox_fs::verified_directory_identity(
        &preflight.final_root, &published_handle,
    ).map_err(|_| "The published runtime path no longer identifies its verified directory handle.".to_string())?;
    if published_identity != preflight.staging_root_identity {
        return Err("The published runtime directory identity differs from the verified staging identity.".into());
    }

    Ok(PublishedGuestRuntimeBundle {
        operation: preflight.operation,
        root_path: preflight.final_root.clone(),
        root_identity: published_identity,
        bundle_id: preflight.bundle_id.clone(),
        manifest_sha256: preflight.manifest_sha256.clone(),
        artifact_count: preflight.artifact_count,
    })
}

/// Evidence that a specific build-pinned runtime manifest authenticated and
/// its entire on-disk bundle matched. Private fields prevent callers from
/// manufacturing this proof from UI, project, model, or guest data.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedGuestRuntimeBundle {
    operation: GuestRunOperation,
    /// Exact directory path whose held handle was inventoried and hashed.
    /// Later launch planning must compare this with the operation's fixed root.
    root_path: PathBuf,
    /// Stable volume/device and file ID/inode captured before and after hashing.
    /// The final pre-launch gate must compare a freshly held handle to this value.
    root_identity: (u64, u64),
    bundle_id: String,
    toolchain_version: String,
    platform: String,
    architecture: String,
    manifest_sha256: String,
    artifact_count: usize,
    /// Complete authenticated relative-path inventory, normalized for Windows.
    /// The proof retains this so launch planning can require exact entrypoints.
    artifact_paths: std::collections::BTreeSet<String>,
}

/// Combine trust-anchor authentication and complete filesystem verification
/// into one gate. Later launch planning can require this proof rather than
/// accepting an independently supplied path or an unverified manifest.
#[allow(dead_code)]
fn verify_authenticated_guest_runtime_bundle(
    root_path: &Path,
    root: &File,
    operation: GuestRunOperation,
    manifest: &GuestRuntimeArtifactManifest,
    anchor: &GuestRuntimeManifestTrustAnchor,
) -> Result<VerifiedGuestRuntimeBundle, String> {
    authenticate_guest_runtime_manifest(operation, manifest, anchor)?;
    let root_identity = crate::coding_sandbox_fs::verified_directory_identity(root_path, root)
        .map_err(|_| "The guest runtime root identity could not be checked.".to_string())?;
    let expected_marker = build_guest_runtime_staging_owner_marker(manifest, anchor)?;
    verify_guest_runtime_artifact_tree_with_owner_marker(
        root, operation, manifest, Some(&expected_marker),
    )?;
    // Recheck after the complete inventory and digest pass to detect path or
    // directory-identity replacement while verification was in progress.
    let final_root_identity = crate::coding_sandbox_fs::verified_directory_identity(root_path, root)
        .map_err(|_| "The guest runtime root identity could not be rechecked.".to_string())?;
    if final_root_identity != root_identity {
        return Err("The guest runtime root identity changed during verification.".into());
    }
    Ok(VerifiedGuestRuntimeBundle {
        operation,
        root_path: root_path.to_path_buf(),
        root_identity,
        bundle_id: anchor.bundle_id.clone(),
        toolchain_version: anchor.toolchain_version.clone(),
        platform: anchor.platform.clone(),
        architecture: anchor.architecture.clone(),
        manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
        artifact_count: manifest.artifacts.len(),
        artifact_paths: manifest.artifacts.iter()
            .map(|artifact| artifact.relative_path.to_ascii_lowercase())
            .collect(),
    })
}



/// Revalidate the proof against a newly held directory handle immediately
/// before any future use. This closes the gap between an earlier successful
/// hash pass and a later path lookup: the same fixed path must still name the
/// exact volume/file identity recorded by the proof.
#[allow(dead_code)]
fn validate_verified_guest_runtime_bundle_root(
    proof: &VerifiedGuestRuntimeBundle,
    root_path: &Path,
    root: &File,
) -> Result<(), String> {
    if root_path != proof.root_path
        || !guest_runtime_root_matches_fixed_profile(proof.operation, root_path)
    {
        return Err("The runtime root path differs from the authenticated proof or fixed operation profile.".into());
    }
    let current_identity = crate::coding_sandbox_fs::verified_directory_identity(root_path, root)
        .map_err(|_| "The runtime root handle could not be revalidated.".to_string())?;
    if current_identity != proof.root_identity {
        return Err("The runtime root directory identity differs from the authenticated proof.".into());
    }
    Ok(())
}

/// Files that must be present in the authenticated bundle before a
/// particular fixed profile can be authorized.
fn required_guest_runtime_artifact_paths(
    operation: GuestRunOperation,
) -> std::collections::BTreeSet<String> {
    let paths: &[&str] = match operation {
        GuestRunOperation::RustCargoTest => &["bin\\cargo.exe"],
        GuestRunOperation::NodeNpmTest => &[
            "node.exe",
            "node_modules\\npm\\bin\\npm-cli.js",
        ],
        GuestRunOperation::PythonUnittest => &["python.exe"],
    };
    paths.iter().map(|path| path.to_ascii_lowercase()).collect()
}

/// Ensure every absolute executable/script path in the fixed launch profile
/// resolves to an exact relative path in the authenticated manifest inventory.
fn guest_runtime_launch_paths_are_manifested(
    operation: GuestRunOperation,
    root_path: &Path,
    launch: &GuestRuntimeLaunchPlan,
    artifact_paths: &std::collections::BTreeSet<String>,
) -> bool {
    let root = root_path.to_string_lossy().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let to_relative = |absolute: &Path| -> Option<String> {
        let full = absolute.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
        let prefix = format!("{root}\\");
        full.strip_prefix(&prefix).map(str::to_string)
    };
    let mut required = Vec::new();
    let Some(program) = to_relative(&launch.program) else { return false; };
    required.push(program);
    if operation == GuestRunOperation::NodeNpmTest {
        let Some(script) = launch.arguments.first() else { return false; };
        let Some(script) = to_relative(Path::new(script)) else { return false; };
        required.push(script);
    }
    required.into_iter().all(|path| artifact_paths.contains(&path))
}

/// Returns the one permitted installation root for each operation. These
/// paths are image-owned policy, not values supplied by the request or proof.
fn fixed_guest_runtime_root(operation: GuestRunOperation) -> &'static str {
    match operation {
        GuestRunOperation::RustCargoTest => r"C:\FRIDAY\Toolchains\Rust",
        GuestRunOperation::NodeNpmTest => r"C:\FRIDAY\Toolchains\Node",
        GuestRunOperation::PythonUnittest => r"C:\FRIDAY\Toolchains\Python",
    }
}

/// Compare a proof's recorded root with the exact operation-specific Windows
/// root. Slash normalization is only for representation; no path resolution or
/// filesystem access occurs here.
fn guest_runtime_root_matches_fixed_profile(
    operation: GuestRunOperation,
    root_path: &Path,
) -> bool {
    let supplied = root_path.to_string_lossy().replace('/', "\\");
    supplied.eq_ignore_ascii_case(fixed_guest_runtime_root(operation))
}

/// Reject inconsistent or non-canonical path inventories even if a proof is
/// accidentally corrupted inside trusted host code. The authenticated verifier
/// emits lowercase, normalized Windows-relative paths and an exact count.
fn verified_runtime_artifact_inventory_is_consistent(
    proof: &VerifiedGuestRuntimeBundle,
) -> bool {
    if proof.artifact_count == 0
        || proof.artifact_count > MAX_GUEST_RUNTIME_MANIFEST_ENTRIES
        || proof.artifact_paths.len() != proof.artifact_count
    {
        return false;
    }
    proof.artifact_paths.iter().all(|path| {
        !path.is_empty()
            && path == &path.to_ascii_lowercase()
            && !path.starts_with('\\')
            && !path.contains('/')
            && !path.contains(':')
            && path.split('\\').all(|component| {
                !component.is_empty()
                    && component != "."
                    && component != ".."
                    && component.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
    })
}

/// A fully derived, but deliberately non-executable, authorization plan.
/// Every field is derived from the private verification proof and fixed policy.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedGuestRuntimeExecutionPlan {
    operation: GuestRunOperation,
    /// The exact verified runtime root and stable filesystem identity are
    /// carried forward so later gates cannot detach the plan from its proof.
    root_path: PathBuf,
    root_identity: (u64, u64),
    bundle_id: String,
    toolchain_version: String,
    manifest_sha256: String,
    artifact_count: usize,
    required_artifact_paths: std::collections::BTreeSet<String>,
    launch: GuestRuntimeLaunchPlan,
    environment: GuestRuntimeEnvironmentPlan,
    environment_block: Vec<u16>,
}

/// Convert verified bundle evidence into the only launch/environment profile
/// allowed for that operation. This does not create a process, start Sandbox,
/// or imply that a production bundle has been provisioned.
#[allow(dead_code)]
fn build_verified_guest_runtime_execution_plan(
    proof: &VerifiedGuestRuntimeBundle,
) -> Result<VerifiedGuestRuntimeExecutionPlan, String> {
    if !guest_runtime_root_matches_fixed_profile(proof.operation, &proof.root_path)
        || proof.platform != "windows"
        || proof.architecture != "x86_64"
        || proof.bundle_id.is_empty()
        || proof.bundle_id.len() > 96
        || !proof.bundle_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || proof.toolchain_version.is_empty()
        || proof.toolchain_version.len() > 64
        || !proof.toolchain_version.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        || proof.manifest_sha256.len() != 64
        || !proof.manifest_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !verified_runtime_artifact_inventory_is_consistent(proof)
    {
        return Err("The verified runtime proof does not match the fixed operation, platform, or installation profile.".into());
    }

    let required_artifact_paths = required_guest_runtime_artifact_paths(proof.operation);
    if !required_artifact_paths.is_subset(&proof.artifact_paths) {
        return Err("The authenticated runtime bundle is missing one or more fixed executable entrypoints.".into());
    }
    let launch = build_guest_runtime_launch_plan(proof.operation);
    validate_guest_runtime_launch_plan(proof.operation, &launch)?;
    if !guest_runtime_launch_paths_are_manifested(
        proof.operation, &proof.root_path, &launch, &proof.artifact_paths,
    ) {
        return Err("The fixed launch executable or script is not present in the authenticated runtime manifest.".into());
    }
    let environment = build_guest_runtime_environment_plan(proof.operation);
    validate_guest_runtime_environment_plan(proof.operation, &environment)?;
    let environment_block =
        serialize_guest_runtime_environment_block(proof.operation, &environment)?;

    Ok(VerifiedGuestRuntimeExecutionPlan {
        operation: proof.operation,
        root_path: proof.root_path.clone(),
        root_identity: proof.root_identity,
        bundle_id: proof.bundle_id.clone(),
        toolchain_version: proof.toolchain_version.clone(),
        manifest_sha256: proof.manifest_sha256.to_ascii_lowercase(),
        artifact_count: proof.artifact_count,
        required_artifact_paths,
        launch,
        environment,
        environment_block,
    })
}

/// Rebuild the entire derived plan from the authenticated proof and compare it
/// byte-for-byte/field-for-field. A caller cannot substitute command, paths,
/// environment values, or serialized environment data.
#[allow(dead_code)]
fn validate_verified_guest_runtime_execution_plan(
    proof: &VerifiedGuestRuntimeBundle,
    plan: &VerifiedGuestRuntimeExecutionPlan,
) -> Result<(), String> {
    let expected = build_verified_guest_runtime_execution_plan(proof)?;
    if plan != &expected {
        return Err("The verified runtime execution plan differs from the proof-bound fixed profile.".into());
    }
    Ok(())
}

/// Final pure authorization gate for a future runtime consumer. It validates
/// the complete proof-derived plan and then reopens/rechecks the exact runtime
/// root identity. Callers must use this immediately before any later handoff;
/// this function itself never launches a process or starts Windows Sandbox.
#[allow(dead_code)]
fn validate_verified_guest_runtime_execution_context(
    proof: &VerifiedGuestRuntimeBundle,
    plan: &VerifiedGuestRuntimeExecutionPlan,
    root_path: &Path,
    root: &File,
) -> Result<(), String> {
    validate_verified_guest_runtime_execution_plan(proof, plan)?;
    if plan.root_path != root_path || plan.root_identity != proof.root_identity {
        return Err("The runtime execution plan is detached from the supplied verified root.".into());
    }
    validate_verified_guest_runtime_bundle_root(proof, root_path, root)
}

/// A one-use authorization result that binds the user's approved run request
/// to a verified runtime bundle and a freshly revalidated root identity.
/// This value is still not a process handle and cannot launch Windows Sandbox.
#[derive(Debug)]
struct AuthorizedVerifiedGuestRun {
    run: GuestRunExecutionPlan,
    proof: VerifiedGuestRuntimeBundle,
    runtime: VerifiedGuestRuntimeExecutionPlan,
    request_bytes: Vec<u8>,
}

/// The final host-side authorization gate. All runtime/proof checks happen
/// before the one-time approval is consumed, so a stale or substituted runtime
/// cannot burn an otherwise valid approval. No process or Sandbox is created.
#[allow(dead_code)]
fn authorize_verified_guest_runtime_execution(
    approvals: &mut GuestRunApprovalStore,
    request: GuestRunRequest,
    token: &str,
    proof: &VerifiedGuestRuntimeBundle,
    runtime_plan: &VerifiedGuestRuntimeExecutionPlan,
    root_path: &Path,
    root: &File,
    now: u64,
) -> Result<AuthorizedVerifiedGuestRun, String> {
    let run = build_guest_run_execution_plan(request)?;
    if run.operation != proof.operation || runtime_plan.operation != run.operation {
        return Err("The approved guest operation does not match the verified runtime operation.".into());
    }
    validate_verified_guest_runtime_execution_context(
        proof, runtime_plan, root_path, root,
    )?;
    let request_bytes = serialize_guest_run_execution_request(&run)?;
    approvals.consume(&run, token, now)?;
    Ok(AuthorizedVerifiedGuestRun {
        run,
        proof: proof.clone(),
        runtime: runtime_plan.clone(),
        request_bytes,
    })
}

/// Validate the immutable contents of an authorization package before it is
/// handed to any later trusted stage. This checks the exact request bytes,
/// typed operation, private verification proof, and complete derived runtime
/// plan. It performs no filesystem access and cannot execute a process.
#[allow(dead_code)]
fn validate_authorized_guest_run_payload(
    authorized: &AuthorizedVerifiedGuestRun,
) -> Result<(), String> {
    validate_guest_run_execution_plan(&authorized.run)?;
    if authorized.run.operation != authorized.proof.operation
        || authorized.runtime.operation != authorized.run.operation
    {
        return Err("The authorized run and verified runtime operations do not match.".into());
    }
    validate_verified_guest_runtime_execution_plan(&authorized.proof, &authorized.runtime)?;
    let expected_request = serialize_guest_run_execution_request(&authorized.run)?;
    if authorized.request_bytes != expected_request {
        return Err("The authorized guest request bytes differ from the exact approved run.".into());
    }
    Ok(())
}

/// Final handoff validation for an already-approved package. It rechecks the
/// immutable package and then the same held runtime root identity immediately
/// before a future consumer. This remains authorization only, not execution.
#[allow(dead_code)]
fn validate_authorized_guest_run_context(
    authorized: &AuthorizedVerifiedGuestRun,
    root_path: &Path,
    root: &File,
) -> Result<(), String> {
    validate_authorized_guest_run_payload(authorized)?;
    validate_verified_guest_runtime_execution_context(
        &authorized.proof, &authorized.runtime, root_path, root,
    )
}

/// Windows reserves DOS device names even when they have an extension.
/// Reject them in every path component so a trusted manifest has one stable
/// interpretation on every supported host and inside Windows Sandbox.
fn is_reserved_windows_runtime_component(component: &str) -> bool {
    if component.ends_with('.') {
        return true;
    }

    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && (b'1'..=b'9').contains(&upper.as_bytes()[3]))
}

/// Validate an image-owned artifact manifest before any future file access.
/// The manifest must come from FRIDAY's trusted, version-pinned provisioning
/// metadata; guest, project, and model content must never supply it.
#[allow(dead_code)]
fn validate_guest_runtime_artifact_manifest(
    operation: GuestRunOperation,
    manifest: &GuestRuntimeArtifactManifest,
) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.operation != operation
        || manifest.artifacts.is_empty()
        || manifest.artifacts.len() > MAX_GUEST_RUNTIME_MANIFEST_ENTRIES
    {
        return Err("The guest runtime artifact manifest is invalid or outside FRIDAY's fixed policy.".into());
    }

    let mut paths = std::collections::BTreeSet::<String>::new();
    let mut total_limit = 0u64;
    for artifact in &manifest.artifacts {
        let path = artifact.relative_path.as_str();
        let components = path.split('\\').collect::<Vec<_>>();
        let normalized_path = path.to_ascii_lowercase();
        let has_file_directory_collision = components.iter().enumerate().skip(1).any(|(index, _)| {
            paths.contains(&components[..index].join("\\").to_ascii_lowercase())
        }) || paths.iter().any(|existing| existing.starts_with(&(normalized_path.clone() + "\\")));
        if path.is_empty()
            || path.len() > MAX_GUEST_RUNTIME_ARTIFACT_PATH_BYTES
            || path.starts_with('\\')
            || path.contains('/')
            || path.contains(':')
            || components.iter().any(|part| {
                part.is_empty()
                    || part.len() > MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES
                    || *part == "."
                    || *part == ".."
                    || is_reserved_windows_runtime_component(part)
                    || !part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            })
            || normalized_path == ".friday-runtime-owner.json"
            || paths.contains(&normalized_path)
            || has_file_directory_collision
            || artifact.sha256.len() != 64
            || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || artifact.max_bytes == 0
            || artifact.max_bytes > MAX_GUEST_RUNTIME_ARTIFACT_BYTES
        {
            return Err("The guest runtime artifact manifest contains an unsafe path, reserved marker, collision, digest, or size limit.".into());
        }
        paths.insert(normalized_path);
        total_limit = total_limit.checked_add(artifact.max_bytes)
            .ok_or("The guest runtime artifact manifest size limit overflowed.")?;
        if total_limit > MAX_GUEST_RUNTIME_MANIFEST_BYTES {
            return Err("The guest runtime artifact manifest exceeds FRIDAY's aggregate size limit.".into());
        }
    }

    let required_entrypoint = match operation {
        GuestRunOperation::RustCargoTest => "bin\\cargo.exe",
        GuestRunOperation::NodeNpmTest => "node.exe",
        GuestRunOperation::PythonUnittest => "python.exe",
    };
    if !paths.contains(&required_entrypoint.to_ascii_lowercase()) {
        return Err("The guest runtime artifact manifest omits its fixed operation entrypoint.".into());
    }
    if operation == GuestRunOperation::NodeNpmTest
        && !paths.contains("node_modules\\npm\\bin\\npm-cli.js")
    {
        return Err("The Node runtime manifest omits the pinned npm entrypoint.".into());
    }
    Ok(())
}

/// Compare bounded artifact bytes with the SHA-256 value from the trusted
/// manifest. This pure verifier performs no path lookup or provisioning.
#[allow(dead_code)]
fn verify_guest_runtime_artifact_bytes(
    artifact: &GuestRuntimeArtifactSpec,
    bytes: &[u8],
) -> Result<(), String> {
    if artifact.max_bytes == 0
        || artifact.max_bytes > MAX_GUEST_RUNTIME_ARTIFACT_BYTES
        || bytes.is_empty()
        || bytes.len() as u64 > artifact.max_bytes
        || artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("The guest runtime artifact is empty, oversized, or has an invalid expected digest.".into());
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let actual = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if !actual.eq_ignore_ascii_case(&artifact.sha256) {
        return Err("A guest runtime artifact does not match its trusted SHA-256 digest.".into());
    }
    Ok(())
}

/// Read a manifest-listed runtime artifact through no-follow, handle-relative
/// filesystem operations, then verify the exact bytes against its trusted digest.
/// The root handle must itself have been acquired from an authenticated,
/// version-pinned provisioning location by a future trusted provisioner.
#[allow(dead_code)]
fn verify_guest_runtime_artifact_from_root(
    root: &File,
    artifact: &GuestRuntimeArtifactSpec,
) -> Result<(), String> {
    let bytes = crate::coding_sandbox_fs::read_bounded_relative_file(
        root, &artifact.relative_path, artifact.max_bytes,
    ).map_err(|_| "A guest runtime artifact could not be safely opened and read.".to_string())?;
    verify_guest_runtime_artifact_bytes(artifact, &bytes)
}

/// Verify the complete runtime directory against its authenticated manifest.
/// Both file and directory inventories must match exactly, so unlisted payloads,
/// empty/injected directories, links, and missing files fail closed. Each listed
/// file is then reopened relative to the held root and checked against its digest.
#[allow(dead_code)]
fn verify_guest_runtime_artifact_tree(
    root: &File,
    operation: GuestRunOperation,
    manifest: &GuestRuntimeArtifactManifest,
) -> Result<(), String> {
    verify_guest_runtime_artifact_tree_with_owner_marker(root, operation, manifest, None)
}

fn verify_guest_runtime_artifact_tree_with_owner_marker(
    root: &File,
    operation: GuestRunOperation,
    manifest: &GuestRuntimeArtifactManifest,
    expected_owner_marker: Option<&[u8]>,
) -> Result<(), String> {
    validate_guest_runtime_artifact_manifest(operation, manifest)?;
    let (mut actual_files, mut actual_directories) =
        crate::coding_sandbox_fs::collect_runtime_tree_inventory(root, 32, 1280)
            .map_err(|_| "The guest runtime directory contains unsafe, unexpected, or excessive entries.".to_string())?;
    // Published bundles retain the staging ownership marker. It is not a
    // runtime artifact, so authenticate its exact bytes and remove it from
    // the payload inventory before enforcing manifest closure.
    if let Some(expected_marker) = expected_owner_marker {
        let marker_name = ".friday-runtime-owner.json";
        let positions = actual_files.iter().enumerate()
            .filter(|(_, path)| path.eq_ignore_ascii_case(marker_name))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if positions.len() != 1 {
            return Err("The published runtime must contain exactly one ownership marker.".into());
        }
        actual_files.remove(positions[0]);
        let marker = crate::coding_sandbox_fs::read_bounded_relative_file(
            root, marker_name, 16 * 1024,
        ).map_err(|_| "The published runtime ownership marker is missing or unsafe.".to_string())?;
        if marker != expected_marker {
            return Err("The published runtime ownership marker does not match its authenticated bundle.".into());
        }
    }
    // The inventory deliberately emits Windows-style relative separators
    // on every host. Keep the authenticated manifest in that same canonical
    // representation when comparing exact file and directory closure.
    let mut expected_files = manifest.artifacts.iter()
        .map(|artifact| artifact.relative_path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut expected_directories = std::collections::BTreeSet::new();
    for artifact in &manifest.artifacts {
        let components = artifact.relative_path.split('\\').collect::<Vec<_>>();
        for depth in 1..components.len() {
            expected_directories.insert(components[..depth].join("\\").to_ascii_lowercase());
        }
    }
    actual_files.iter_mut().for_each(|path| *path = path.to_ascii_lowercase());
    actual_directories.iter_mut().for_each(|path| *path = path.to_ascii_lowercase());
    expected_files.sort();
    actual_files.sort();
    let mut expected_directories = expected_directories.into_iter().collect::<Vec<_>>();
    expected_directories.sort();
    actual_directories.sort();
    if actual_files != expected_files || actual_directories != expected_directories {
        return Err("The guest runtime directory does not exactly match its authenticated artifact manifest.".into());
    }
    for artifact in &manifest.artifacts {
        verify_guest_runtime_artifact_from_root(root, artifact)?;
    }
    Ok(())
}

/// Immutable, policy-bounded description of one guest test run. It deliberately
/// contains no executable path, working-directory path, environment, or
/// caller-supplied arguments; those are resolved only by later trusted stages.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestRunExecutionPlan {
    run_id: String,
    snapshot_id: String,
    operation: GuestRunOperation,
    command: GuestOperationCommand,
    max_duration: Duration,
    max_output_bytes: u64,
}

fn build_guest_run_execution_plan(
    request: GuestRunRequest,
) -> Result<GuestRunExecutionPlan, String> {
    let request = validate_guest_run_request(request)?;
    Ok(GuestRunExecutionPlan {
        run_id: request.run_id,
        snapshot_id: request.snapshot_id,
        operation: request.operation,
        command: guest_operation_command(request.operation),
        max_duration: Duration::from_millis(request.max_duration_ms),
        max_output_bytes: request.max_output_bytes,
    })
}

/// Produces the exact bounded protocol document represented by an approved
/// immutable plan. This is a serialization boundary only: it does not stage,
/// mount, or transmit the request to a guest.
#[allow(dead_code)]
fn serialize_guest_run_execution_request(
    plan: &GuestRunExecutionPlan,
) -> Result<Vec<u8>, String> {
    validate_guest_run_execution_plan(plan)?;
    let request = GuestRunRequest {
        schema_version: 1,
        run_id: plan.run_id.clone(),
        snapshot_id: plan.snapshot_id.clone(),
        operation: plan.operation,
        max_duration_ms: u64::try_from(plan.max_duration.as_millis())
            .map_err(|_| "The approved guest duration cannot be represented safely.")?,
        max_output_bytes: plan.max_output_bytes,
    };
    let bytes = serde_json::to_vec(&request)
        .map_err(|_| "FRIDAY could not serialize the approved guest request.")?;
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err("The approved guest request exceeds the protocol size limit.".into());
    }
    let parsed = parse_guest_run_request(&bytes)?;
    if parsed.run_id != plan.run_id
        || parsed.snapshot_id != plan.snapshot_id
        || parsed.operation != plan.operation
        || parsed.max_duration_ms != plan.max_duration.as_millis() as u64
        || parsed.max_output_bytes != plan.max_output_bytes
    {
        return Err("The serialized guest request does not match the approved execution plan.".into());
    }
    Ok(bytes)
}

const GUEST_RUN_APPROVAL_TTL_SECONDS: u64 = 120;

/// A short-lived authorization for one exact immutable execution plan.
/// The token is generated by FRIDAY and is never supplied by the model or guest.
struct GuestRunApprovalGrant {
    plan: GuestRunExecutionPlan,
    token: String,
    expires_at: u64,
    consumed: bool,
}

fn validate_guest_run_execution_plan(plan: &GuestRunExecutionPlan) -> Result<(), String> {
    if !valid_snapshot_id(&plan.run_id)
        || !valid_snapshot_id(&plan.snapshot_id)
        || plan.max_duration.is_zero()
        || plan.max_duration > Duration::from_millis(MAX_GUEST_REQUEST_DURATION_MS)
        || plan.max_output_bytes == 0
        || plan.max_output_bytes > MAX_GUEST_REQUEST_OUTPUT_BYTES
        || plan.command != guest_operation_command(plan.operation)
    {
        return Err("The guest execution plan is invalid or outside FRIDAY's fixed policy.".into());
    }
    Ok(())
}

fn issue_guest_run_approval(
    plan: &GuestRunExecutionPlan,
    now: u64,
) -> Result<(String, GuestRunApprovalGrant), String> {
    validate_guest_run_execution_plan(plan)?;
    let expires_at = now.checked_add(GUEST_RUN_APPROVAL_TTL_SECONDS)
        .ok_or("FRIDAY could not establish a safe approval expiry.")?;
    let mut random = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut random);
    let token = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok((token.clone(), GuestRunApprovalGrant {
        plan: plan.clone(),
        token,
        expires_at,
        consumed: false,
    }))
}

/// Consumes approval once, and only for the exact plan the user approved.
/// The caller must retain this grant in trusted host state and discard it after use.
fn consume_guest_run_approval(
    grant: &mut GuestRunApprovalGrant,
    token: &str,
    plan: &GuestRunExecutionPlan,
    now: u64,
) -> Result<(), String> {
    validate_guest_run_execution_plan(plan)?;
    if grant.consumed || now >= grant.expires_at || grant.expires_at.saturating_sub(now) > GUEST_RUN_APPROVAL_TTL_SECONDS
        || grant.plan != *plan || !constant_time_approval_token_eq(&grant.token, token)
    {
        return Err("Guest execution approval is expired, already used, or does not match this exact run.".into());
    }
    grant.consumed = true;
    Ok(())
}

fn constant_time_approval_token_eq(expected: &str, supplied: &str) -> bool {
    if expected.len() != 48 || supplied.len() != 48 {
        return false;
    }
    expected.as_bytes().iter().zip(supplied.as_bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right)) == 0
}

const MAX_PENDING_GUEST_RUN_APPROVALS: usize = 16;

/// Host-memory approval registry. Entries are indexed by run ID, so issuing a
/// replacement for the same run invalidates its previous token atomically.
#[derive(Default)]
struct GuestRunApprovalStore {
    grants: BTreeMap<String, GuestRunApprovalGrant>,
}

impl GuestRunApprovalStore {
    fn prune_expired(&mut self, now: u64) {
        self.grants.retain(|_, grant| {
            grant.expires_at > now
                && grant.expires_at.saturating_sub(now) <= GUEST_RUN_APPROVAL_TTL_SECONDS
                && !grant.consumed
        });
    }

    fn issue(&mut self, plan: &GuestRunExecutionPlan, now: u64) -> Result<String, String> {
        self.prune_expired(now);
        if !self.grants.contains_key(&plan.run_id)
            && self.grants.len() >= MAX_PENDING_GUEST_RUN_APPROVALS
        {
            return Err("FRIDAY already has the maximum number of pending guest approvals. Cancel or expire an existing run before approving another.".into());
        }
        let (token, grant) = issue_guest_run_approval(plan, now)?;
        self.grants.insert(plan.run_id.clone(), grant);
        Ok(token)
    }

    /// Removes the entry before checking it: every consume attempt is terminal,
    /// including mismatches, so a token cannot be probed or replayed.
    fn consume(
        &mut self,
        plan: &GuestRunExecutionPlan,
        token: &str,
        now: u64,
    ) -> Result<(), String> {
        let grant = self.grants.remove(&plan.run_id)
            .ok_or("No pending approval exists for this guest run. Review and approve it again.")?;
        let mut grant = grant;
        consume_guest_run_approval(&mut grant, token, plan, now)
    }

    fn revoke(&mut self, run_id: &str) -> bool {
        self.grants.remove(run_id).is_some()
    }

    fn pending_count(&self) -> usize {
        self.grants.len()
    }
}

static GUEST_RUN_APPROVALS: Mutex<GuestRunApprovalStore> =
    Mutex::new(GuestRunApprovalStore { grants: BTreeMap::new() });

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestRunApprovalReceipt {
    token: String,
    expires_at_unix_seconds: u64,
}

fn guest_run_now_unix_seconds() -> Result<u64, String> {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "FRIDAY could not read a trustworthy system clock for guest approval.".into())
}

/// Called only after the user explicitly approves the exact guest-run review.
/// The request is parsed again here; no command or path is accepted from the UI.
#[tauri::command]
pub fn issue_guest_run_execution_approval(request_json: String) -> Result<GuestRunApprovalReceipt, String> {
    let request = parse_guest_run_request(request_json.as_bytes())?;
    let plan = build_guest_run_execution_plan(request)?;
    let now = guest_run_now_unix_seconds()?;
    let token = {
        let mut approvals = GUEST_RUN_APPROVALS.lock()
            .map_err(|_| "FRIDAY's guest approval registry is unavailable; restart FRIDAY before retrying.".to_string())?;
        approvals.issue(&plan, now)?
    };
    let expires_at_unix_seconds = now.checked_add(GUEST_RUN_APPROVAL_TTL_SECONDS)
        .ok_or("FRIDAY could not establish a safe approval expiry.")?;
    Ok(GuestRunApprovalReceipt { token, expires_at_unix_seconds })
}

/// Consumes the one-time approval for this exact request. This does not execute it.
#[tauri::command]
pub fn consume_guest_run_execution_approval(request_json: String, token: String) -> Result<(), String> {
    let request = parse_guest_run_request(request_json.as_bytes())?;
    let plan = build_guest_run_execution_plan(request)?;
    let now = guest_run_now_unix_seconds()?;
    let mut approvals = GUEST_RUN_APPROVALS.lock()
        .map_err(|_| "FRIDAY's guest approval registry is unavailable; restart FRIDAY before retrying.".to_string())?;
    approvals.consume(&plan, &token, now)
}

/// Cancels a pending approval. Run IDs are fixed-width hexadecimal identifiers.
#[tauri::command]
pub fn revoke_guest_run_execution_approval(run_id: String) -> Result<bool, String> {
    if !valid_snapshot_id(&run_id) {
        return Err("The guest run identifier is invalid.".into());
    }
    let mut approvals = GUEST_RUN_APPROVALS.lock()
        .map_err(|_| "FRIDAY's guest approval registry is unavailable; restart FRIDAY before retrying.".to_string())?;
    Ok(approvals.revoke(&run_id))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GuestRunStatus {
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Unsupported,
    RunnerError,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestRunResult {
    schema_version: u32,
    run_id: String,
    snapshot_id: String,
    operation: GuestRunOperation,
    status: GuestRunStatus,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
    truncated: bool,
    duration_ms: u64,
}

fn validate_guest_result_json(
    bytes: &[u8],
    expected_run_id: &str,
    expected_snapshot_id: &str,
    expected_operation: GuestRunOperation,
) -> Result<GuestRunResult, String> {
    validate_guest_result_json_with_limit(
        bytes,
        expected_run_id,
        expected_snapshot_id,
        expected_operation,
        MAX_GUEST_RESULT_BYTES as u64,
    )
}


const GUEST_RESULT_FILE_NAME: &str = "result.json";

fn read_verified_guest_result_file(
    result_directory: &Path,
    expected_run_id: &str,
    expected_snapshot_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
    max_duration_ms: u64,
) -> Result<GuestRunResult, String> {
    let bytes = crate::coding_sandbox_fs::read_single_bounded_file_from_directory(
        result_directory,
        OsStr::new(GUEST_RESULT_FILE_NAME),
        MAX_GUEST_RESULT_BYTES as u64,
    ).map_err(|_| "The guest result directory or file could not be safely verified and read.".to_string())?;
    validate_guest_result_json_with_limits(
        &bytes, expected_run_id, expected_snapshot_id, expected_operation,
        max_output_bytes, max_duration_ms,
    )
}

fn validate_guest_result_json_with_limit(
    bytes: &[u8],
    expected_run_id: &str,
    expected_snapshot_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
) -> Result<GuestRunResult, String> {
    validate_guest_result_json_with_limits(
        bytes, expected_run_id, expected_snapshot_id, expected_operation,
        max_output_bytes, MAX_GUEST_RESULT_DURATION_MS,
    )
}

fn validate_guest_result_json_with_limits(
    bytes: &[u8],
    expected_run_id: &str,
    expected_snapshot_id: &str,
    expected_operation: GuestRunOperation,
    max_output_bytes: u64,
    max_duration_ms: u64,
) -> Result<GuestRunResult, String> {
    if max_output_bytes == 0 || max_output_bytes > MAX_GUEST_REQUEST_OUTPUT_BYTES {
        return Err("The expected guest output limit is outside FRIDAY's policy.".into());
    }
    if max_duration_ms == 0 || max_duration_ms > MAX_GUEST_RESULT_DURATION_MS {
        return Err("The expected guest duration limit is outside FRIDAY's policy.".into());
    }
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_BYTES {
        return Err("The guest result is empty or exceeds FRIDAY's size limit.".into());
    }
    if !valid_snapshot_id(expected_run_id) || !valid_snapshot_id(expected_snapshot_id) {
        return Err("The expected guest run or snapshot identifier is invalid.".into());
    }
    let result: GuestRunResult = serde_json::from_slice(bytes)
        .map_err(|_| "The guest result is malformed, contains duplicate or unknown fields, or is not valid UTF-8 JSON.".to_string())?;
    if result.schema_version != 1
        || result.run_id != expected_run_id
        || result.snapshot_id != expected_snapshot_id
        || result.operation != expected_operation
        || result.duration_ms > max_duration_ms
    {
        return Err("The guest result does not match FRIDAY's run contract.".into());
    }
    let output_bytes = result.stdout.as_ref().map_or(0, |value| value.len())
        .saturating_add(result.stderr.as_ref().map_or(0, |value| value.len()));
    if output_bytes as u64 > max_output_bytes {
        return Err("The guest result diagnostics exceed the limit assigned to this run.".into());
    }
    match (result.status, result.exit_code) {
        (GuestRunStatus::Passed, Some(0)) => {}
        (GuestRunStatus::Failed, Some(code)) if code != 0 => {}
        (GuestRunStatus::TimedOut | GuestRunStatus::Cancelled | GuestRunStatus::Unsupported | GuestRunStatus::RunnerError, None) => {}
        _ => return Err("The guest result has an inconsistent status and exit code.".into()),
    }
    Ok(result)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxPreflight {
    platform_supported: bool,
    runtime_detected: bool,
    execution_enabled: bool,
    network_isolation_enforced: bool,
    windows_edition: String,
    windows_build: String,
    sandbox_feature_state: String,
    wsb_cli_available: bool,
    wsb_cli_version: String,
    virtualization_firmware_enabled: Option<bool>,
    sandbox_process_detected: bool,
    virtual_disk_api_available: bool,
    manage_volume_privilege_enabled: bool,
    hyperv_cmdlets_available: bool,
    mapped_folders_policy: String,
    mapped_folder_write_policy: String,
    result_storage_ready: bool,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxSnapshot {
    snapshot_id: String,
    files_copied: usize,
    bytes_copied: u64,
    truncated: bool,
    message: String,
}


#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotOwnershipMarker {
    schema_version: u32,
    snapshot_id: String,
    owner: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSandboxRecovery {
    removed_snapshots: usize,
    preserved_unverified_entries: usize,
    message: String,
}

static OWNED_SNAPSHOTS: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

fn valid_snapshot_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_snapshot_ownership_marker(snapshot_path: &Path, snapshot_id: &str) -> Result<(), String> {
    let marker_path = snapshot_path.join(SNAPSHOT_OWNER_MARKER);
    let marker = SnapshotOwnershipMarker {
        schema_version: SNAPSHOT_OWNER_SCHEMA,
        snapshot_id: snapshot_id.to_string(),
        owner: "FRIDAY-CodingAssistant".to_string(),
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare snapshot ownership metadata.".to_string())?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not safely create snapshot ownership metadata.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write snapshot ownership metadata.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist snapshot ownership metadata.".to_string())?;
    Ok(())
}

fn verified_owned_snapshot_path(workspace_root: &Path, snapshot_id: &str) -> Result<PathBuf, String> {
    if !valid_snapshot_id(snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let root_meta = fs::symlink_metadata(workspace_root)
        .map_err(|_| "The private snapshot workspace is unavailable.".to_string())?;
    if !root_meta.is_dir() || is_reparse_or_symlink(&root_meta) {
        return Err("The private snapshot workspace is not a normal directory.".into());
    }
    let canonical_root = fs::canonicalize(workspace_root)
        .map_err(|_| "FRIDAY could not verify the private snapshot workspace.".to_string())?;
    let candidate = workspace_root.join(snapshot_id);
    let candidate_meta = fs::symlink_metadata(&candidate)
        .map_err(|_| "The snapshot folder is unavailable.".to_string())?;
    if !candidate_meta.is_dir() || is_reparse_or_symlink(&candidate_meta) {
        return Err("The snapshot folder is not a normal directory.".into());
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|_| "FRIDAY could not verify the snapshot folder.".to_string())?;
    // Windows canonicalization may add an extended-length prefix (\\?\) that is
    // not present in the original candidate path. Verify the canonical parent and
    // exact child name instead of comparing differently normalized path strings.
    if canonical_candidate.parent() != Some(canonical_root.as_path())
        || canonical_candidate.file_name().and_then(OsStr::to_str) != Some(snapshot_id)
    {
        return Err("The snapshot folder failed its ownership-path check.".into());
    }
    let marker_path = canonical_candidate.join(SNAPSHOT_OWNER_MARKER);
    let marker_meta = fs::symlink_metadata(&marker_path)
        .map_err(|_| "Snapshot ownership evidence is missing; cleanup was refused.".to_string())?;
    if !marker_meta.is_file() || is_reparse_or_symlink(&marker_meta) || marker_meta.len() > 1024 {
        return Err("Snapshot ownership evidence is not a small regular file; cleanup was refused.".into());
    }
    let mut marker_file = open_source_file(&marker_path)
        .map_err(|_| "Snapshot ownership evidence could not be opened without following links.".to_string())?;
    let opened_marker_meta = marker_file.metadata()
        .map_err(|_| "Snapshot ownership evidence could not be verified.".to_string())?;
    if !opened_marker_meta.is_file() || is_reparse_or_symlink(&opened_marker_meta) || opened_marker_meta.len() > 1024 {
        return Err("Snapshot ownership evidence changed or exceeded its size limit; cleanup was refused.".into());
    }
    let mut bytes = Vec::new();
    marker_file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "Snapshot ownership evidence could not be read; cleanup was refused.".to_string())?;
    if bytes.len() > 1024 {
        return Err("Snapshot ownership evidence exceeded its read limit; cleanup was refused.".into());
    }
    let marker: SnapshotOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "Snapshot ownership evidence is malformed; cleanup was refused.".to_string())?;
    if marker.schema_version != SNAPSHOT_OWNER_SCHEMA
        || marker.snapshot_id != snapshot_id
        || marker.owner != "FRIDAY-CodingAssistant"
    {
        return Err("Snapshot ownership evidence does not match this FRIDAY workspace; cleanup was refused.".into());
    }
    Ok(canonical_candidate)
}

fn remove_verified_owned_snapshot(workspace_root: &Path, snapshot_id: &str) -> Result<(), String> {
    let verified = verified_owned_snapshot_path(workspace_root, snapshot_id)?;
    fs::remove_dir_all(&verified)
        .map_err(|_| "FRIDAY could not completely remove the verified snapshot folder.".to_string())
}


fn detect_windows_sandbox_at(system_root: Option<&OsStr>) -> bool {
    let Some(system_root) = system_root else { return false; };
    let executable = PathBuf::from(system_root).join("System32").join("WindowsSandbox.exe");
    fs::symlink_metadata(executable)
        .map(|metadata| metadata.is_file() && !is_reparse_or_symlink(&metadata))
        .unwrap_or(false)
}

fn detect_windows_sandbox() -> bool {
    detect_windows_sandbox_at(env::var_os("SystemRoot").as_deref())
}

#[cfg(windows)]
pub(crate) fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || (metadata.file_attributes() & 0x400 != 0)
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[derive(Debug, PartialEq, Eq)]
struct BoundedCommandOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Retains at most max_bytes while continuing to drain the pipe to EOF.
/// This prevents a child from blocking forever when its output exceeds the
/// amount FRIDAY is willing to keep in memory.
fn read_bounded_command_output<R: Read>(
    mut reader: R,
    max_bytes: usize,
) -> io::Result<BoundedCommandOutput> {
    let mut retained = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut buffer = [0u8; 8 * 1024];
    let mut truncated = false;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        if keep < read {
            truncated = true;
        }
    }

    Ok(BoundedCommandOutput { bytes: retained, truncated })
}

#[cfg(windows)]
const WINDOWS_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(windows)]
const MAX_PREFLIGHT_OUTPUT_BYTES: u64 = 64 * 1024;

#[cfg(windows)]
fn run_bounded_probe(mut command: Command) -> Result<(ExitStatus, Vec<u8>), String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "The Windows Sandbox capability probe could not be started.".to_string())?;

    let stdout = child.stdout.take()
        .ok_or_else(|| "The Windows Sandbox capability probe output was unavailable.".to_string())?;
    let stderr = child.stderr.take()
        .ok_or_else(|| "The Windows Sandbox capability probe diagnostics were unavailable.".to_string())?;

    let stdout_reader = thread::spawn(move || {
        read_bounded_command_output(stdout, MAX_PREFLIGHT_OUTPUT_BYTES as usize)
    });
    let stderr_reader = thread::spawn(move || {
        read_bounded_command_output(stderr, MAX_PREFLIGHT_OUTPUT_BYTES as usize)
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < WINDOWS_PREFLIGHT_TIMEOUT => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("The Windows Sandbox capability probe timed out; readiness is unverified.".into());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("The Windows Sandbox capability probe could not be monitored; readiness is unverified.".into());
            }
        }
    };

    let stdout = stdout_reader.join()
        .map_err(|_| "The Windows Sandbox capability probe output could not be collected.".to_string())?
        .map_err(|_| "The Windows Sandbox capability probe output could not be read.".to_string())?;
    let stderr = stderr_reader.join()
        .map_err(|_| "The Windows Sandbox capability probe diagnostics could not be collected.".to_string())?
        .map_err(|_| "The Windows Sandbox capability probe diagnostics could not be read.".to_string())?;
    if stdout.truncated || stderr.truncated {
        return Err("The Windows Sandbox capability probe exceeded its fixed output limit; readiness is unverified.".into());
    }
    Ok((status, stdout.bytes))
}

#[cfg(windows)]
fn detect_windows_sandbox_capabilities() -> (String, String, String, bool, String, String, Option<bool>, bool, bool, bool, bool, String, String) {
    // Fixed, read-only PowerShell probes. No user or model supplied command text is accepted.
    let script = r#"$ErrorActionPreference = 'SilentlyContinue'
$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$feature = Get-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty VirtualizationFirmwareEnabled
$active = [bool](Get-Process -Name WindowsSandbox -ErrorAction SilentlyContinue)
$wsb = Get-Command wsb.exe -CommandType Application -ErrorAction SilentlyContinue
$wsbPath = if ($wsb -and $wsb.Source) { [string]$wsb.Source } else { '' }
$version = ''
$signatureStatus = 'Unknown'
$signerName = ''
if ($wsbPath -and (Test-Path -LiteralPath $wsbPath -PathType Leaf)) {
  $version = [string](Get-Item -LiteralPath $wsbPath -ErrorAction SilentlyContinue).VersionInfo.FileVersion
  $signature = Get-AuthenticodeSignature -LiteralPath $wsbPath -ErrorAction SilentlyContinue
  if ($signature) {
    $signatureStatus = [string]$signature.Status
    if ($signature.SignerCertificate) {
      $signerName = [string]$signature.SignerCertificate.GetNameInfo(
        [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
    }
  }
}
$privileges = (& whoami.exe /priv 2>$null | Out-String)
$virtualDiskApi = Test-Path -LiteralPath (Join-Path $env:SystemRoot 'System32\virtdisk.dll')
$hypervCmdlets = [bool](Get-Command New-VHD -ErrorAction SilentlyContinue) -and [bool](Get-Command Mount-VHD -ErrorAction SilentlyContinue)
$sandboxPolicy = $null
$policyReadFailed = $false
try {
  $sandboxPolicy = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Sandbox' -ErrorAction Stop
} catch [System.Management.Automation.ItemNotFoundException] {
  $sandboxPolicy = $null
} catch {
  $policyReadFailed = $true
}
$mappedFoldersPolicy = if ($policyReadFailed) { -1 } elseif ($null -eq $sandboxPolicy -or $null -eq $sandboxPolicy.AllowMappedFolders) { $null } else { try { [int]$sandboxPolicy.AllowMappedFolders } catch { -1 } }
$mappedFolderWritePolicy = if ($policyReadFailed) { -1 } elseif ($null -eq $sandboxPolicy -or $null -eq $sandboxPolicy.AllowWriteToMappedFolders) { $null } else { try { [int]$sandboxPolicy.AllowWriteToMappedFolders } catch { -1 } }
[pscustomobject]@{
  edition = [string]$cv.EditionID
  build = [string]$cv.CurrentBuildNumber
  feature = [string]$feature.State
  wsbAvailable = [bool]($wsbPath -and (Test-Path -LiteralPath $wsbPath -PathType Leaf) -and ([IO.Path]::GetFileName($wsbPath) -ieq 'wsb.exe') -and $signatureStatus -eq 'Valid' -and $signerName -ceq 'Microsoft Corporation')
  wsbPath = [string]$wsbPath
  wsbVersion = [string]$version
  virtualizationFirmwareEnabled = if ($null -eq $cpu) { $null } else { [bool]$cpu }
  sandboxProcessDetected = $active
  privileges = [string]$privileges
  virtualDiskApiAvailable = [bool]$virtualDiskApi
  hypervCmdletsAvailable = [bool]$hypervCmdlets
  mappedFoldersPolicy = $mappedFoldersPolicy
  mappedFolderWritePolicy = $mappedFolderWritePolicy
} | ConvertTo-Json -Compress"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    let Ok((status, stdout)) = run_bounded_probe(command) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), "Unknown".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    };
    if !status.success() {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), "Unknown".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&stdout) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into(), false, "Unavailable".into(), "Unknown".into(), None, false, false, false, false, "Unknown".into(), "Unknown".into());
    };
    let string_field = |key: &str| value.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("Unknown").to_string();
    let firmware = value.get("virtualizationFirmwareEnabled").and_then(serde_json::Value::as_bool);
    (
        string_field("edition"),
        string_field("build"),
        string_field("feature"),
        value.get("wsbAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        string_field("wsbPath"),
        string_field("wsbVersion"),
        firmware,
        value.get("sandboxProcessDetected").and_then(serde_json::Value::as_bool).unwrap_or(false),
        value.get("virtualDiskApiAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        privilege_output_has_enabled(&value.get("privileges").and_then(serde_json::Value::as_str).unwrap_or(""), "SeManageVolumePrivilege"),
        value.get("hypervCmdletsAvailable").and_then(serde_json::Value::as_bool).unwrap_or(false),
        sandbox_policy_state(value.get("mappedFoldersPolicy")),
        sandbox_policy_state(value.get("mappedFolderWritePolicy")),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuestResultStorageState {
    Planned, Created, Attached, GptWriteInProgress, Partitioned,
    NtfsFormatInProgress, Formatted, MountInProgress, Mounted, Shared,
    GuestStopped, ShareRevoked, UnmountInProgress, Unmounted,
    Detached, Removed, RecoveryRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuestResultStorageEvent {
    CreateVerified, AttachVerified, BeginGptInitialization, CompleteGptInitialization,
    BeginNtfsFormat, CompleteNtfsFormat, BeginMount, CompleteMount, GrantShare,
    ConfirmGuestStopped, RevokeShare, BeginUnmount, CompleteUnmount,
    DetachVerified, DeleteVerified, PreserveForRecovery,
}

/// Pure lifecycle gate for a future fixed-capacity result volume.
/// This records allowed ordering only; it performs no disk or Sandbox I/O.
fn advance_guest_result_storage_state(
    state: GuestResultStorageState,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageState, String> {
    if event == GuestResultStorageEvent::PreserveForRecovery {
        return if state == GuestResultStorageState::Removed {
            Err("Removed result storage cannot be moved back into recovery.".into())
        } else {
            Ok(GuestResultStorageState::RecoveryRequired)
        };
    }
    if state == GuestResultStorageState::RecoveryRequired {
        return Err("Result storage is preserved for recovery; automatic lifecycle actions are blocked.".into());
    }
    if !ownership_verified {
        return Err("Result-storage ownership could not be verified; the operation was refused.".into());
    }

    match (state, event) {
        (GuestResultStorageState::Planned, GuestResultStorageEvent::CreateVerified) => Ok(GuestResultStorageState::Created),
        (GuestResultStorageState::Created, GuestResultStorageEvent::AttachVerified) => Ok(GuestResultStorageState::Attached),
        (GuestResultStorageState::Attached, GuestResultStorageEvent::BeginGptInitialization) => Ok(GuestResultStorageState::GptWriteInProgress),
        (GuestResultStorageState::GptWriteInProgress, GuestResultStorageEvent::CompleteGptInitialization) => Ok(GuestResultStorageState::Partitioned),
        (GuestResultStorageState::Partitioned, GuestResultStorageEvent::BeginNtfsFormat) => Ok(GuestResultStorageState::NtfsFormatInProgress),
        (GuestResultStorageState::NtfsFormatInProgress, GuestResultStorageEvent::CompleteNtfsFormat) => Ok(GuestResultStorageState::Formatted),
        (GuestResultStorageState::Formatted, GuestResultStorageEvent::BeginMount) => Ok(GuestResultStorageState::MountInProgress),
        (GuestResultStorageState::MountInProgress, GuestResultStorageEvent::CompleteMount) => Ok(GuestResultStorageState::Mounted),
        (GuestResultStorageState::Mounted, GuestResultStorageEvent::GrantShare) => Ok(GuestResultStorageState::Shared),
        (GuestResultStorageState::Shared, GuestResultStorageEvent::ConfirmGuestStopped) if guest_stopped => Ok(GuestResultStorageState::GuestStopped),
        (GuestResultStorageState::GuestStopped, GuestResultStorageEvent::RevokeShare) => Ok(GuestResultStorageState::ShareRevoked),
        (GuestResultStorageState::ShareRevoked, GuestResultStorageEvent::BeginUnmount) => Ok(GuestResultStorageState::UnmountInProgress),
        (GuestResultStorageState::UnmountInProgress, GuestResultStorageEvent::CompleteUnmount) => Ok(GuestResultStorageState::Unmounted),
        (GuestResultStorageState::Unmounted, GuestResultStorageEvent::DetachVerified) => Ok(GuestResultStorageState::Detached),
        (GuestResultStorageState::Detached, GuestResultStorageEvent::DeleteVerified) => Ok(GuestResultStorageState::Removed),
        _ => Err("The result-storage lifecycle transition is out of order or lacks required evidence.".into()),
    }
}


const GUEST_RESULT_STORAGE_JOURNAL_SCHEMA: u32 = 1;
const MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES: usize = 16 * 1024;
const MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultStorageJournalEntry {
    sequence: u32,
    previous_state: GuestResultStorageState,
    event: GuestResultStorageEvent,
    next_state: GuestResultStorageState,
    ownership_verified: bool,
    guest_stopped: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultStorageJournal {
    schema_version: u32,
    run_id: String,
    vhdx_identifier: String,
    entries: Vec<GuestResultStorageJournalEntry>,
}

fn validate_guest_result_storage_journal(
    journal: &GuestResultStorageJournal,
) -> Result<GuestResultStorageState, String> {
    if journal.schema_version != GUEST_RESULT_STORAGE_JOURNAL_SCHEMA
        || !valid_snapshot_id(&journal.run_id)
        || !valid_guest_vhdx_identifier(&journal.vhdx_identifier)
        || journal.entries.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES
    {
        return Err("The result-storage recovery journal header or bounds are invalid.".into());
    }
    let mut state = GuestResultStorageState::Planned;
    for (index, entry) in journal.entries.iter().enumerate() {
        if entry.sequence != index as u32 + 1 || entry.previous_state != state {
            return Err("The result-storage recovery journal is out of sequence or has a broken state chain.".into());
        }
        state = advance_guest_result_storage_state(
            state, entry.event, entry.ownership_verified, entry.guest_stopped,
        )?;
        if entry.next_state != state {
            return Err("The result-storage recovery journal contains an invalid transition result.".into());
        }
    }
    Ok(state)
}

fn encode_guest_result_storage_journal(
    journal: &GuestResultStorageJournal,
) -> Result<Vec<u8>, String> {
    validate_guest_result_storage_journal(journal)?;
    let bytes = serde_json::to_vec(journal)
        .map_err(|_| "The result-storage recovery journal could not be encoded.".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES {
        return Err("The result-storage recovery journal exceeds its fixed size limit.".into());
    }
    Ok(bytes)
}

fn parse_guest_result_storage_journal(bytes: &[u8]) -> Result<GuestResultStorageJournal, String> {
    if bytes.is_empty() || bytes.len() > MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES {
        return Err("The result-storage recovery journal is empty or oversized.".into());
    }
    let journal: GuestResultStorageJournal = serde_json::from_slice(bytes)
        .map_err(|_| "The result-storage recovery journal is malformed or contains unsupported fields.".to_string())?;
    if journal.entries.is_empty() {
        return Err("The result-storage recovery journal contains no recorded transitions.".into());
    }
    validate_guest_result_storage_journal(&journal)?;
    Ok(journal)
}

fn append_guest_result_storage_journal_entry(
    journal: &GuestResultStorageJournal,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageJournal, String> {
    let state = validate_guest_result_storage_journal(journal)?;
    if journal.entries.len() >= MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("The result-storage recovery journal reached its fixed transition limit.".into());
    }
    let next_state = advance_guest_result_storage_state(
        state, event, ownership_verified, guest_stopped,
    )?;
    let mut next = journal.clone();
    next.entries.push(GuestResultStorageJournalEntry {
        sequence: next.entries.len() as u32 + 1,
        previous_state: state,
        event,
        next_state,
        ownership_verified,
        guest_stopped,
    });
    encode_guest_result_storage_journal(&next)?;
    Ok(next)
}

const GUEST_RESULT_STORAGE_JOURNAL_DIR_PREFIX: &str = ".friday-result-journal-";
const GUEST_RESULT_STORAGE_JOURNAL_FILE: &str = "state.json";

fn guest_result_storage_journal_generation_path(root: &Path, generation: usize) -> PathBuf {
    root.join(format!("{GUEST_RESULT_STORAGE_JOURNAL_DIR_PREFIX}{generation:02}"))
}

fn read_guest_result_storage_journal_generation(
    root: &Path,
    generation: usize,
) -> Result<GuestResultStorageJournal, String> {
    if generation == 0 || generation > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("The result-storage journal generation is outside its fixed limit.".into());
    }
    let directory = guest_result_storage_journal_generation_path(root, generation);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|_| "A result-storage journal generation is missing or unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("A result-storage journal generation is not an ordinary directory.".into());
    }
    let bytes = crate::coding_sandbox_fs::read_single_bounded_file_from_directory(
        &directory, OsStr::new(GUEST_RESULT_STORAGE_JOURNAL_FILE),
        MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES as u64,
    ).map_err(|_| "A result-storage journal generation is incomplete or unsafe.".to_string())?;
    let journal = parse_guest_result_storage_journal(&bytes)?;
    if journal.entries.len() != generation {
        return Err("A result-storage journal generation does not match its sequence.".into());
    }
    Ok(journal)
}

/// Persist an immutable, create-new journal generation. Each generation is a
/// separate private directory containing exactly one bounded, flushed file.
/// Existing generations are never overwritten; an interrupted generation is
/// retained and forces manual recovery instead of being silently repaired.
fn persist_guest_result_storage_journal(
    root: &Path,
    journal: &GuestResultStorageJournal,
) -> Result<(), String> {
    let state = validate_guest_result_storage_journal(journal)?;
    let generation = journal.entries.len();
    if generation == 0 || generation > MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        return Err("Only a non-empty bounded result-storage journal can be persisted.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The result-storage journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-storage journal root must be an ordinary directory.".into());
    }
    if generation > 1 {
        let previous = read_guest_result_storage_journal_generation(root, generation - 1)?;
        if previous.run_id != journal.run_id
            || previous.vhdx_identifier != journal.vhdx_identifier
            || previous.entries != journal.entries[..generation - 1]
        {
            return Err("The next result-storage journal generation does not extend the persisted history.".into());
        }
    } else {
        let first = guest_result_storage_journal_generation_path(root, 1);
        if fs::symlink_metadata(first).is_ok() {
            return Err("The initial result-storage journal generation already exists.".into());
        }
    }
    let directory = guest_result_storage_journal_generation_path(root, generation);
    fs::create_dir(&directory)
        .map_err(|_| "FRIDAY could not exclusively create the next result-storage journal generation.".to_string())?;
    secure_private_workspace_directory(&directory)
        .map_err(|_| "FRIDAY could not secure the new journal generation; it was preserved for recovery.".to_string())?;
    let bytes = encode_guest_result_storage_journal(journal)?;
    let file_path = directory.join(GUEST_RESULT_STORAGE_JOURNAL_FILE);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&file_path)
        .map_err(|_| "FRIDAY could not exclusively create the journal generation file; it was preserved.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write the journal generation; it was preserved.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not flush the journal generation; it was preserved.".to_string())?;
    drop(file);
    let verified = read_guest_result_storage_journal_generation(root, generation)?;
    if verified != *journal || validate_guest_result_storage_journal(&verified)? != state {
        return Err("The persisted result-storage journal did not pass read-back verification.".into());
    }
    Ok(())
}

/// Load the longest contiguous sequence of immutable journal generations.
/// Gaps, malformed files, unexpected generation directories, and identity
/// changes fail closed; callers must preserve the VHDX for manual recovery.
fn load_guest_result_storage_journal(
    root: &Path,
    expected_run_id: &str,
    expected_vhdx_identifier: &str,
) -> Result<GuestResultStorageJournal, String> {
    if !valid_snapshot_id(expected_run_id) || !valid_guest_vhdx_identifier(expected_vhdx_identifier) {
        return Err("The expected result-storage journal identity is invalid.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The result-storage journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-storage journal root must be an ordinary directory.".into());
    }
    let mut latest: Option<GuestResultStorageJournal> = None;
    let mut gap_seen = false;
    for generation in 1..=MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
        let path = guest_result_storage_journal_generation_path(root, generation);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gap_seen = true;
            }
            Err(_) => return Err("A result-storage journal generation could not be inspected.".into()),
            Ok(_) if gap_seen => return Err("The result-storage journal contains a generation gap.".into()),
            Ok(_) => {
                let current = read_guest_result_storage_journal_generation(root, generation)?;
                if current.run_id != expected_run_id || current.vhdx_identifier != expected_vhdx_identifier {
                    return Err("The result-storage journal belongs to a different run or VHDX.".into());
                }
                if let Some(previous) = &latest {
                    if current.entries[..generation - 1] != previous.entries {
                        return Err("The result-storage journal history is not append-only.".into());
                    }
                }
                latest = Some(current);
            }
        }
    }
    latest.ok_or_else(|| "No complete result-storage journal generation is available.".into())
}




/// Append one verified lifecycle transition through the durable journal boundary.
/// A non-empty caller snapshot must exactly match the latest persisted generation;
/// competing writers are serialized by exclusive generation-directory creation.
fn append_and_persist_guest_result_storage_transition(
    root: &Path,
    current: &GuestResultStorageJournal,
    event: GuestResultStorageEvent,
    ownership_verified: bool,
    guest_stopped: bool,
) -> Result<GuestResultStorageJournal, String> {
    validate_guest_result_storage_journal(current)?;
    if current.entries.is_empty() {
        for generation in 1..=MAX_GUEST_RESULT_STORAGE_JOURNAL_ENTRIES {
            let path = guest_result_storage_journal_generation_path(root, generation);
            match fs::symlink_metadata(path) {
                Ok(_) => return Err("An initial result-storage journal cannot be created over existing recovery evidence.".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Existing result-storage journal evidence could not be inspected.".into()),
            }
        }
    } else {
        let latest = load_guest_result_storage_journal(
            root, &current.run_id, &current.vhdx_identifier,
        )?;
        if latest != *current {
            return Err("The supplied result-storage journal is stale or differs from persisted recovery history.".into());
        }
    }

    let next = append_guest_result_storage_journal_entry(
        current, event, ownership_verified, guest_stopped,
    )?;
    persist_guest_result_storage_journal(root, &next)?;
    let verified = load_guest_result_storage_journal(
        root, &next.run_id, &next.vhdx_identifier,
    )?;
    if verified != next {
        return Err("The newly appended result-storage transition failed full recovery verification.".into());
    }
    Ok(next)
}

const GUEST_RESULT_VOLUME_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;
const GUEST_RESULT_VOLUME_GUEST_MOUNT: &str = r"C:\FRIDAY\Results";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultVolumePlan {
    run_id: String,
    storage_root: PathBuf,
    run_directory: PathBuf,
    vhdx_path: PathBuf,
    host_mount_directory: PathBuf,
    guest_mount_path: String,
    capacity_bytes: u64,
    max_result_bytes: u64,
}

/// Builds an immutable, policy-owned plan for one bounded result volume.
/// This does not create, attach, format, mount, or share a VHDX.
fn build_guest_result_volume_plan(
    storage_root: &Path,
    run_id: &str,
) -> Result<GuestResultVolumePlan, String> {
    if !valid_snapshot_id(run_id) {
        return Err("The result-volume run identifier is invalid.".into());
    }
    if !storage_root.is_absolute() {
        return Err("The result-volume storage root must be absolute.".into());
    }
    let root_metadata = fs::symlink_metadata(storage_root)
        .map_err(|_| "The private result-volume storage root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-volume storage root must be an ordinary directory.".into());
    }
    let canonical_root = fs::canonicalize(storage_root)
        .map_err(|_| "The private result-volume storage root could not be verified.".to_string())?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root)
        .map_err(|_| "The canonical result-volume storage root could not be verified.".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_or_symlink(&canonical_metadata) {
        return Err("The canonical result-volume storage root is not an ordinary directory.".into());
    }

    let run_directory = canonical_root.join(run_id);
    match fs::symlink_metadata(&run_directory) {
        Ok(_) => return Err("A result-volume run directory already exists; reuse is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The result-volume run directory could not be safely inspected.".into()),
    }
    let vhdx_path = run_directory.join("result.vhdx");
    if !vhdx_path.starts_with(&canonical_root)
        || run_directory.file_name().and_then(OsStr::to_str) != Some(run_id)
    {
        return Err("The result-volume path failed its containment check.".into());
    }

    Ok(GuestResultVolumePlan {
        run_id: run_id.to_string(),
        storage_root: canonical_root,
        host_mount_directory: run_directory.join("result-mount"),
        run_directory,
        vhdx_path,
        guest_mount_path: GUEST_RESULT_VOLUME_GUEST_MOUNT.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        max_result_bytes: MAX_GUEST_RESULT_BYTES as u64,
    })
}

/// Re-derives the plan from trusted host inputs and compares every field.
/// A guest request can never choose a path, mount point, capacity, or result cap.
fn validate_guest_result_volume_plan(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_run_id: &str,
) -> Result<(), String> {
    if !valid_snapshot_id(expected_run_id) {
        return Err("The result-volume run identifier is invalid.".into());
    }
    if !trusted_storage_root.is_absolute() {
        return Err("The result-volume storage root must be absolute.".into());
    }
    let root_metadata = fs::symlink_metadata(trusted_storage_root)
        .map_err(|_| "The private result-volume storage root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The result-volume storage root must be an ordinary directory.".into());
    }
    let canonical_root = fs::canonicalize(trusted_storage_root)
        .map_err(|_| "The private result-volume storage root could not be verified.".to_string())?;
    let canonical_metadata = fs::symlink_metadata(&canonical_root)
        .map_err(|_| "The canonical result-volume storage root could not be verified.".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_or_symlink(&canonical_metadata) {
        return Err("The canonical result-volume storage root is not an ordinary directory.".into());
    }

    // Validation intentionally does not require the run directory to be absent:
    // reservation creates it before ownership evidence can be verified.
    let expected_run_directory = canonical_root.join(expected_run_id);
    let expected = GuestResultVolumePlan {
        run_id: expected_run_id.to_string(),
        storage_root: canonical_root,
        run_directory: expected_run_directory.clone(),
        vhdx_path: expected_run_directory.join("result.vhdx"),
        host_mount_directory: expected_run_directory.join("result-mount"),
        guest_mount_path: GUEST_RESULT_VOLUME_GUEST_MOUNT.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        max_result_bytes: MAX_GUEST_RESULT_BYTES as u64,
    };
    if plan != &expected {
        return Err("The result-volume plan differs from FRIDAY's fixed per-run policy.".into());
    }
    if plan.capacity_bytes < plan.max_result_bytes
        || plan.max_result_bytes != MAX_GUEST_RESULT_BYTES as u64
        || plan.guest_mount_path != GUEST_RESULT_VOLUME_GUEST_MOUNT
        || plan.host_mount_directory != plan.run_directory.join("result-mount")
    {
        return Err("The result-volume capacity or guest mount point violates FRIDAY's fixed policy.".into());
    }
    Ok(())
}


const GUEST_RESULT_VOLUME_OWNER_MARKER: &str = ".friday-result-volume-owner.json";
const GUEST_RESULT_VOLUME_OWNER_SCHEMA: u32 = 1;

const GUEST_RESULT_VHDX_OWNER_MARKER: &str = ".friday-result-vhdx-owner.json";
const GUEST_RESULT_VHDX_OWNER_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultVhdxOwnershipMarker {
    schema_version: u32,
    run_id: String,
    owner: String,
    identifier: String,
    capacity_bytes: u64,
}

fn validate_guest_result_vhdx_ownership_marker(
    marker: &GuestResultVhdxOwnershipMarker,
    expected_run_id: &str,
    expected_identifier: Option<&str>,
) -> Result<(), String> {
    if !valid_snapshot_id(expected_run_id)
        || marker.schema_version != GUEST_RESULT_VHDX_OWNER_SCHEMA
        || marker.run_id != expected_run_id
        || marker.owner != "FRIDAY-CodingAssistant-ResultVhdx"
        || marker.capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || !valid_guest_vhdx_identifier(&marker.identifier)
        || expected_identifier.is_some_and(|expected| expected != marker.identifier)
    {
        return Err("The result VHDX ownership record does not match FRIDAY's exact run and disk identity.".into());
    }
    Ok(())
}

/// Persists the VHDX's stable on-disk identifier in a separate exclusive marker.
/// This marker is evidence for later verification, not authorization to attach,
/// mount, detach, or delete the virtual disk.
fn write_guest_result_vhdx_ownership_marker(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    identifier: &str,
) -> Result<(), String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if !valid_guest_vhdx_identifier(identifier) {
        return Err("The result VHDX identifier is invalid; ownership was not recorded.".into());
    }
    let marker = GuestResultVhdxOwnershipMarker {
        schema_version: GUEST_RESULT_VHDX_OWNER_SCHEMA,
        run_id: plan.run_id.clone(),
        owner: "FRIDAY-CodingAssistant-ResultVhdx".to_string(),
        identifier: identifier.to_string(),
        capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare VHDX ownership metadata.".to_string())?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not exclusively create VHDX ownership metadata.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write VHDX ownership metadata.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist VHDX ownership metadata.".to_string())?;
    verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, Some(identifier))?;
    Ok(())
}

fn verify_guest_result_vhdx_ownership_marker(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_identifier: Option<&str>,
) -> Result<String, String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER);
    let metadata = fs::symlink_metadata(&marker_path)
        .map_err(|_| "VHDX ownership evidence is missing; the resource was preserved.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() == 0 || metadata.len() > 1024 {
        return Err("VHDX ownership evidence is not a bounded ordinary file.".into());
    }
    let mut file = open_source_file(&marker_path)
        .map_err(|_| "VHDX ownership evidence could not be opened without following links.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "VHDX ownership evidence could not be verified.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() || opened.len() > 1024 {
        return Err("VHDX ownership evidence changed while being opened.".into());
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "VHDX ownership evidence could not be read.".to_string())?;
    if bytes.is_empty() || bytes.len() > 1024 {
        return Err("VHDX ownership evidence exceeded its read limit.".into());
    }
    let marker: GuestResultVhdxOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "VHDX ownership evidence is malformed or contains unsupported fields.".to_string())?;
    validate_guest_result_vhdx_ownership_marker(&marker, &plan.run_id, expected_identifier)?;
    Ok(marker.identifier)
}




/// Verifies that the result VHDX is the exact ordinary file directly inside
/// FRIDAY's verified per-run directory. This is a read-only path/handle check;
/// it does not authorize attachment, mounting, sharing, or deletion.
fn verify_guest_result_vhdx_file_path(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    let verified_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if plan.vhdx_path.parent() != Some(plan.run_directory.as_path())
        || plan.vhdx_path.file_name() != Some(OsStr::new("result.vhdx"))
    {
        return Err("The result VHDX path is not FRIDAY's exact per-run disk path.".into());
    }
    let metadata = fs::symlink_metadata(&plan.vhdx_path)
        .map_err(|_| "The result VHDX is unavailable; it was preserved for recovery.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() < 8 {
        return Err("The result VHDX is not an ordinary non-empty file.".into());
    }
    let canonical = fs::canonicalize(&plan.vhdx_path)
        .map_err(|_| "The result VHDX path could not be verified.".to_string())?;
    if canonical.parent() != Some(verified_directory.as_path())
        || canonical.file_name() != Some(OsStr::new("result.vhdx"))
    {
        return Err("The result VHDX is not the exact direct child of its owned run directory.".into());
    }
    let mut file = open_source_file(&plan.vhdx_path)
        .map_err(|_| "The result VHDX could not be opened without following links.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "The opened result VHDX handle could not be verified.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() {
        return Err("The result VHDX changed while its file handle was opened.".into());
    }
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature)
        .map_err(|_| "The result VHDX header could not be read.".to_string())?;
    if &signature != b"vhdxfile" {
        return Err("The result file does not contain the VHDX signature.".into());
    }
    Ok(canonical)
}

#[cfg(windows)]
fn verify_guest_result_vhdx_identity(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<String, String> {
    let recorded_identifier =
        verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, None)?;
    let verified_path = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    let observed_identifier =
        verify_guest_result_vhdx_capacity(&verified_path, GUEST_RESULT_VOLUME_CAPACITY_BYTES)?;
    // Recheck the directory and file after the path-based Virtual Disk API query.
    // A changed path or replaced file is never accepted as ownership evidence.
    let verified_again = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    if verified_again != verified_path || observed_identifier != recorded_identifier {
        return Err("The result VHDX identity changed or does not match its FRIDAY ownership record.".into());
    }
    Ok(observed_identifier)
}

#[cfg(windows)]
const FRIDAY_GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID: u32 = 14;

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayWindowsGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayVirtualStorageType {
    device_id: u32,
    vendor_id: FridayWindowsGuid,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayCreateVirtualDiskVersion2 {
    unique_id: FridayWindowsGuid,
    maximum_size: u64,
    block_size_in_bytes: u32,
    sector_size_in_bytes: u32,
    physical_sector_size_in_bytes: u32,
    parent_path: *const u16,
    source_path: *const u16,
    open_flags: u32,
    parent_virtual_storage_type: FridayVirtualStorageType,
    source_virtual_storage_type: FridayVirtualStorageType,
    resiliency_guid: FridayWindowsGuid,
}

#[cfg(windows)]
#[repr(C)]
union FridayCreateVirtualDiskParametersUnion {
    version2: std::mem::ManuallyDrop<FridayCreateVirtualDiskVersion2>,
    alignment: [u64; 16],
}

#[cfg(windows)]
#[repr(C)]
struct FridayCreateVirtualDiskParameters {
    version: u32,
    parameters: FridayCreateVirtualDiskParametersUnion,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayVirtualDiskSizeInfo {
    virtual_size: u64,
    physical_size: u64,
    block_size: u32,
    sector_size: u32,
}

#[cfg(windows)]
#[repr(C)]
union FridayGetVirtualDiskInfoData {
    size: FridayVirtualDiskSizeInfo,
    identifier: FridayWindowsGuid,
    provider_subtype: u32,
    alignment: [u64; 3],
}

#[cfg(windows)]
#[repr(C)]
struct FridayGetVirtualDiskInfo {
    version: u32,
    data: FridayGetVirtualDiskInfoData,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayOpenVirtualDiskVersion1 {
    read_write_depth: u32,
}

#[cfg(windows)]
#[repr(C)]
union FridayOpenVirtualDiskParametersData {
    version1: FridayOpenVirtualDiskVersion1,
    alignment: [u64; 3],
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct FridayAttachVirtualDiskVersion1 {
    reserved: u32,
}

#[cfg(windows)]
#[repr(C)]
struct FridayAttachVirtualDiskParameters {
    version: u32,
    version1: FridayAttachVirtualDiskVersion1,
}

#[cfg(windows)]
#[repr(C)]
struct FridayOpenVirtualDiskParameters {
    version: u32,
    parameters: FridayOpenVirtualDiskParametersData,
}

#[cfg(windows)]
#[link(name = "virtdisk")]
extern "system" {
    fn OpenVirtualDisk(
        virtual_storage_type: *const FridayVirtualStorageType,
        path: *const u16,
        virtual_disk_access_mask: u32,
        flags: u32,
        parameters: *const FridayOpenVirtualDiskParameters,
        handle: *mut *mut std::ffi::c_void,
    ) -> u32;
    fn AttachVirtualDisk(
        virtual_disk_handle: *mut std::ffi::c_void,
        security_descriptor: *const std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *const FridayAttachVirtualDiskParameters,
        overlapped: *mut std::ffi::c_void,
    ) -> u32;
    fn DetachVirtualDisk(
        virtual_disk_handle: *mut std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
    ) -> u32;
    fn GetVirtualDiskPhysicalPath(
        virtual_disk_handle: *mut std::ffi::c_void,
        disk_path_size_in_bytes: *mut u32,
        disk_path: *mut u16,
    ) -> u32;
    fn GetVirtualDiskInformation(
        virtual_disk_handle: *mut std::ffi::c_void,
        virtual_disk_info_size: *mut u32,
        virtual_disk_info: *mut FridayGetVirtualDiskInfo,
        size_used: *mut u32,
    ) -> u32;
    fn CreateVirtualDisk(
        virtual_storage_type: *const FridayVirtualStorageType,
        path: *const u16,
        virtual_disk_access_mask: u32,
        security_descriptor: *const std::ffi::c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *const FridayCreateVirtualDiskParameters,
        overlapped: *mut std::ffi::c_void,
        handle: *mut *mut std::ffi::c_void,
    ) -> u32;
}

#[cfg(windows)]
#[repr(C)]
struct FridayFileTime { low: u32, high: u32 }

#[cfg(windows)]
struct OwnedObservedProcessHandle(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for OwnedObservedProcessHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0); }
            self.0 = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut std::ffi::c_void;
    fn GetProcessTimes(process: *mut std::ffi::c_void, creation_time: *mut FridayFileTime, exit_time: *mut FridayFileTime, kernel_time: *mut FridayFileTime, user_time: *mut FridayFileTime) -> i32;
    fn QueryFullProcessImageNameW(process: *mut std::ffi::c_void, flags: u32, image_name: *mut u16, size: *mut u32) -> i32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn SetVolumeMountPointW(volume_mount_point: *const u16, volume_name: *const u16) -> i32;
    fn GetVolumeNameForVolumeMountPointW(volume_mount_point: *const u16, volume_name: *mut u16, buffer_length: u32) -> i32;
    fn DeleteVolumeMountPointW(volume_mount_point: *const u16) -> i32;
}

#[cfg(windows)]
struct OwnedVirtualDiskHandle(*mut std::ffi::c_void);

#[cfg(windows)]
impl OwnedVirtualDiskHandle {
    fn close(mut self) -> Result<(), String> {
        let handle = std::mem::replace(&mut self.0, std::ptr::null_mut());
        if handle.is_null() || unsafe { CloseHandle(handle) } == 0 {
            return Err("Windows could not close the newly created virtual-disk handle; the resource was preserved for recovery.".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for OwnedVirtualDiskHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0); }
            self.0 = std::ptr::null_mut();
        }
    }
}


/// Owns a single FRIDAY-created VHDX attachment. The handle is deliberately
/// non-permanent: process termination closes it and Windows releases the attach.
#[cfg(windows)]
struct OwnedAttachedGuestVhdx {
    handle: OwnedVirtualDiskHandle,
    physical_path: String,
}

#[cfg(windows)]
impl OwnedAttachedGuestVhdx {
    fn physical_path(&self) -> &str {
        &self.physical_path
    }

    fn detach(self) -> Result<(), String> {
        let status = unsafe { DetachVirtualDisk(self.handle.0, 0, 0) };
        if status != 0 {
            return Err(format!("Windows could not detach FRIDAY's owned result VHDX (error {status}); the attachment handle will be closed for automatic cleanup."));
        }
        if guest_vhdx_physical_path(self.handle.0).is_ok() {
            return Err("Windows reported detachment but the result VHDX still has an attached physical-device path.".into());
        }
        self.handle.close()
    }
}

#[cfg(windows)]
fn guest_vhdx_physical_path(handle: *mut std::ffi::c_void) -> Result<String, String> {
    let mut buffer = [0u16; 1024];
    let mut size_bytes = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe { GetVirtualDiskPhysicalPath(handle, &mut size_bytes, buffer.as_mut_ptr()) };
    if status != 0 {
        return Err(format!("Windows could not confirm the attached result VHDX device path (error {status})."));
    }
    if size_bytes < 2 || size_bytes as usize > buffer.len() * 2 || size_bytes % 2 != 0 {
        return Err("Windows returned an invalid result VHDX device-path length.".into());
    }
    let end = buffer.iter().position(|unit| *unit == 0)
        .ok_or_else(|| "Windows returned an unterminated result VHDX device path.".to_string())?;
    let path = String::from_utf16(&buffer[..end])
        .map_err(|_| "Windows returned a malformed result VHDX device path.".to_string())?;
    let prefix = r"\\.\PhysicalDrive";
    if !path.starts_with(prefix)
        || path[prefix.len()..].is_empty()
        || !path[prefix.len()..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Windows returned a physical-device path outside the expected disk namespace.".into());
    }
    Ok(path)
}

/// Attaches only the exact FRIDAY-owned fixed VHDX, read/write, without drive
/// letters or permanent lifetime. No partition is created or mounted here.
#[cfg(windows)]
fn attach_guest_result_vhdx(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<OwnedAttachedGuestVhdx, String> {
    let expected_identifier = verify_guest_result_vhdx_identity(plan, trusted_storage_root)?;
    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = plan.vhdx_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid for attachment.".into());
    }
    let storage_type = FridayVirtualStorageType {
        device_id: 3,
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let open_parameters = FridayOpenVirtualDiskParameters {
        version: 1,
        parameters: FridayOpenVirtualDiskParametersData {
            version1: FridayOpenVirtualDiskVersion1 { read_write_depth: 1 },
        },
    };
    let mut raw_handle = std::ptr::null_mut();
    // ATTACH_RW | DETACH | GET_INFO. The handle is scoped to this owner.
    let open_status = unsafe {
        OpenVirtualDisk(&storage_type, path_wide.as_ptr(), 0x0002_0000 | 0x0004_0000 | 0x0008_0000,
            0, &open_parameters, &mut raw_handle)
    };
    if open_status != 0 || raw_handle.is_null() {
        return Err(format!("Windows could not open the verified result VHDX for controlled attachment (error {open_status})."));
    }
    let handle = OwnedVirtualDiskHandle(raw_handle);
    let parameters = FridayAttachVirtualDiskParameters {
        version: 1,
        version1: FridayAttachVirtualDiskVersion1 { reserved: 0 },
    };
    // NO_DRIVE_LETTER. PERMANENT_LIFETIME is intentionally never set.
    let attach_status = unsafe {
        AttachVirtualDisk(handle.0, std::ptr::null(), 0x2, 0, &parameters, std::ptr::null_mut())
    };
    if attach_status != 0 {
        return Err(format!("Windows refused to attach the private result VHDX (error {attach_status}); no volume was exposed."));
    }
    let physical_path = guest_vhdx_physical_path(handle.0)
        .map_err(|error| format!("{error} The attachment handle will be closed for automatic cleanup."))?;
    let observed_identifier = verify_attached_guest_result_vhdx_identity(
        &handle, &physical_path, plan, trusted_storage_root,
    ).map_err(|error| format!("The attached result VHDX failed identity revalidation ({error}); the attachment handle will be closed."))?;
    if observed_identifier != expected_identifier {
        return Err("The result VHDX identity changed during attachment; the attachment handle will be closed.".into());
    }
    Ok(OwnedAttachedGuestVhdx { handle, physical_path })
}

/// Revalidates an attached disk through the exact FRIDAY-owned handle rather
/// than reopening its backing path while Windows has it attached.
#[cfg(windows)]
fn verify_attached_guest_result_vhdx_identity(
    handle: &OwnedVirtualDiskHandle,
    expected_physical_path: &str,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<String, String> {
    let recorded_identifier =
        verify_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, None)?;
    let verified_path = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    let physical_path = guest_vhdx_physical_path(handle.0)?;
    if physical_path != expected_physical_path {
        return Err("The attached physical-device path changed during identity verification.".into());
    }
    let mut identifier_info = FridayGetVirtualDiskInfo {
        version: FRIDAY_GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID, // VHDX GET_VIRTUAL_DISK_INFO_VIRTUAL_DISK_ID
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut identifier_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut identifier_bytes, &mut identifier_info, std::ptr::null_mut())
    };
    if status != 0 {
        return Err(format!("Windows could not read the persistent identifier from FRIDAY's attached VHDX handle (error {status})."));
    }
    let observed_identifier = format_guest_vhdx_identifier(unsafe { identifier_info.data.identifier })?;
    let verified_again = verify_guest_result_vhdx_file_path(plan, trusted_storage_root)?;
    if verified_again != verified_path {
        return Err("The result VHDX backing-file path changed during attached identity verification.".into());
    }
    if observed_identifier != recorded_identifier {
        return Err(format!(
            "The attached VHDX persistent identifier does not match its FRIDAY ownership record (recorded={}, observed={}).",
            recorded_identifier, observed_identifier
        ));
    }
    Ok(observed_identifier)
}

#[cfg(windows)]
fn query_guest_result_vhdx_info(path: &Path) -> Result<(FridayVirtualDiskSizeInfo, u32, String), String> {
    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid for metadata verification.".into());
    }
    let storage_type = FridayVirtualStorageType {
        device_id: 3,
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let open_parameters = FridayOpenVirtualDiskParameters {
        version: 1,
        parameters: FridayOpenVirtualDiskParametersData {
            version1: FridayOpenVirtualDiskVersion1 { read_write_depth: 0 },
        },
    };
    let mut raw_handle: *mut std::ffi::c_void = std::ptr::null_mut();
    // GET_INFO only: this handle cannot attach, modify, or write the disk.
    let open_status = unsafe {
        OpenVirtualDisk(&storage_type, path_wide.as_ptr(), 0x0008_0000, 0, &open_parameters, &mut raw_handle)
    };
    if open_status != 0 || raw_handle.is_null() {
        return Err(format!("Windows could not open the result VHDX for read-only metadata verification (error {open_status})."));
    }
    let handle = OwnedVirtualDiskHandle(raw_handle);

    let mut size_info = FridayGetVirtualDiskInfo {
        version: 1, // GET_VIRTUAL_DISK_INFO_SIZE
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut size_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let size_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut size_bytes, &mut size_info, std::ptr::null_mut())
    };
    if size_status != 0 {
        return Err(format!("Windows could not read the result VHDX capacity (error {size_status})."));
    }
    let size = unsafe { size_info.data.size };

    let mut subtype_info = FridayGetVirtualDiskInfo {
        version: 7, // GET_VIRTUAL_DISK_INFO_PROVIDER_SUBTYPE
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut subtype_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let subtype_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut subtype_bytes, &mut subtype_info, std::ptr::null_mut())
    };
    if subtype_status != 0 {
        return Err(format!("Windows could not verify the result VHDX allocation type (error {subtype_status})."));
    }
    let subtype = unsafe { subtype_info.data.provider_subtype };

    let mut identifier_info = FridayGetVirtualDiskInfo {
        version: 2, // GET_VIRTUAL_DISK_INFO_IDENTIFIER
        data: FridayGetVirtualDiskInfoData { alignment: [0; 3] },
    };
    let mut identifier_bytes = std::mem::size_of::<FridayGetVirtualDiskInfo>() as u32;
    let identifier_status = unsafe {
        GetVirtualDiskInformation(handle.0, &mut identifier_bytes, &mut identifier_info, std::ptr::null_mut())
    };
    if identifier_status != 0 {
        return Err(format!("Windows could not read the persistent result VHDX identifier (error {identifier_status})."));
    }
    let identifier = unsafe { identifier_info.data.identifier };
    let identifier = format_guest_vhdx_identifier(identifier)?;
    handle.close()?;
    Ok((size, subtype, identifier))
}

fn valid_guest_vhdx_identifier(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23].iter().all(|index| value.as_bytes().get(*index) == Some(&b'-'))
        && value.bytes().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit()
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
        && value == value.to_ascii_lowercase()
}

#[cfg(windows)]
fn format_guest_vhdx_identifier(value: FridayWindowsGuid) -> Result<String, String> {
    if value.data1 == 0 && value.data2 == 0 && value.data3 == 0 && value.data4.iter().all(|byte| *byte == 0) {
        return Err("Windows returned an empty result VHDX identifier.".into());
    }
    let formatted = format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        value.data1, value.data2, value.data3,
        value.data4[0], value.data4[1], value.data4[2], value.data4[3],
        value.data4[4], value.data4[5], value.data4[6], value.data4[7]
    );
    if !valid_guest_vhdx_identifier(&formatted) {
        return Err("Windows returned a malformed result VHDX identifier.".into());
    }
    Ok(formatted)
}

#[cfg(windows)]
fn validate_guest_result_vhdx_readback(
    size: FridayVirtualDiskSizeInfo,
    provider_subtype: u32,
    identifier: &str,
    expected_capacity: u64,
) -> Result<(), String> {
    if expected_capacity != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || size.virtual_size != expected_capacity
        || size.physical_size < expected_capacity
        || size.block_size != 0 // Windows reports the default block-size sentinel for fixed VHDX disks
        || size.sector_size != 512
        || provider_subtype != 2 // VIRTUAL_DISK_TYPE_FIXED
        || !valid_guest_vhdx_identifier(identifier)
    {
        return Err(format!(
            "The result VHDX metadata does not match FRIDAY's fixed-capacity disk policy (expected_capacity={}, virtual_size={}, physical_size={}, block_size={}, sector_size={}, provider_subtype={}, identifier_valid={}).",
            expected_capacity, size.virtual_size, size.physical_size, size.block_size, size.sector_size, provider_subtype, valid_guest_vhdx_identifier(identifier)
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_guest_result_vhdx_capacity(path: &Path, expected_capacity: u64) -> Result<String, String> {
    let (size, provider_subtype, identifier) = query_guest_result_vhdx_info(path)?;
    validate_guest_result_vhdx_readback(size, provider_subtype, &identifier, expected_capacity)?;
    Ok(identifier)
}

#[cfg(windows)]
fn create_guest_result_vhdx(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let verified_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if verified_directory != plan.run_directory {
        return Err("The result-volume directory identity changed; VHDX creation was refused.".into());
    }
    match fs::symlink_metadata(&plan.vhdx_path) {
        Ok(_) => return Err("A result VHDX already exists; replacement is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The result VHDX path could not be safely inspected.".into()),
    }
    if plan.capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || plan.capacity_bytes < 3 * 1024 * 1024
        || plan.capacity_bytes % 512 != 0
    {
        return Err("The result VHDX capacity is outside FRIDAY's fixed creation policy.".into());
    }

    use std::os::windows::ffi::OsStrExt;
    let path_wide: Vec<u16> = plan.vhdx_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if path_wide.len() <= 1 {
        return Err("The result VHDX path is invalid.".into());
    }

    // Microsoft VHDX provider. Version 2 is required for the VHDX 1 MiB
    // block-size and 4 KiB physical-sector settings. A zero GUID requests a
    // system-generated unique disk identifier; no parent/source disk is used.
    let storage_type = FridayVirtualStorageType {
        device_id: 3, // VIRTUAL_STORAGE_TYPE_DEVICE_VHDX
        vendor_id: FridayWindowsGuid {
            data1: 0xec984aec,
            data2: 0xa0f9,
            data3: 0x47e9,
            data4: [0x90, 0x1f, 0x71, 0x41, 0x5a, 0x66, 0x34, 0x5b],
        },
    };
    let zero_guid = FridayWindowsGuid {
        data1: 0, data2: 0, data3: 0, data4: [0; 8],
    };
    let version2 = FridayCreateVirtualDiskVersion2 {
        unique_id: zero_guid,
        maximum_size: plan.capacity_bytes,
        block_size_in_bytes: 0, // Fixed VHDX uses the Windows default block-size sentinel.
        sector_size_in_bytes: 512,
        physical_sector_size_in_bytes: 4096,
        parent_path: std::ptr::null(),
        source_path: std::ptr::null(),
        open_flags: 0,
        parent_virtual_storage_type: FridayVirtualStorageType {
            device_id: 0, vendor_id: zero_guid,
        },
        source_virtual_storage_type: FridayVirtualStorageType {
            device_id: 0, vendor_id: zero_guid,
        },
        resiliency_guid: zero_guid,
    };
    let parameters = FridayCreateVirtualDiskParameters {
        version: 2, // CREATE_VIRTUAL_DISK_VERSION_2
        parameters: FridayCreateVirtualDiskParametersUnion {
            version2: std::mem::ManuallyDrop::new(version2),
        },
    };

    let mut raw_handle: *mut std::ffi::c_void = std::ptr::null_mut();
    // VIRTUAL_DISK_ACCESS_NONE is required for Version 2. FULL_PHYSICAL_ALLOCATION
    // requests a fixed-size VHDX instead of a dynamically expanding backing file.
    let status = unsafe {
        CreateVirtualDisk(
            &storage_type,
            path_wide.as_ptr(),
            0,
            std::ptr::null(),
            0x1, // CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION
            0,
            &parameters,
            std::ptr::null_mut(),
            &mut raw_handle,
        )
    };
    if status != 0 {
        return Err(format!("Windows refused to create the private result VHDX (error {status}); the run directory was preserved."));
    }
    if raw_handle.is_null() {
        return Err("Windows reported VHDX creation without returning an owned handle; the run directory was preserved.".into());
    }
    OwnedVirtualDiskHandle(raw_handle).close()?;

    // Verify the resulting object through a no-follow file open. Never delete
    // an unexpected or partially-created file automatically; recovery owns it.
    let metadata = fs::symlink_metadata(&plan.vhdx_path)
        .map_err(|_| "Windows created a VHDX that FRIDAY could not safely inspect; it was preserved for recovery.".to_string())?;
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) || metadata.len() < 8 {
        return Err("The created result VHDX is not an ordinary non-empty file; it was preserved for recovery.".into());
    }
    let mut file = open_source_file(&plan.vhdx_path)
        .map_err(|_| "The created result VHDX could not be opened without following links; it was preserved.".to_string())?;
    let opened = file.metadata()
        .map_err(|_| "The created result VHDX handle could not be verified; it was preserved.".to_string())?;
    if !opened.is_file() || is_reparse_or_symlink(&opened) || opened.len() != metadata.len() {
        return Err("The result VHDX changed during verification; it was preserved for recovery.".into());
    }
    let mut signature = [0u8; 8];
    file.read_exact(&mut signature)
        .map_err(|_| "The result VHDX header could not be read; it was preserved for recovery.".to_string())?;
    if &signature != b"vhdxfile" {
        return Err("Windows returned a file without the expected VHDX signature; it was preserved for recovery.".into());
    }
    let identifier = verify_guest_result_vhdx_capacity(&plan.vhdx_path, plan.capacity_bytes)
        .map_err(|error| format!("The created VHDX failed Windows metadata verification ({error}); it was preserved for recovery."))?;
    write_guest_result_vhdx_ownership_marker(plan, trusted_storage_root, &identifier)
        .map_err(|error| format!("The created VHDX identity could not be bound to its FRIDAY run ({error}); it was preserved for recovery."))?;
    verify_guest_result_vhdx_identity(plan, trusted_storage_root)
        .map_err(|error| format!("The created VHDX failed its final read-only ownership check ({error}); it was preserved for recovery."))?;
    Ok(plan.vhdx_path.clone())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultVolumeOwnershipMarker {
    schema_version: u32,
    run_id: String,
    owner: String,
}

/// Atomically reserves a new per-run directory and writes its ownership record.
/// If marker creation or persistence fails, the incomplete directory is left
/// untouched for recovery rather than being recursively deleted.
fn reserve_guest_result_volume_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    fs::create_dir(&plan.run_directory)
        .map_err(|_| "FRIDAY could not exclusively reserve this result-volume run directory.".to_string())?;
    // This is private host state, never a general guest share. Apply the same
    // fail-closed owner/SYSTEM/Administrators boundary as the snapshot workspace
    // before publishing any ownership metadata.
    secure_private_workspace_directory(&plan.run_directory)
        .map_err(|_| "FRIDAY could not restrict the reserved result-volume directory; it was preserved for recovery.".to_string())?;

    let marker = GuestResultVolumeOwnershipMarker {
        schema_version: GUEST_RESULT_VOLUME_OWNER_SCHEMA,
        run_id: plan.run_id.clone(),
        owner: "FRIDAY-CodingAssistant-ResultVolume".to_string(),
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|_| "FRIDAY could not prepare result-volume ownership metadata.".to_string())?;
    let marker_path = plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker_path)
        .map_err(|_| "FRIDAY could not exclusively create result-volume ownership metadata; the directory was preserved.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write result-volume ownership metadata; the directory was preserved.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not persist result-volume ownership metadata; the directory was preserved.".to_string())?;
    verify_guest_result_volume_directory(plan, trusted_storage_root)
}

/// Reserves FRIDAY's exact empty host mount-point directory under the private
/// per-run storage folder. This does not mount a volume; native mount APIs must
/// still verify the volume GUID and exact mount-point binding before use.
fn reserve_guest_result_host_mount_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if plan.host_mount_directory != plan.run_directory.join("result-mount") {
        return Err("The result-volume host mount directory differs from FRIDAY's fixed per-run path.".into());
    }
    match fs::symlink_metadata(&plan.host_mount_directory) {
        Ok(_) => return Err("A result-volume host mount directory already exists; reuse is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The result-volume host mount directory could not be inspected safely.".into()),
    }
    fs::create_dir(&plan.host_mount_directory)
        .map_err(|_| "FRIDAY could not exclusively create its private host mount directory.".to_string())?;
    secure_private_workspace_directory(&plan.host_mount_directory)
        .map_err(|_| "FRIDAY could not secure the host mount directory; it was preserved for recovery.".to_string())?;
    verify_guest_result_host_mount_directory(plan, trusted_storage_root)
}

fn verify_guest_result_host_mount_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    let metadata = fs::symlink_metadata(&plan.host_mount_directory)
        .map_err(|_| "The private result-volume host mount directory is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The result-volume host mount point is not an ordinary directory.".into());
    }
    let canonical = fs::canonicalize(&plan.host_mount_directory)
        .map_err(|_| "The result-volume host mount directory could not be canonicalized.".to_string())?;
    if canonical.parent() != Some(plan.run_directory.as_path())
        || canonical.file_name() != Some(OsStr::new("result-mount"))
    {
        return Err("The result-volume host mount directory failed its exact-child ownership check.".into());
    }
    let mut entries = fs::read_dir(&canonical)
        .map_err(|_| "The result-volume host mount directory could not be enumerated.".to_string())?;
    if entries.next().is_some() {
        return Err("The result-volume host mount directory is not empty; mounting was refused.".into());
    }
    Ok(canonical)
}

/// Verifies the exact per-run child directory and its no-follow ownership marker.
/// This does not authorize disk detach or recursive deletion.
fn verify_guest_result_volume_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let root = fs::canonicalize(trusted_storage_root)
        .map_err(|_| "The trusted result-volume root could not be reverified.".to_string())?;
    let metadata = fs::symlink_metadata(&plan.run_directory)
        .map_err(|_| "The result-volume run directory is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The result-volume run directory is not an ordinary directory.".into());
    }
    let canonical = fs::canonicalize(&plan.run_directory)
        .map_err(|_| "The result-volume run directory could not be canonicalized.".to_string())?;
    if canonical.parent() != Some(root.as_path())
        || canonical.file_name().and_then(OsStr::to_str) != Some(plan.run_id.as_str())
    {
        return Err("The result-volume run directory failed its exact-child ownership check.".into());
    }

    let marker_path = canonical.join(GUEST_RESULT_VOLUME_OWNER_MARKER);
    let marker_metadata = fs::symlink_metadata(&marker_path)
        .map_err(|_| "Result-volume ownership evidence is missing; the directory was preserved.".to_string())?;
    if !marker_metadata.is_file() || is_reparse_or_symlink(&marker_metadata) || marker_metadata.len() > 1024 {
        return Err("Result-volume ownership evidence is not a small regular file.".into());
    }
    let mut marker_file = open_source_file(&marker_path)
        .map_err(|_| "Result-volume ownership evidence could not be opened without following links.".to_string())?;
    let opened_metadata = marker_file.metadata()
        .map_err(|_| "Result-volume ownership evidence could not be verified.".to_string())?;
    if !opened_metadata.is_file() || is_reparse_or_symlink(&opened_metadata) || opened_metadata.len() > 1024 {
        return Err("Result-volume ownership evidence changed or exceeded its size limit.".into());
    }
    let mut bytes = Vec::new();
    marker_file.take(1025).read_to_end(&mut bytes)
        .map_err(|_| "Result-volume ownership evidence could not be read.".to_string())?;
    if bytes.len() > 1024 {
        return Err("Result-volume ownership evidence exceeded its read limit.".into());
    }
    let marker: GuestResultVolumeOwnershipMarker = serde_json::from_slice(&bytes)
        .map_err(|_| "Result-volume ownership evidence is malformed.".to_string())?;
    if marker.schema_version != GUEST_RESULT_VOLUME_OWNER_SCHEMA
        || marker.run_id != plan.run_id
        || marker.owner != "FRIDAY-CodingAssistant-ResultVolume"
    {
        return Err("Result-volume ownership evidence does not match this FRIDAY run.".into());
    }
    Ok(canonical)
}


#[cfg(windows)]
fn windows_wide_null(value: &str) -> Result<Vec<u16>, String> {
    if value.is_empty() || value.contains('\0') {
        return Err("A Windows volume path was empty or contained a null character.".into());
    }
    use std::os::windows::ffi::OsStrExt;
    Ok(std::ffi::OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect())
}

#[cfg(windows)]
fn verify_guest_result_volume_mount(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_volume_path: &str,
) -> Result<(), String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if !valid_windows_volume_guid_path(expected_volume_path) {
        return Err("The result volume GUID path is malformed; mounting was refused.".into());
    }
    let mount_directory = verify_guest_result_host_mount_directory_for_mount(plan, trusted_storage_root)?;
    let mount_path = format!("{}\\", mount_directory.to_string_lossy().trim_end_matches(['\\', '/']));
    let mount_wide = windows_wide_null(&mount_path)?;
    let mut observed = [0u16; 1024];
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(mount_wide.as_ptr(), observed.as_mut_ptr(), observed.len() as u32)
    };
    if ok == 0 {
        return Err("Windows could not verify the result-volume mount point.".into());
    }
    let end = observed.iter().position(|unit| *unit == 0)
        .ok_or_else(|| "Windows returned an unterminated mounted-volume identity.".to_string())?;
    let observed_path = String::from_utf16(&observed[..end])
        .map_err(|_| "Windows returned an invalid mounted-volume identity.".to_string())?;
    if !observed_path.eq_ignore_ascii_case(expected_volume_path) {
        return Err("The host mount directory resolves to a different volume than FRIDAY's formatted result volume.".into());
    }
    Ok(())
}

#[cfg(windows)]
fn verify_guest_result_host_mount_directory_for_mount(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
) -> Result<PathBuf, String> {
    let canonical_run_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    if plan.host_mount_directory != plan.run_directory.join("result-mount")
        || plan.host_mount_directory.parent() != Some(plan.run_directory.as_path())
        || plan.host_mount_directory.file_name() != Some(OsStr::new("result-mount"))
    {
        return Err("The result-volume mount path differs from FRIDAY's fixed per-run child.".into());
    }
    // Do not classify the mount-point itself with std::fs metadata: on Windows
    // mounted-volume directories are reparse points and Rust's generic directory
    // flags do not reliably distinguish them from ordinary directories. The
    // exact fixed child path plus GetVolumeNameForVolumeMountPointW read-back in
    // verify_guest_result_volume_mount is the authoritative mount identity check.
    if fs::canonicalize(&plan.run_directory).map_err(|_| "The result-volume run directory could not be reverified.".to_string())?
        != canonical_run_directory
    {
        return Err("The result-volume run directory changed while verifying its mount point.".into());
    }
    Ok(plan.host_mount_directory.clone())
}

/// Mounts only the verified NTFS volume GUID at FRIDAY's reserved private directory.
/// This primitive is not wired to guest execution or Sandbox folder sharing.
#[cfg(windows)]
fn mount_guest_result_volume_at_private_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    volume_path: &str,
) -> Result<(), String> {
    if !valid_windows_volume_guid_path(volume_path) {
        return Err("The formatted result volume GUID path is invalid; mounting was refused.".into());
    }
    // Before mounting, require the original empty, ordinary private directory.
    let mount_directory = verify_guest_result_host_mount_directory(plan, trusted_storage_root)?;
    let mount_path = format!("{}\\", mount_directory.to_string_lossy().trim_end_matches(['\\', '/']));
    let mount_wide = windows_wide_null(&mount_path)?;
    let volume_wide = windows_wide_null(volume_path)?;
    // Refuse an already-mounted path rather than replacing or stacking a mapping.
    let mut existing = [0u16; 1024];
    if unsafe { GetVolumeNameForVolumeMountPointW(mount_wide.as_ptr(), existing.as_mut_ptr(), existing.len() as u32) } != 0 {
        return Err("The private result-volume mount directory is already a mount point; reuse is refused.".into());
    }
    if unsafe { SetVolumeMountPointW(mount_wide.as_ptr(), volume_wide.as_ptr()) } == 0 {
        return Err("Windows refused to mount the verified result volume at FRIDAY's private directory.".into());
    }
    if let Err(error) = verify_guest_result_volume_mount(plan, trusted_storage_root, volume_path) {
        // Undo only the exact mount point FRIDAY just created. If cleanup cannot be
        // verified, preserve the directory and surface the ambiguity to recovery.
        if unsafe { DeleteVolumeMountPointW(mount_wide.as_ptr()) } == 0 {
            return Err(format!("{error} Windows could not revoke the unverified mount; the result volume requires recovery."));
        }
        return Err(error);
    }
    Ok(())
}

/// Journaled mount gateway. The journal enters MountInProgress before the OS
/// mapping is changed and reaches Mounted only after volume-GUID read-back.
#[cfg(windows)]
fn mount_guest_result_volume_journaled(
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    journal_root: &Path,
    current_journal: &GuestResultStorageJournal,
    volume_path: &str,
) -> Result<GuestResultStorageJournal, String> {
    // Verify the attached backing disk through the exact handle FRIDAY owns;
    // a second path-opened VHDX handle is not ownership evidence while mounted.
    let identifier = verify_attached_guest_result_vhdx_identity(
        &attached.handle, attached.physical_path(), plan, trusted_storage_root,
    )?;
    if current_journal.run_id != plan.run_id
        || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::Formatted {
        return Err("The result-volume mount journal is not bound to this verified formatted FRIDAY volume.".into());
    }
    let in_progress = append_and_persist_guest_result_storage_transition(
        journal_root, current_journal, GuestResultStorageEvent::BeginMount, true, false,
    )?;
    if let Err(error) = mount_guest_result_volume_at_private_directory(plan, trusted_storage_root, volume_path) {
        let _ = append_and_persist_guest_result_storage_transition(
            journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, false,
        );
        return Err(format!("{error} The result volume was preserved for recovery; no automatic retry will occur."));
    }
    append_and_persist_guest_result_storage_transition(
        journal_root, &in_progress, GuestResultStorageEvent::CompleteMount, true, false,
    )
}

/// Journaled unmount gateway. It is permitted only after guest stop and share
/// revocation have already been durably recorded.
#[cfg(windows)]
fn unmount_guest_result_volume_journaled(
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    journal_root: &Path,
    current_journal: &GuestResultStorageJournal,
    expected_volume_path: &str,
) -> Result<GuestResultStorageJournal, String> {
    let identifier = verify_attached_guest_result_vhdx_identity(
        &attached.handle, attached.physical_path(), plan, trusted_storage_root,
    )?;
    if current_journal.run_id != plan.run_id
        || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::ShareRevoked {
        return Err("The result-volume unmount journal is not in the verified post-share-revocation state.".into());
    }
    let in_progress = append_and_persist_guest_result_storage_transition(
        journal_root, current_journal, GuestResultStorageEvent::BeginUnmount, true, true,
    )?;
    if let Err(error) = unmount_guest_result_volume_from_private_directory(
        plan, trusted_storage_root, expected_volume_path,
    ) {
        let _ = append_and_persist_guest_result_storage_transition(
            journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, true,
        );
        return Err(format!("{error} The result volume was preserved for recovery; no automatic retry will occur."));
    }
    append_and_persist_guest_result_storage_transition(
        journal_root, &in_progress, GuestResultStorageEvent::CompleteUnmount, true, true,
    )
}

/// Removes only FRIDAY's exact private directory mount and confirms it is no longer mapped.
#[cfg(windows)]
fn unmount_guest_result_volume_from_private_directory(
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    expected_volume_path: &str,
) -> Result<(), String> {
    verify_guest_result_volume_mount(plan, trusted_storage_root, expected_volume_path)?;
    let mount_directory = verify_guest_result_volume_directory(plan, trusted_storage_root)?
        .join("result-mount");
    let mount_path = format!("{}\\", mount_directory.to_string_lossy().trim_end_matches(['\\', '/']));
    let mount_wide = windows_wide_null(&mount_path)?;
    if unsafe { DeleteVolumeMountPointW(mount_wide.as_ptr()) } == 0 {
        return Err("Windows refused to unmount FRIDAY's verified private result-volume directory.".into());
    }
    let mut observed = [0u16; 1024];
    if unsafe { GetVolumeNameForVolumeMountPointW(mount_wide.as_ptr(), observed.as_mut_ptr(), observed.len() as u32) } != 0 {
        return Err("Windows still reports a volume mounted at FRIDAY's private result directory; recovery is required.".into());
    }
    Ok(())
}

const SANDBOX_EXECUTABLE_FILE_NAME: &str = "WindowsSandbox.exe";

const WINDOWS_SANDBOX_CLI_FILE_NAME: &str = "wsb.exe";
const MAX_WINDOWS_SANDBOX_CLI_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_WINDOWS_SANDBOX_CLI_SESSIONS: usize = 64;
const MIN_WINDOWS_SANDBOX_CLI_BUILD: u32 = 26_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowsSandboxCliCommand {
    Start,
    List,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowsSandboxCliPlan {
    executable_path: String,
    sandbox_id: String,
    arguments: Vec<String>,
}

fn valid_windows_sandbox_cli_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit()
        })
}

fn valid_windows_sandbox_cli_executable_path(path: &str) -> bool {
    valid_absolute_windows_drive_path(path)
        && path.rsplit(|character| character == '\\' || character == '/')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(WINDOWS_SANDBOX_CLI_FILE_NAME))
}

fn windows_sandbox_cli_signature_evidence_valid(signature_status: &str, signer_name: &str) -> bool {
    signature_status == "Valid" && signer_name == "Microsoft Corporation"
}

fn windows_sandbox_cli_runtime_supported(
    build: &str,
    executable_path: &str,
    version: &str,
    signature_status: &str,
    signer_name: &str,
) -> bool {
    let Ok(build_number) = build.trim().parse::<u32>() else {
        return false;
    };
    if build_number < MIN_WINDOWS_SANDBOX_CLI_BUILD
        || !valid_windows_sandbox_cli_executable_path(executable_path)
        || !windows_sandbox_cli_signature_evidence_valid(signature_status, signer_name)
    {
        return false;
    }

    // FileVersionInfo may include a descriptive suffix. Only its leading
    // dotted-numeric version is accepted as concrete runtime evidence.
    let version = version.split_whitespace().next().unwrap_or("");
    let components: Vec<&str> = version.split('.').collect();
    (2..=4).contains(&components.len())
        && components.iter().all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
        && components.iter().any(|component| component.bytes().any(|byte| byte != b'0'))
}

fn windows_sandbox_cli_id_from_run_id(run_id: &str) -> Result<String, String> {
    if !valid_snapshot_id(run_id) {
        return Err("The FRIDAY run identifier cannot be converted to a Windows Sandbox session identifier.".into());
    }
    let id = format!(
        "{}-{}-{}-{}-{}",
        &run_id[0..8], &run_id[8..12], &run_id[12..16], &run_id[16..20], &run_id[20..32],
    ).to_ascii_lowercase();
    if !valid_windows_sandbox_cli_id(&id) {
        return Err("The generated Windows Sandbox session identifier is invalid.".into());
    }
    Ok(id)
}

fn build_windows_sandbox_cli_plan(
    executable_path: &str,
    run_id: &str,
    command: WindowsSandboxCliCommand,
    config: Option<&str>,
) -> Result<WindowsSandboxCliPlan, String> {
    if !valid_windows_sandbox_cli_executable_path(executable_path) {
        return Err("The Windows Sandbox CLI must resolve to an absolute wsb.exe path.".into());
    }
    let sandbox_id = windows_sandbox_cli_id_from_run_id(run_id)?;
    let arguments = match command {
        WindowsSandboxCliCommand::Start => {
            let config = config.ok_or_else(|| "The Windows Sandbox CLI start plan requires FRIDAY's validated configuration.".to_string())?;
            if config.is_empty()
                || config.len() > MAX_SANDBOX_CONFIG_BYTES
                || config.contains('\0')
                || validate_sandbox_config_with_result_policy(config).is_err()
            {
                return Err("The Windows Sandbox CLI start configuration is empty, oversized, or violates FRIDAY's exact isolation policy.".into());
            }
            vec![
                "start".into(), "--raw".into(), "--id".into(), sandbox_id.clone(),
                "--config".into(), config.to_string(),
            ]
        }
        WindowsSandboxCliCommand::List => {
            if config.is_some() {
                return Err("A Windows Sandbox CLI list plan cannot contain configuration text.".into());
            }
            vec!["list".into(), "--raw".into()]
        }
        WindowsSandboxCliCommand::Stop => {
            if config.is_some() {
                return Err("A Windows Sandbox CLI stop plan cannot contain configuration text.".into());
            }
            vec!["stop".into(), "--raw".into(), "--id".into(), sandbox_id.clone()]
        }
    };
    Ok(WindowsSandboxCliPlan {
        executable_path: executable_path.to_string(),
        sandbox_id,
        arguments,
    })
}

fn validate_windows_sandbox_cli_plan(
    plan: &WindowsSandboxCliPlan,
    executable_path: &str,
    run_id: &str,
    command: WindowsSandboxCliCommand,
    config: Option<&str>,
) -> Result<(), String> {
    let expected = build_windows_sandbox_cli_plan(executable_path, run_id, command, config)?;
    if plan != &expected {
        return Err("The Windows Sandbox CLI plan differs from FRIDAY's fixed command, session-ID, or configuration policy.".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowsSandboxCliSessionStatus {
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowsSandboxCliSession {
    id: String,
    status: WindowsSandboxCliSessionStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsSandboxCliListResponse {
    windows_sandbox_environments: Vec<WindowsSandboxCliListEntry>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct WindowsSandboxCliListEntry {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Status", alias = "State")]
    status: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[cfg(windows)]
fn verify_windows_sandbox_cli_authenticode(executable_path: &str) -> Result<(), String> {
    if !valid_windows_sandbox_cli_executable_path(executable_path) {
        return Err("The Windows Sandbox CLI path is invalid before invocation.".into());
    }

    // Re-check the exact path immediately before process creation. The path is
    // passed as an environment value, never interpolated into PowerShell code.
    let script = r#"$ErrorActionPreference = 'Stop'
$target = $env:FRIDAY_WSB_VERIFY_PATH
if (-not $target -or -not (Test-Path -LiteralPath $target -PathType Leaf)) { exit 2 }
$signature = Get-AuthenticodeSignature -LiteralPath $target -ErrorAction Stop
$signer = ''
if ($signature.SignerCertificate) {
  $signer = [string]$signature.SignerCertificate.GetNameInfo(
    [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
}
[pscustomobject]@{ status = [string]$signature.Status; signer = $signer } | ConvertTo-Json -Compress"#;

    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("FRIDAY_WSB_VERIFY_PATH", executable_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (status, stdout) = run_bounded_probe(command)?;
    if !status.success() {
        return Err("The Windows Sandbox CLI signature could not be re-verified immediately before invocation.".into());
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SignatureEvidence {
        status: String,
        signer: String,
    }
    let evidence: SignatureEvidence = serde_json::from_slice(&stdout)
        .map_err(|_| "The Windows Sandbox CLI signature evidence is malformed.".to_string())?;
    if !windows_sandbox_cli_signature_evidence_valid(&evidence.status, &evidence.signer) {
        return Err("The Windows Sandbox CLI signature or publisher changed; invocation was blocked.".into());
    }
    Ok(())
}

#[cfg(windows)]
fn query_windows_sandbox_cli_inventory(
    executable_path: &str,
) -> Result<Vec<WindowsSandboxCliSession>, String> {
    // This is deliberately read-only: it never starts or stops a Sandbox.
    let run_id = "00000000000000000000000000000000";
    let plan = build_windows_sandbox_cli_plan(
        executable_path, run_id, WindowsSandboxCliCommand::List, None,
    )?;
    validate_windows_sandbox_cli_plan(
        &plan, executable_path, run_id, WindowsSandboxCliCommand::List, None,
    )?;
    verify_windows_sandbox_cli_authenticode(&plan.executable_path)?;

    // Invoke the resolved executable directly with the fixed argument vector.
    // No shell, command string, user-provided arguments, or inherited stdin.
    let mut command = Command::new(&plan.executable_path);
    command.args(&plan.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (status, stdout) = run_bounded_probe(command)?;
    if !status.success() {
        return Err("The Windows Sandbox CLI inventory command failed; session state is unverified.".into());
    }
    parse_windows_sandbox_cli_list(&stdout)
}

fn parse_windows_sandbox_cli_list(bytes: &[u8]) -> Result<Vec<WindowsSandboxCliSession>, String> {
    if bytes.is_empty() || bytes.len() > MAX_WINDOWS_SANDBOX_CLI_OUTPUT_BYTES {
        return Err("The Windows Sandbox CLI inventory is empty or exceeds its fixed output limit.".into());
    }
    let response: WindowsSandboxCliListResponse = serde_json::from_slice(bytes)
        .map_err(|_| "The Windows Sandbox CLI inventory is malformed or has an unsupported schema.".to_string())?;
    if !response.extra.is_empty() {
        return Err("The Windows Sandbox CLI inventory contains unsupported top-level fields.".into());
    }
    if response.windows_sandbox_environments.len() > MAX_WINDOWS_SANDBOX_CLI_SESSIONS {
        return Err("The Windows Sandbox CLI inventory contains more sessions than FRIDAY's fixed safety limit.".into());
    }
    let mut sessions = Vec::with_capacity(response.windows_sandbox_environments.len());
    for entry in response.windows_sandbox_environments {
        if !entry.extra.is_empty() || !valid_windows_sandbox_cli_id(&entry.id) {
            return Err("The Windows Sandbox CLI inventory contains an invalid or unsupported session record.".into());
        }
        let status = if entry.status.eq_ignore_ascii_case("running") {
            WindowsSandboxCliSessionStatus::Running
        } else if entry.status.eq_ignore_ascii_case("stopped") {
            WindowsSandboxCliSessionStatus::Stopped
        } else {
            return Err("The Windows Sandbox CLI inventory contains an unknown session status.".into());
        };
        sessions.push(WindowsSandboxCliSession { id: entry.id.to_ascii_lowercase(), status });
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    if sessions.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err("The Windows Sandbox CLI inventory contains duplicate session identifiers.".into());
    }
    Ok(sessions)
}

fn verify_windows_sandbox_cli_session(
    sessions: &[WindowsSandboxCliSession],
    expected_id: &str,
    expected_status: WindowsSandboxCliSessionStatus,
) -> Result<(), String> {
    if !valid_windows_sandbox_cli_id(expected_id) {
        return Err("The expected Windows Sandbox session identifier is invalid.".into());
    }
    match sessions.iter().find(|session| session.id.eq_ignore_ascii_case(expected_id)) {
        Some(session) if session.status == expected_status => Ok(()),
        _ => Err("The Windows Sandbox CLI did not report FRIDAY's exact session in the required state.".into()),
    }
}

/// A new FRIDAY run must never adopt, stop, or reuse a session that existed
/// before its own launch transaction. The CLI inventory must be empty first.
fn require_empty_windows_sandbox_cli_inventory(
    sessions: &[WindowsSandboxCliSession],
) -> Result<(), String> {
    if sessions.is_empty() {
        Ok(())
    } else {
        Err("A Windows Sandbox session already exists; FRIDAY will not adopt, stop, or reuse it.".into())
    }
}

/// After a launch request, accept only a fresh inventory containing exactly
/// the run-derived session ID in the expected state. Extra sessions make the
/// observation ambiguous and must never be silently ignored.
fn require_exclusive_windows_sandbox_cli_session(
    sessions: &[WindowsSandboxCliSession],
    expected_id: &str,
    expected_status: WindowsSandboxCliSessionStatus,
) -> Result<(), String> {
    if !valid_windows_sandbox_cli_id(expected_id) {
        return Err("The expected Windows Sandbox session identifier is invalid.".into());
    }
    if sessions.len() != 1 {
        return Err("The Windows Sandbox inventory is not exclusive to FRIDAY's expected session.".into());
    }
    verify_windows_sandbox_cli_session(sessions, expected_id, expected_status)
        .map_err(|_| "The Windows Sandbox inventory does not contain FRIDAY's exact session in the required state.".into())
}


#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowsSandboxLaunchPlan {
    executable_path: String,
    config_path: String,
    arguments: Vec<String>,
}

fn valid_absolute_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || value.chars().any(|character| character.is_control() || character == '"' || character == '<' || character == '>' || character == '|')
    {
        return false;
    }
    let normalized = value.replace('/', "\\");
    !normalized.split('\\').any(|component| component == "..")
        && !normalized.ends_with("\\")
}

fn build_windows_sandbox_launch_plan(
    system_root: &str,
    run_directory: &str,
) -> Result<WindowsSandboxLaunchPlan, String> {
    if !valid_absolute_windows_drive_path(system_root)
        || !valid_absolute_windows_drive_path(run_directory)
    {
        return Err("The Windows Sandbox executable root or private run directory is not a safe absolute Windows path.".into());
    }
    let normalized_root = system_root.replace('/', "\\").trim_end_matches('\\').to_string();
    let normalized_run = run_directory.replace('/', "\\").trim_end_matches('\\').to_string();
    let executable_path = format!("{normalized_root}\\System32\\{SANDBOX_EXECUTABLE_FILE_NAME}");
    let config_path = format!("{normalized_run}\\{SANDBOX_CONFIG_FILE_NAME}");
    let plan = WindowsSandboxLaunchPlan {
        executable_path,
        config_path: config_path.clone(),
        arguments: vec![config_path],
    };
    validate_windows_sandbox_launch_plan(&plan, system_root, run_directory)?;
    Ok(plan)
}

fn validate_windows_sandbox_launch_plan(
    plan: &WindowsSandboxLaunchPlan,
    system_root: &str,
    run_directory: &str,
) -> Result<(), String> {
    if !valid_absolute_windows_drive_path(system_root)
        || !valid_absolute_windows_drive_path(run_directory)
    {
        return Err("The trusted Windows Sandbox launch roots are invalid.".into());
    }
    let expected = build_windows_sandbox_launch_plan_unchecked(system_root, run_directory);
    if plan != &expected {
        return Err("The Windows Sandbox launch plan differs from FRIDAY's fixed executable and single-configuration-argument policy.".into());
    }
    Ok(())
}

fn build_windows_sandbox_launch_plan_unchecked(
    system_root: &str,
    run_directory: &str,
) -> WindowsSandboxLaunchPlan {
    let normalized_root = system_root.replace('/', "\\").trim_end_matches('\\').to_string();
    let normalized_run = run_directory.replace('/', "\\").trim_end_matches('\\').to_string();
    let executable_path = format!("{normalized_root}\\System32\\{SANDBOX_EXECUTABLE_FILE_NAME}");
    let config_path = format!("{normalized_run}\\{SANDBOX_CONFIG_FILE_NAME}");
    WindowsSandboxLaunchPlan {
        executable_path,
        config_path: config_path.clone(),
        arguments: vec![config_path],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SandboxSessionState {
    Planned,
    LaunchRequested,
    ProcessObserved,
    SessionConfirmed,
    Running,
    StopRequested,
    Stopped,
    Removed,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SandboxSessionEvent {
    RequestLaunch,
    ObserveProcess,
    ConfirmSession,
    ConfirmRunning,
    RequestStop,
    ConfirmStopped,
    RemoveOwnershipRecord,
    PreserveForRecovery,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SandboxProcessIdentity {
    session_id: String,
    process_id: u32,
    process_creation_time: u64,
    image_path: String,
}

fn valid_windows_sandbox_image_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let unc_absolute = value.starts_with(r"\\") || value.starts_with(r"\\?\\");
    let normalized = value.replace('/', "\\");
    let has_parent_component = normalized.split('\\').any(|part| part == "..");
    let file_name = normalized.rsplit('\\').next().unwrap_or("");
    (drive_absolute || unc_absolute)
        && !has_parent_component
        && file_name.eq_ignore_ascii_case("WindowsSandbox.exe")
}

fn validate_sandbox_process_identity(identity: &SandboxProcessIdentity) -> Result<(), String> {
    if !valid_snapshot_id(&identity.session_id)
        || identity.process_id <= 4
        || identity.process_creation_time == 0
        || !valid_windows_sandbox_image_path(&identity.image_path)
    {
        return Err("The Windows Sandbox process identity is incomplete or invalid.".into());
    }
    Ok(())
}

/// Pure ownership gate. A stop or cleanup is authorized only for the exact
/// process identity FRIDAY recorded and re-verified; PID alone is never enough.
fn advance_sandbox_session(
    state: SandboxSessionState,
    event: SandboxSessionEvent,
    identity: Option<&SandboxProcessIdentity>,
    observed_identity: Option<&SandboxProcessIdentity>,
    ownership_verified: bool,
    process_confirmed_stopped: bool,
) -> Result<SandboxSessionState, String> {
    if event == SandboxSessionEvent::PreserveForRecovery {
        return if state == SandboxSessionState::Removed {
            Err("A removed Sandbox session record cannot be restored to recovery.".into())
        } else {
            Ok(SandboxSessionState::RecoveryRequired)
        };
    }
    if state == SandboxSessionState::RecoveryRequired {
        return Err("Sandbox session ownership is ambiguous; automatic actions are blocked.".into());
    }
    match event {
        SandboxSessionEvent::RequestLaunch if state == SandboxSessionState::Planned => {
            Ok(SandboxSessionState::LaunchRequested)
        }
        SandboxSessionEvent::ObserveProcess if state == SandboxSessionState::LaunchRequested => {
            let observed = observed_identity.ok_or_else(|| "No Sandbox process identity was observed.".to_string())?;
            validate_sandbox_process_identity(observed)?;
            Ok(SandboxSessionState::ProcessObserved)
        }
        SandboxSessionEvent::ConfirmSession if state == SandboxSessionState::ProcessObserved => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The observed Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified {
                return Err("The observed Sandbox process does not match FRIDAY's exact owned session.".into());
            }
            Ok(SandboxSessionState::SessionConfirmed)
        }
        SandboxSessionEvent::ConfirmRunning if state == SandboxSessionState::SessionConfirmed => {
            if !ownership_verified {
                return Err("Sandbox ownership was not re-verified before marking the session running.".into());
            }
            Ok(SandboxSessionState::Running)
        }
        SandboxSessionEvent::RequestStop if state == SandboxSessionState::Running => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The current Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified {
                return Err("Stopping was refused because the Sandbox process identity or ownership changed.".into());
            }
            Ok(SandboxSessionState::StopRequested)
        }
        SandboxSessionEvent::ConfirmStopped if state == SandboxSessionState::StopRequested => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            let observed = observed_identity.ok_or_else(|| "The final Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            validate_sandbox_process_identity(observed)?;
            if expected != observed || !ownership_verified || !process_confirmed_stopped {
                return Err("The exact owned Sandbox process was not positively verified as stopped.".into());
            }
            Ok(SandboxSessionState::Stopped)
        }
        SandboxSessionEvent::RemoveOwnershipRecord if state == SandboxSessionState::Stopped => {
            let expected = identity.ok_or_else(|| "FRIDAY's recorded Sandbox process identity is missing.".to_string())?;
            validate_sandbox_process_identity(expected)?;
            if !ownership_verified {
                return Err("Sandbox ownership evidence is required before removing its session record.".into());
            }
            Ok(SandboxSessionState::Removed)
        }
        _ => Err("The Sandbox session transition is out of order or lacks exact ownership evidence.".into()),
    }
}


const SANDBOX_SESSION_JOURNAL_SCHEMA: u32 = 1;
const MAX_SANDBOX_SESSION_JOURNAL_BYTES: usize = 16 * 1024;
const MAX_SANDBOX_SESSION_JOURNAL_ENTRIES: usize = 8;
const SANDBOX_SESSION_JOURNAL_DIR_PREFIX: &str = ".friday-sandbox-session-";
const SANDBOX_SESSION_JOURNAL_FILE: &str = "state.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SandboxSessionJournalEntry {
    sequence: u32,
    previous_state: SandboxSessionState,
    event: SandboxSessionEvent,
    next_state: SandboxSessionState,
    identity: Option<SandboxProcessIdentity>,
    observed_identity: Option<SandboxProcessIdentity>,
    ownership_verified: bool,
    process_confirmed_stopped: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SandboxSessionJournal {
    schema_version: u32,
    run_id: String,
    entries: Vec<SandboxSessionJournalEntry>,
}

fn validate_sandbox_session_journal(
    journal: &SandboxSessionJournal,
) -> Result<SandboxSessionState, String> {
    if journal.schema_version != SANDBOX_SESSION_JOURNAL_SCHEMA {
        return Err("The Sandbox session journal schema version is invalid.".into());
    }
    if !valid_snapshot_id(&journal.run_id) {
        return Err(format!(
            "The Sandbox session journal run identifier is invalid ({} characters).",
            journal.run_id.len(),
        ));
    }
    if journal.entries.len() > MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
        return Err("The Sandbox session journal exceeds its fixed transition limit.".into());
    }
    let mut state = SandboxSessionState::Planned;
    let mut recorded_identity: Option<SandboxProcessIdentity> = None;
    for (index, entry) in journal.entries.iter().enumerate() {
        if entry.sequence != index as u32 + 1 || entry.previous_state != state {
            return Err("The Sandbox session journal is out of sequence or has a broken state chain.".into());
        }
        if entry.identity != recorded_identity {
            return Err("The Sandbox session journal changed FRIDAY's recorded process identity.".into());
        }
        if entry.event == SandboxSessionEvent::ObserveProcess {
            if entry.observed_identity.is_none() || entry.identity.is_some()
                || entry.ownership_verified || entry.process_confirmed_stopped
            {
                return Err("The observed Sandbox process record contains unexpected ownership evidence.".into());
            }
        } else if entry.event != SandboxSessionEvent::PreserveForRecovery
            && entry.event != SandboxSessionEvent::RemoveOwnershipRecord
            && entry.event != SandboxSessionEvent::RequestLaunch
            && entry.observed_identity.is_none()
        {
            return Err("The Sandbox session transition is missing its observed process identity.".into());
        }
        state = advance_sandbox_session(
            state,
            entry.event,
            recorded_identity.as_ref(),
            entry.observed_identity.as_ref(),
            entry.ownership_verified,
            entry.process_confirmed_stopped,
        )?;
        if entry.next_state != state {
            return Err("The Sandbox session journal contains an invalid transition result.".into());
        }
        if entry.event == SandboxSessionEvent::ObserveProcess {
            recorded_identity = entry.observed_identity.clone();
        }
    }
    Ok(state)
}

#[cfg(windows)]
fn observe_windows_sandbox_process_identity(process_id: u32, run_id: &str) -> Result<SandboxProcessIdentity, String> {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    if !valid_snapshot_id(run_id) || process_id <= 4 {
        return Err("The Sandbox process observation request is invalid.".into());
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if raw.is_null() {
        return Err("Windows could not open the recorded Sandbox process for limited identity inspection.".into());
    }
    let _process = OwnedObservedProcessHandle(raw);
    let mut creation = FridayFileTime { low: 0, high: 0 };
    let mut exit = FridayFileTime { low: 0, high: 0 };
    let mut kernel = FridayFileTime { low: 0, high: 0 };
    let mut user = FridayFileTime { low: 0, high: 0 };
    if unsafe { GetProcessTimes(raw, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err("Windows could not verify the recorded Sandbox process creation time.".into());
    }
    let creation_time = ((creation.high as u64) << 32) | creation.low as u64;
    if creation_time == 0 { return Err("Windows returned an invalid Sandbox process creation time.".into()); }

    let mut image_buffer = vec![0u16; 32_768];
    let mut image_length = image_buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(raw, 0, image_buffer.as_mut_ptr(), &mut image_length) } == 0
        || image_length == 0 || image_length as usize > image_buffer.len()
    {
        return Err("Windows could not verify the recorded Sandbox executable path.".into());
    }
    image_buffer.truncate(image_length as usize);
    let image_path = String::from_utf16(&image_buffer)
        .map_err(|_| "Windows returned an invalid Sandbox executable path.".to_string())?;

    let mut system_buffer = vec![0u16; 32_768];
    let system_length = unsafe { GetSystemDirectoryW(system_buffer.as_mut_ptr(), system_buffer.len() as u32) };
    if system_length == 0 || system_length as usize >= system_buffer.len() {
        return Err("Windows could not establish its system directory for Sandbox identity verification.".into());
    }
    system_buffer.truncate(system_length as usize);
    let system_directory = String::from_utf16(&system_buffer)
        .map_err(|_| "Windows returned an invalid system directory.".to_string())?;
    if !windows_sandbox_image_path_in_system_directory(&image_path, &system_directory) {
        return Err("The observed process image is not the Windows system-directory Sandbox executable.".into());
    }
    let identity = SandboxProcessIdentity {
        session_id: run_id.to_string(), process_id, process_creation_time: creation_time, image_path,
    };
    validate_sandbox_process_identity(&identity)?;
    Ok(identity)
}

fn windows_sandbox_image_path_in_system_directory(image_path: &str, system_directory: &str) -> bool {
    // Compare path components directly so this check is independent of the
    // host OS path parser and of separator replacement/escaping behavior.
    let image_parts: Vec<&str> = image_path
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();
    let directory_parts: Vec<&str> = system_directory
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();

    image_parts.len() == directory_parts.len() + 1
        && image_parts
            .iter()
            .zip(directory_parts.iter())
            .all(|(image, directory)| image.eq_ignore_ascii_case(directory))
        && image_parts
            .last()
            .is_some_and(|name| name.eq_ignore_ascii_case("WindowsSandbox.exe"))
}

#[cfg(windows)]
fn reobserve_windows_sandbox_process_for_stop(journal: &SandboxSessionJournal, ownership_verified: bool) -> Result<(), String> {
    if validate_sandbox_session_journal(journal)? != SandboxSessionState::Running {
        return Err("The Sandbox journal is not in the Running state.".into());
    }
    let recorded = journal.entries.iter().rev()
        .find(|entry| entry.event == SandboxSessionEvent::ObserveProcess)
        .and_then(|entry| entry.observed_identity.as_ref())
        .ok_or_else(|| "The Sandbox journal has no recorded process identity.".to_string())?;
    let observed = observe_windows_sandbox_process_identity(recorded.process_id, &journal.run_id)?;
    verify_reobserved_sandbox_process_for_stop(journal, &observed, ownership_verified)
}

/// Require a fresh native process observation to match the exact identity
/// recorded by the running journal before a stop transaction can proceed.
/// The caller must set ownership_verified only after an authoritative Windows
/// observation (not a PID/name-only lookup) has established causal ownership.
fn verify_reobserved_sandbox_process_for_stop(
    journal: &SandboxSessionJournal,
    observed_identity: &SandboxProcessIdentity,
    ownership_verified: bool,
) -> Result<(), String> {
    if validate_sandbox_session_journal(journal)? != SandboxSessionState::Running {
        return Err("The Sandbox journal is not in the Running state.".into());
    }
    validate_sandbox_process_identity(observed_identity)?;
    let recorded_identity = journal.entries.iter()
        .rev()
        .find(|entry| entry.event == SandboxSessionEvent::ObserveProcess)
        .and_then(|entry| entry.observed_identity.as_ref())
        .ok_or_else(|| "The Sandbox journal has no recorded process identity.".to_string())?;
    if !recorded_identity.session_id.eq_ignore_ascii_case(&journal.run_id)
        || recorded_identity != observed_identity
        || !ownership_verified
    {
        return Err("The current Sandbox process could not be proven to be the exact FRIDAY-owned session.".into());
    }
    Ok(())
}

/// Prepare a stop command only when the validated durable journal records
/// this run as Running, its recorded process identity is bound to the
/// journal's 32-character run ID, and the fresh CLI inventory contains only
/// the separately derived CLI session ID in Running state. This builds a
/// plan; it does not execute the command.
fn prepare_authorized_windows_sandbox_stop_plan(
    executable_path: &str,
    journal: &SandboxSessionJournal,
    sessions: &[WindowsSandboxCliSession],
) -> Result<WindowsSandboxCliPlan, String> {
    if validate_sandbox_session_journal(journal)? != SandboxSessionState::Running {
        return Err("The Sandbox journal does not authorize stopping from its current state.".into());
    }
    let expected_id = windows_sandbox_cli_id_from_run_id(&journal.run_id)?;
    let recorded_identity = journal.entries.iter()
        .rev()
        .find(|entry| entry.event == SandboxSessionEvent::ObserveProcess)
        .and_then(|entry| entry.observed_identity.as_ref())
        .ok_or_else(|| "The Sandbox journal has no recorded process identity.".to_string())?;
    if !recorded_identity.session_id.eq_ignore_ascii_case(&journal.run_id) {
        return Err("The recorded Sandbox process identity does not match this FRIDAY run.".into());
    }
    require_exclusive_windows_sandbox_cli_session(
        sessions, &expected_id, WindowsSandboxCliSessionStatus::Running,
    )?;
    let plan = build_windows_sandbox_cli_plan(
        executable_path, &journal.run_id, WindowsSandboxCliCommand::Stop, None,
    )?;
    validate_windows_sandbox_cli_plan(
        &plan, executable_path, &journal.run_id, WindowsSandboxCliCommand::Stop, None,
    )?;
    Ok(plan)
}

fn encode_sandbox_session_journal(
    journal: &SandboxSessionJournal,
) -> Result<Vec<u8>, String> {
    validate_sandbox_session_journal(journal)?;
    let bytes = serde_json::to_vec(journal)
        .map_err(|_| "The Sandbox session journal could not be encoded.".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_SANDBOX_SESSION_JOURNAL_BYTES {
        return Err("The Sandbox session journal exceeds its fixed size limit.".into());
    }
    Ok(bytes)
}

fn parse_sandbox_session_journal(
    bytes: &[u8],
) -> Result<SandboxSessionJournal, String> {
    if bytes.is_empty() || bytes.len() > MAX_SANDBOX_SESSION_JOURNAL_BYTES {
        return Err("The Sandbox session journal is empty or oversized.".into());
    }
    let journal: SandboxSessionJournal = serde_json::from_slice(bytes)
        .map_err(|_| "The Sandbox session journal is malformed or contains unsupported fields.".to_string())?;
    if journal.entries.is_empty() {
        return Err("The persisted Sandbox session journal contains no recorded transitions.".into());
    }
    validate_sandbox_session_journal(&journal)?;
    Ok(journal)
}

fn append_sandbox_session_journal_entry(
    journal: &SandboxSessionJournal,
    event: SandboxSessionEvent,
    observed_identity: Option<&SandboxProcessIdentity>,
    ownership_verified: bool,
    process_confirmed_stopped: bool,
) -> Result<SandboxSessionJournal, String> {
    let state = validate_sandbox_session_journal(journal)?;
    if journal.entries.len() >= MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
        return Err("The Sandbox session journal reached its fixed transition limit.".into());
    }
    let recorded_identity = journal.entries.iter()
        .rev()
        .find(|entry| entry.event == SandboxSessionEvent::ObserveProcess)
        .and_then(|entry| entry.observed_identity.as_ref());
    let next_state = advance_sandbox_session(
        state, event, recorded_identity, observed_identity,
        ownership_verified, process_confirmed_stopped,
    )?;
    let mut next = journal.clone();
    next.entries.push(SandboxSessionJournalEntry {
        sequence: next.entries.len() as u32 + 1,
        previous_state: state,
        event,
        next_state,
        identity: recorded_identity.cloned(),
        observed_identity: observed_identity.cloned(),
        ownership_verified,
        process_confirmed_stopped,
    });
    encode_sandbox_session_journal(&next)?;
    Ok(next)
}

fn sandbox_session_journal_generation_path(root: &Path, generation: usize) -> PathBuf {
    root.join(format!("{SANDBOX_SESSION_JOURNAL_DIR_PREFIX}{generation:02}"))
}

fn read_sandbox_session_journal_generation(
    root: &Path,
    generation: usize,
) -> Result<SandboxSessionJournal, String> {
    if generation == 0 || generation > MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
        return Err("The Sandbox session journal generation is outside its fixed limit.".into());
    }
    let directory = sandbox_session_journal_generation_path(root, generation);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|_| "A Sandbox session journal generation is missing or unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("A Sandbox session journal generation is not an ordinary directory.".into());
    }
    let bytes = crate::coding_sandbox_fs::read_single_bounded_file_from_directory(
        &directory, OsStr::new(SANDBOX_SESSION_JOURNAL_FILE),
        MAX_SANDBOX_SESSION_JOURNAL_BYTES as u64,
    ).map_err(|_| "A Sandbox session journal generation is incomplete or unsafe.".to_string())?;
    let journal = parse_sandbox_session_journal(&bytes)?;
    if journal.entries.len() != generation {
        return Err("A Sandbox session journal generation does not match its sequence.".into());
    }
    Ok(journal)
}

/// Persists each session transition as an immutable generation. Existing or
/// interrupted generations are never overwritten; ambiguity requires recovery.
fn persist_sandbox_session_journal(
    root: &Path,
    journal: &SandboxSessionJournal,
) -> Result<(), String> {
    validate_sandbox_session_journal(journal)?;
    let generation = journal.entries.len();
    if generation == 0 || generation > MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
        return Err("Only a non-empty bounded Sandbox session journal can be persisted.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The Sandbox session journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The Sandbox session journal root must be an ordinary directory.".into());
    }
    if generation > 1 {
        let previous = read_sandbox_session_journal_generation(root, generation - 1)?;
        if previous.run_id != journal.run_id
            || previous.entries != journal.entries[..generation - 1]
        {
            return Err("The next Sandbox session journal generation does not extend persisted history.".into());
        }
    } else if fs::symlink_metadata(sandbox_session_journal_generation_path(root, 1)).is_ok() {
        return Err("The initial Sandbox session journal generation already exists.".into());
    }
    let directory = sandbox_session_journal_generation_path(root, generation);
    fs::create_dir(&directory)
        .map_err(|_| "FRIDAY could not exclusively create the Sandbox session journal generation.".to_string())?;
    secure_private_workspace_directory(&directory)
        .map_err(|_| "FRIDAY could not secure the new session journal generation; it was preserved.".to_string())?;
    let bytes = encode_sandbox_session_journal(journal)?;
    let file_path = directory.join(SANDBOX_SESSION_JOURNAL_FILE);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&file_path)
        .map_err(|_| "FRIDAY could not exclusively create the Sandbox session journal file; it was preserved.".to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|_| "FRIDAY could not write the Sandbox session journal; it was preserved.".to_string())?;
    file.sync_all()
        .map_err(|_| "FRIDAY could not flush the Sandbox session journal; it was preserved.".to_string())?;
    drop(file);
    let verified = read_sandbox_session_journal_generation(root, generation)?;
    if verified != *journal {
        return Err("The persisted Sandbox session journal failed exact read-back verification.".into());
    }
    Ok(())
}

fn load_sandbox_session_journal(
    root: &Path,
    expected_run_id: &str,
) -> Result<SandboxSessionJournal, String> {
    if !valid_snapshot_id(expected_run_id) {
        return Err("The expected Sandbox session run identifier is invalid.".into());
    }
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The Sandbox session journal root is unavailable.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The Sandbox session journal root must be an ordinary directory.".into());
    }
    let mut latest: Option<SandboxSessionJournal> = None;
    let mut gap_seen = false;
    for generation in 1..=MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
        let path = sandbox_session_journal_generation_path(root, generation);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => gap_seen = true,
            Err(_) => return Err("A Sandbox session journal generation could not be inspected.".into()),
            Ok(_) if gap_seen => return Err("The Sandbox session journal contains a generation gap.".into()),
            Ok(_) => {
                let current = read_sandbox_session_journal_generation(root, generation)?;
                if current.run_id != expected_run_id {
                    return Err("The Sandbox session journal belongs to a different FRIDAY run.".into());
                }
                if let Some(previous) = &latest {
                    if current.entries[..generation - 1] != previous.entries {
                        return Err("The Sandbox session journal history is not append-only.".into());
                    }
                }
                latest = Some(current);
            }
        }
    }
    latest.ok_or_else(|| "No complete Sandbox session journal generation is available.".into())
}

fn append_and_persist_sandbox_session_transition(
    root: &Path,
    current: &SandboxSessionJournal,
    event: SandboxSessionEvent,
    observed_identity: Option<&SandboxProcessIdentity>,
    ownership_verified: bool,
    process_confirmed_stopped: bool,
) -> Result<SandboxSessionJournal, String> {
    validate_sandbox_session_journal(current)?;
    if current.entries.is_empty() {
        for generation in 1..=MAX_SANDBOX_SESSION_JOURNAL_ENTRIES {
            match fs::symlink_metadata(sandbox_session_journal_generation_path(root, generation)) {
                Ok(_) => return Err("An initial Sandbox session journal cannot overwrite existing recovery evidence.".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Existing Sandbox session journal evidence could not be inspected.".into()),
            }
        }
    } else if load_sandbox_session_journal(root, &current.run_id)? != *current {
        return Err("The supplied Sandbox session journal is stale or differs from persisted history.".into());
    }
    let next = append_sandbox_session_journal_entry(
        current, event, observed_identity, ownership_verified, process_confirmed_stopped,
    )?;
    persist_sandbox_session_journal(root, &next)?;
    if load_sandbox_session_journal(root, &next.run_id)? != next {
        return Err("The newly appended Sandbox session transition failed recovery verification.".into());
    }
    Ok(next)
}

fn sandbox_policy_state(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => "Not configured".into(),
        Some(value) if value.as_i64() == Some(0) => "Blocked".into(),
        Some(value) if value.as_i64() == Some(1) => "Allowed".into(),
        _ => "Unknown".into(),
    }
}

fn privilege_output_has_enabled(output: &str, privilege: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(privilege) && fields.last() == Some("Enabled")
    })
}

#[tauri::command]
pub async fn coding_sandbox_preflight() -> Result<CodingSandboxPreflight, String> {
    let _project_root = crate::coding_assistant::selected_project_root_for_sandbox()?;
    #[cfg(windows)]
    {
        let detected = detect_windows_sandbox();
        let (edition, build, feature, wsb_available, wsb_path, wsb_version, virtualization, active,
            virtual_disk_api, manage_volume_privilege, hyperv_cmdlets,
            mapped_folders_policy, mapped_folder_write_policy) = detect_windows_sandbox_capabilities();
        let edition_supported = ["Professional", "Enterprise", "Education"]
            .iter().any(|supported| edition.eq_ignore_ascii_case(supported));
        let cli_runtime_supported =
            windows_sandbox_cli_runtime_supported(
                &build, &wsb_path, &wsb_version,
                if wsb_available { "Valid" } else { "Unknown" },
                if wsb_available { "Microsoft Corporation" } else { "Unknown" },
            );
        let runtime_ready = detected && edition_supported
            && feature.eq_ignore_ascii_case("Enabled") && cli_runtime_supported
            && virtualization == Some(true) && !active;
        let message = if active {
            "A Windows Sandbox process is already running. FRIDAY will not attach to or stop a session it did not create. Execution remains disabled.".to_string()
        } else if runtime_ready {
            "Windows Sandbox prerequisites appear present. This is detection only, not a launch test. Result-storage readiness is not established; execution remains disabled until storage, command restrictions, resource enforcement, cancellation and result validation are verified.".to_string()
        } else {
            "Windows Sandbox is not yet confirmed ready. Review the detected edition, feature, CLI, virtualization and result-storage capability details. Unknown values are treated as unverified. FRIDAY will not elevate or fall back to host execution.".to_string()
        };
        Ok(CodingSandboxPreflight {
            platform_supported: true, runtime_detected: detected,
            execution_enabled: false, network_isolation_enforced: false,
            windows_edition: edition, windows_build: build, sandbox_feature_state: feature,
            wsb_cli_available: wsb_available, wsb_cli_version: wsb_version,
            virtualization_firmware_enabled: virtualization, sandbox_process_detected: active,
            virtual_disk_api_available: virtual_disk_api,
            manage_volume_privilege_enabled: manage_volume_privilege,
            hyperv_cmdlets_available: hyperv_cmdlets,
            mapped_folders_policy,
            mapped_folder_write_policy,
            result_storage_ready: false,
            message,
        })
    }
    #[cfg(not(windows))]
    {
        Ok(CodingSandboxPreflight {
            platform_supported: false, runtime_detected: false,
            execution_enabled: false, network_isolation_enforced: false,
            windows_edition: "Not applicable".into(), windows_build: "Not applicable".into(),
            sandbox_feature_state: "Not applicable".into(), wsb_cli_available: false,
            wsb_cli_version: "Not applicable".into(), virtualization_firmware_enabled: None,
            sandbox_process_detected: false,
            virtual_disk_api_available: false, manage_volume_privilege_enabled: false,
            hyperv_cmdlets_available: false,
            mapped_folders_policy: "Not applicable".into(),
            mapped_folder_write_policy: "Not applicable".into(),
            result_storage_ready: false,
            message: "The Coding Assistant execution sandbox is designed for Windows Sandbox. No project code has been executed and no host-execution fallback is available.".to_string(),
        })
    }
}

fn safe_project_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty()
        || lower.starts_with('.')
        || ["secret", "credential", "password", "token", "private", "id_rsa", ".env"]
            .iter().any(|needle| lower.contains(needle))
    {
        return false;
    }
    matches!(
        Path::new(name).extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase).as_deref(),
        Some("rs" | "toml" | "json" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
            | "py" | "pyi" | "go" | "mod" | "sum" | "java" | "kt" | "kts" | "cs"
            | "fs" | "fsx" | "vb" | "cpp" | "c" | "h" | "hpp" | "cc" | "html"
            | "css" | "scss" | "sass" | "md" | "mdx" | "yaml" | "yml" | "xml"
            | "sql" | "sh" | "bat" | "ps1" | "txt" | "lock" | "gradle" | "properties")
    ) || matches!(lower.as_str(), "makefile" | "dockerfile" | "cmakelists.txt")
}

fn excluded_directory(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.') || matches!(
        lower.as_str(),
        "node_modules" | "target" | "dist" | "build" | "out" | "coverage"
            | "vendor" | ".git" | ".svn" | ".hg" | ".idea" | ".vscode"
            | ".friday-code-backups" | "venv" | ".venv" | "__pycache__"
    )
}

struct SnapshotBudget {
    files: usize,
    bytes: u64,
    truncated: bool,
}

fn safe_relative_path(path: &Path) -> bool {
    path.components().all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(windows)]
fn open_source_file(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OPEN_REPARSE_POINT: inspect the link itself instead of following it.
    OpenOptions::new().read(true).custom_flags(0x0020_0000).open(path)
        .map_err(|_| "A project file could not be opened safely.".to_string())
}

#[cfg(unix)]
fn open_source_file(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW closes the final-component symlink race between lstat-style
    // validation and open. Parent-directory identity is still checked by callers.
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
        .map_err(|_| "A project file could not be opened safely.".to_string())
}

#[cfg(not(any(windows, unix)))]
fn open_source_file(path: &Path) -> Result<File, String> {
    File::open(path).map_err(|_| "A project file could not be opened safely.".to_string())
}

fn copy_snapshot_tree(
    source_root: &Path,
    current: &Path,
    destination_root: &Path,
    depth: usize,
    budget: &mut SnapshotBudget,
) -> Result<(), String> {
    if depth > MAX_SNAPSHOT_DEPTH {
        budget.truncated = true;
        return Ok(());
    }
    let entries = fs::read_dir(current)
        .map_err(|_| "FRIDAY could not safely read part of the selected project.".to_string())?;
    for entry in entries {
        if budget.files >= MAX_SNAPSHOT_FILES || budget.bytes >= MAX_SNAPSHOT_TOTAL_BYTES {
            budget.truncated = true;
            break;
        }
        let entry = match entry { Ok(value) => value, Err(_) => { budget.truncated = true; continue; } };
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else { budget.truncated = true; continue; };
        let source = entry.path();
        let metadata = match fs::symlink_metadata(&source) {
            Ok(value) => value,
            Err(_) => { budget.truncated = true; continue; }
        };
        if is_reparse_or_symlink(&metadata) { continue; }
        if metadata.is_dir() {
            if excluded_directory(name_text) { continue; }
            let relative = source.strip_prefix(source_root).map_err(|_| "Project path escaped its selected root.".to_string())?;
            if !safe_relative_path(relative) { continue; }
            let target_dir = destination_root.join(relative);
            fs::create_dir_all(&target_dir).map_err(|_| "FRIDAY could not prepare the isolated project copy.".to_string())?;
            copy_snapshot_tree(source_root, &source, destination_root, depth + 1, budget)?;
            if budget.truncated { break; }
            continue;
        }
        if !metadata.is_file() || !safe_project_file_name(name_text)
            || metadata.len() == 0 || metadata.len() > MAX_SNAPSHOT_FILE_BYTES
        {
            continue;
        }
        let relative = source.strip_prefix(source_root).map_err(|_| "Project path escaped its selected root.".to_string())?;
        if !safe_relative_path(relative) { continue; }
        let canonical = match fs::canonicalize(&source) { Ok(value) => value, Err(_) => continue };
        if !canonical.starts_with(source_root) { continue; }

        let mut file = match open_source_file(&source) {
            Ok(value) => value,
            Err(_) => { budget.truncated = true; continue; }
        };
        let opened_metadata = file.metadata().map_err(|_| "FRIDAY could not verify a project file.".to_string())?;
        if is_reparse_or_symlink(&opened_metadata) || !opened_metadata.is_file()
            || opened_metadata.len() != metadata.len()
        { continue; }
        let remaining = MAX_SNAPSHOT_TOTAL_BYTES.saturating_sub(budget.bytes);
        let limit = MAX_SNAPSHOT_FILE_BYTES.min(remaining);
        let mut contents = Vec::with_capacity(opened_metadata.len() as usize);
        file.take(limit.saturating_add(1)).read_to_end(&mut contents)
            .map_err(|_| "FRIDAY could not read a project file safely.".to_string())?;
        if contents.is_empty() || contents.len() as u64 > limit {
            budget.truncated = true;
            continue;
        }
        let after = fs::symlink_metadata(&source).map_err(|_| "A project file changed during snapshot preparation.".to_string())?;
        if is_reparse_or_symlink(&after) || after.len() != opened_metadata.len() { continue; }

        let target = destination_root.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|_| "FRIDAY could not prepare the isolated project copy.".to_string())?;
        }
        let mut output = OpenOptions::new().write(true).create_new(true).open(&target)
            .map_err(|_| "FRIDAY could not create a new isolated snapshot file.".to_string())?;
        use std::io::Write;
        output.write_all(&contents).map_err(|_| "FRIDAY could not finish writing the isolated project copy.".to_string())?;
        output.sync_all().map_err(|_| "FRIDAY could not verify the isolated project copy was written.".to_string())?;
        budget.files += 1;
        budget.bytes = budget.bytes.saturating_add(contents.len() as u64);
    }
    Ok(())
}


/// Restrict the private snapshot workspace to the current user and the
/// operating-system principals required for administration and recovery.
fn secure_private_workspace_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "FRIDAY could not inspect its private workspace directory.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The private workspace is not an ordinary directory.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
        let directory = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "FRIDAY could not securely open its private workspace directory.".to_string())?;
        let opened = directory.metadata().map_err(|_| "FRIDAY could not verify its private workspace directory.".to_string())?;
        if !opened.is_dir() || is_reparse_or_symlink(&opened) {
            return Err("The private workspace changed type during permission setup.".into());
        }
        if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
            return Err("FRIDAY could not restrict private workspace permissions.".into());
        }
        let verified = directory.metadata().map_err(|_| "FRIDAY could not verify private workspace permissions.".to_string())?;
        use std::os::unix::fs::PermissionsExt;
        if verified.permissions().mode() & 0o077 != 0 {
            return Err("Private workspace permissions are broader than owner-only.".into());
        }
    }
    #[cfg(windows)]
    {
        let system_root = env::var_os("SystemRoot")
            .ok_or_else(|| "Windows system directory is unavailable; private workspace ACL setup was refused.".to_string())?;
        let system32 = PathBuf::from(system_root).join("System32");
        let identity = Command::new(system32.join("whoami.exe"))
            .args(["/user", "/fo", "csv", "/nh"]).output()
            .map_err(|_| "FRIDAY could not identify the current Windows account for workspace ACL setup.".to_string())?;
        if !identity.status.success() {
            return Err("Windows account identity lookup failed; private workspace ACL setup was refused.".into());
        }
        let identity_text = String::from_utf8(identity.stdout)
            .map_err(|_| "Windows account identity output was not valid UTF-8.".to_string())?;
        let sid = identity_text.lines().filter_map(|line| line.split(',').last())
            .map(|value| value.trim().trim_matches('"'))
            .find(|value| value.starts_with("S-1-") && value.len() <= 184)
            .ok_or_else(|| "FRIDAY could not validate the current Windows account SID.".to_string())?;
        let script = r#"$ErrorActionPreference = 'Stop'
$path = [System.Environment]::GetEnvironmentVariable('FRIDAY_WORKSPACE_PATH')
$currentSid = [System.Environment]::GetEnvironmentVariable('FRIDAY_WORKSPACE_SID')
if ([string]::IsNullOrWhiteSpace($path) -or [string]::IsNullOrWhiteSpace($currentSid)) {
  throw 'ACL_INPUT_MISSING'
}
$allowed = @($currentSid, 'S-1-5-18', 'S-1-5-32-544')
try {
  # Use the .NET filesystem ACL APIs directly. Keep stage-specific diagnostics
  # so Windows CI identifies the exact failing ACL operation.
  # A protected SDDL descriptor avoids ambiguous rule-merging and purge behavior.
  $sddl = "D:P(A;OICI;FA;;;$currentSid)(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)"
  # Windows PowerShell 5.1 exposes ACL methods on DirectoryInfo instances.
  $directory = [System.IO.DirectoryInfo]::new($path)
  $stage = 'read-acl'
  $acl = $directory.GetAccessControl()
  $stage = 'apply-sddl'
  $acl.SetSecurityDescriptorSddlForm($sddl)
  $stage = 'write-acl'
  $directory.SetAccessControl($acl)
  $stage = 'verify-acl'
  $check = $directory.GetAccessControl()
  if (-not $check.AreAccessRulesProtected) { throw 'ACL_INHERITANCE' }
  $rules = @($check.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
  if ($rules.Count -ne 3) { throw 'ACL_RULE_COUNT' }
  foreach ($rule in $rules) {
    if ($rule.IsInherited -or $rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow -or $rule.FileSystemRights -ne [System.Security.AccessControl.FileSystemRights]::FullControl -or $allowed -notcontains $rule.IdentityReference.Value) {
      throw 'ACL_RULE_CONTENT'
    }
  }
} catch {
  $message = ($_.Exception.Message -replace '[\r\n]+', ' ').Trim()
  [Console]::Error.WriteLine('PRIVATE_WORKSPACE_ACL_FAILED:' + $stage + ':' + $_.Exception.GetType().FullName + ':' + $message + ':' + $_.FullyQualifiedErrorId)
  exit 1
}
"#;
        let powershell = system32.join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        let applied = Command::new(powershell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            // Pass values through the child process environment. PowerShell's
            // -Command argument parsing does not reliably populate $args for
            // this invocation shape, and paths may contain spaces or Unicode.
            .env("FRIDAY_WORKSPACE_PATH", path.as_os_str())
            .env("FRIDAY_WORKSPACE_SID", sid)
            .output()
            .map_err(|_| "FRIDAY could not apply restrictive Windows workspace permissions.".to_string())?;
        if !applied.status.success() {
            let diagnostic = String::from_utf8_lossy(&applied.stderr);
            let diagnostic = diagnostic.lines().find(|line| line.contains("PRIVATE_WORKSPACE_ACL_FAILED:"))
                .unwrap_or("PRIVATE_WORKSPACE_ACL_FAILED:UNKNOWN");
            return Err(format!("Windows workspace ACL could not be rebuilt and verified; snapshot operation was stopped ({diagnostic})."));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn coding_sandbox_prepare_snapshot(app: AppHandle) -> Result<CodingSandboxSnapshot, String> {
    let source_root = crate::coding_assistant::selected_project_root_for_sandbox()?;
    let root_metadata = fs::symlink_metadata(&source_root)
        .map_err(|_| "The selected project folder is no longer available.".to_string())?;
    if !root_metadata.is_dir() || is_reparse_or_symlink(&root_metadata) {
        return Err("The selected project folder is not a safe normal directory.".into());
    }

    let app_data = app.path().app_local_data_dir()
        .map_err(|_| "FRIDAY could not locate its private application data folder.".to_string())?;
    let workspace_root = app_data.join("coding-sandbox-workspaces");
    fs::create_dir_all(&workspace_root)
        .map_err(|_| "FRIDAY could not create private sandbox workspace storage.".to_string())?;
    secure_private_workspace_directory(&workspace_root)?;
    let workspace_meta = fs::symlink_metadata(&workspace_root)
        .map_err(|_| "FRIDAY could not verify private sandbox workspace storage.".to_string())?;
    if !workspace_meta.is_dir() || is_reparse_or_symlink(&workspace_meta) {
        return Err("Private sandbox workspace storage is not a safe normal directory.".into());
    }

    let mut random = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    let snapshot_id = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let snapshot_path = workspace_root.join(&snapshot_id);

    // Production snapshots are collected and written through held directory
    // handles. The schema-2 inventory is created last, only after every bounded
    // payload file has been written and flushed successfully.
    let (files_copied, bytes_copied) =
        crate::coding_sandbox_fs::prepare_snapshot_from_paths(
            &source_root,
            &workspace_root,
            &snapshot_id,
            (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
            safe_project_file_name,
            excluded_directory,
        ).map_err(|_| "FRIDAY could not safely create a complete isolated project snapshot. Any incomplete, unmarked data is preserved for manual review.".to_string())?;
    if files_copied == 0 {
        return Err("No eligible source or project-manifest files were found to snapshot.".into());
    }

    OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?
        .insert(snapshot_id.clone(), snapshot_path);
    Ok(CodingSandboxSnapshot {
        snapshot_id,
        files_copied,
        bytes_copied,
        truncated: false,
        message: "A complete bounded schema-2 snapshot was prepared through verified directory handles. The original project was not changed. Execution remains disabled.".into(),
    })
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn build_sandbox_config(snapshot_path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(snapshot_path)
        .map_err(|_| "The isolated snapshot is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The snapshot is not a safe normal directory.".into());
    }
    let canonical = fs::canonicalize(snapshot_path)
        .map_err(|_| "FRIDAY could not verify the snapshot path.".to_string())?;
    if canonical.as_path() != snapshot_path {
        return Err("The snapshot path changed during validation.".into());
    }
    let host_folder = canonical.to_str()
        .ok_or_else(|| "The snapshot path cannot be represented safely in Windows Sandbox configuration.".to_string())?;
    if !canonical.is_absolute() || canonical.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err("The snapshot path is not a safe absolute path.".into());
    }
    let escaped = xml_escape(host_folder);
    let config = format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{escaped}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#);
    validate_sandbox_config_policy(&config)?;
    Ok(config)
}

/// Strict validator for FRIDAY's one generated Windows Sandbox template.
/// This intentionally accepts only the exact template grammar, not arbitrary XML.
/// It does not prove that the Windows host enforces the settings.
fn validate_sandbox_config_policy(config: &str) -> Result<(), String> {
    const PREFIX: &str = r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>"#;
    const SUFFIX: &str = r#"</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#;

    let Some(encoded_host_path) = config.strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
    else {
        return Err("The Windows Sandbox configuration does not match FRIDAY's exact isolation template; launch is refused.".into());
    };
    if encoded_host_path.is_empty() {
        return Err("The Windows Sandbox host folder is empty; launch is refused.".into());
    }

    // Only the five named entities emitted by xml_escape are accepted. Decode
    // and re-encode to reject malformed, unknown, or raw entity syntax.
    let decoded = decode_xml_text(encoded_host_path)
        .ok_or_else(|| "The Windows Sandbox host folder contains invalid XML text; launch is refused.".to_string())?;
    if decoded.is_empty() || decoded.chars().any(char::is_control)
        || xml_escape(&decoded) != encoded_host_path
    {
        return Err("The Windows Sandbox host folder is not safely XML-escaped; launch is refused.".into());
    }
    Ok(())
}



/// Builds the exact two-folder Sandbox configuration only after the caller has
/// independently verified the mounted, bounded FRIDAY result volume.
/// The snapshot is read-only; the result mapping is writable and must point
/// only at the fixed per-run mount directory on the 64 MiB result VHDX.
fn build_sandbox_config_with_result_paths(
    snapshot_path: &Path,
    result_mount_path: &Path,
) -> Result<String, String> {
    let metadata = fs::symlink_metadata(snapshot_path)
        .map_err(|_| "The isolated snapshot is unavailable.".to_string())?;
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        return Err("The snapshot is not a safe normal directory.".into());
    }
    let canonical_snapshot = fs::canonicalize(snapshot_path)
        .map_err(|_| "FRIDAY could not verify the snapshot path.".to_string())?;
    if canonical_snapshot.as_path() != snapshot_path
        || !canonical_snapshot.is_absolute()
        || canonical_snapshot.components().any(|part| matches!(part, Component::ParentDir))
    {
        return Err("The snapshot path changed or is not a safe absolute path.".into());
    }
    // A mounted-volume path is a Windows mount-point reparse point. Do not
    // canonicalize it; the Windows wrapper below proves its exact volume GUID.
    if !result_mount_path.is_absolute()
        || result_mount_path.components().any(|part| matches!(part, Component::ParentDir))
    {
        return Err("The result mount path is not a safe absolute path.".into());
    }
    let snapshot_text = canonical_snapshot.to_str()
        .ok_or_else(|| "The snapshot path cannot be represented safely in Sandbox configuration.".to_string())?;
    let result_text = result_mount_path.to_str()
        .ok_or_else(|| "The result mount path cannot be represented safely in Sandbox configuration.".to_string())?;
    if snapshot_text.chars().any(char::is_control) || result_text.chars().any(char::is_control) {
        return Err("A Sandbox mapped-folder path contains unsupported control characters.".into());
    }
    let config = format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#, xml_escape(snapshot_text), xml_escape(result_text));
    validate_sandbox_config_with_result_policy(&config)?;
    Ok(config)
}

/// Windows-only production gateway: a writable mapping is emitted only when
/// the durable journal says Mounted and the exact FRIDAY-owned attached VHDX,
/// its identity, and its volume-GUID mount point all verify.
#[cfg(windows)]
#[allow(dead_code)]
fn build_sandbox_config_for_mounted_result(
    snapshot_path: &Path,
    trusted_snapshot_workspace: &Path,
    expected_snapshot_id: &str,
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    current_journal: &GuestResultStorageJournal,
    expected_volume_path: &str,
) -> Result<String, String> {
    let verified_snapshot = crate::coding_sandbox_fs::verify_snapshot_for_sandbox_mapping(
        trusted_snapshot_workspace,
        expected_snapshot_id,
        (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
    ).map_err(|_| "The project snapshot failed FRIDAY's ownership-inventory preflight and cannot be shared with Sandbox.".to_string())?;
    if snapshot_path != verified_snapshot.as_path() {
        return Err("The requested Sandbox project mapping does not match the verified FRIDAY snapshot.".into());
    }
    let identifier = verify_attached_guest_result_vhdx_identity(
        &attached.handle, &attached.physical_path, plan, trusted_storage_root,
    )?;
    if current_journal.run_id != plan.run_id
        || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::Mounted
    {
        return Err("The writable Sandbox result mapping is not backed by the current verified Mounted journal state.".into());
    }
    verify_guest_result_volume_mount(plan, trusted_storage_root, expected_volume_path)?;
    if plan.host_mount_directory != plan.run_directory.join("result-mount") {
        return Err("The writable Sandbox mapping is not the fixed per-run result mount directory.".into());
    }
    build_sandbox_config_with_result_paths(snapshot_path, &plan.host_mount_directory)
}

/// Persist one already-validated Sandbox configuration as an exclusive, bounded file.
/// Existing or interrupted files are never overwritten; callers must preserve them
/// for recovery rather than attempting an in-place repair.
fn write_new_sandbox_config_file(
    private_run_directory: &Path,
    config: &str,
) -> Result<PathBuf, String> {
    if config.is_empty() || config.len() > MAX_SANDBOX_CONFIG_BYTES {
        return Err("The Sandbox configuration is empty or exceeds FRIDAY's fixed size limit.".into());
    }
    validate_sandbox_config_with_result_policy(config)?;
    let directory_metadata = fs::symlink_metadata(private_run_directory)
        .map_err(|_| "The private Sandbox run directory is unavailable.".to_string())?;
    if !directory_metadata.is_dir() || is_reparse_or_symlink(&directory_metadata) {
        return Err("The Sandbox configuration parent is not an ordinary private directory.".into());
    }
    let canonical_directory = fs::canonicalize(private_run_directory)
        .map_err(|_| "The private Sandbox run directory could not be verified.".to_string())?;
    // Windows may return an equivalent extended/case-normalized spelling from
    // canonicalize(); use that verified spelling rather than requiring textual
    // equality with the caller's path. Reinspect the canonical target itself so
    // a final-component reparse point is never accepted.
    let canonical_metadata = fs::symlink_metadata(&canonical_directory)
        .map_err(|_| "The canonical Sandbox run directory could not be inspected.".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_or_symlink(&canonical_metadata)
        || !canonical_directory.is_absolute()
        || canonical_directory.components().any(|part| matches!(part, Component::ParentDir))
    {
        return Err("The canonical Sandbox configuration parent is not a safe ordinary directory.".into());
    }

    let config_path = canonical_directory.join(SANDBOX_CONFIG_FILE_NAME);
    match fs::symlink_metadata(&config_path) {
        Ok(_) => return Err("A Sandbox configuration file already exists; reuse or overwrite is refused.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("The existing Sandbox configuration path could not be safely inspected.".into()),
    }

    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
        .map_err(|_| "FRIDAY could not exclusively create its Sandbox configuration file.".to_string())?;
    use std::io::Write;
    output.write_all(config.as_bytes())
        .map_err(|_| "FRIDAY could not completely write the Sandbox configuration; the partial file was preserved.".to_string())?;
    output.sync_all()
        .map_err(|_| "FRIDAY could not flush the Sandbox configuration; the file was preserved.".to_string())?;
    drop(output);

    let file_metadata = fs::symlink_metadata(&config_path)
        .map_err(|_| "The staged Sandbox configuration could not be inspected.".to_string())?;
    if !file_metadata.is_file() || is_reparse_or_symlink(&file_metadata)
        || file_metadata.len() != config.len() as u64
    {
        return Err("The staged Sandbox configuration is not the expected bounded regular file.".into());
    }
    let mut input = open_source_file(&config_path)
        .map_err(|_| "The staged Sandbox configuration could not be reopened without following links.".to_string())?;
    let opened_metadata = input.metadata()
        .map_err(|_| "The staged Sandbox configuration identity could not be verified.".to_string())?;
    if !opened_metadata.is_file() || is_reparse_or_symlink(&opened_metadata)
        || opened_metadata.len() != config.len() as u64
    {
        return Err("The staged Sandbox configuration changed before read-back verification.".into());
    }
    let mut read_back = Vec::with_capacity(config.len());
    input.take((MAX_SANDBOX_CONFIG_BYTES + 1) as u64).read_to_end(&mut read_back)
        .map_err(|_| "The staged Sandbox configuration could not be read back.".to_string())?;
    if read_back.len() != config.len() || read_back.as_slice() != config.as_bytes() {
        return Err("The staged Sandbox configuration failed exact read-back verification.".into());
    }
    let verified_text = std::str::from_utf8(&read_back)
        .map_err(|_| "The staged Sandbox configuration is not valid UTF-8.".to_string())?;
    validate_sandbox_config_with_result_policy(verified_text)?;
    let final_parent = fs::canonicalize(config_path.parent().unwrap_or(Path::new("")))
        .map_err(|_| "The staged Sandbox configuration parent could not be reverified.".to_string())?;
    if final_parent != canonical_directory
        || config_path.file_name() != Some(OsStr::new(SANDBOX_CONFIG_FILE_NAME))
    {
        return Err("The staged Sandbox configuration escaped its exact FRIDAY run directory.".into());
    }
    Ok(config_path)
}

/// Windows-only staging gateway. It revalidates the owned result run directory,
/// reapplies its protected ACL, and obtains the exact mounted two-folder policy
/// before creating sandbox.wsb. This does not launch Windows Sandbox.
#[cfg(windows)]
#[allow(dead_code)]
fn stage_sandbox_config_for_mounted_result(
    snapshot_path: &Path,
    trusted_snapshot_workspace: &Path,
    expected_snapshot_id: &str,
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    current_journal: &GuestResultStorageJournal,
    expected_volume_path: &str,
) -> Result<PathBuf, String> {
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    secure_private_workspace_directory(&plan.run_directory)?;
    verify_guest_result_volume_directory(plan, trusted_storage_root)?;
    let config = build_sandbox_config_for_mounted_result(
        snapshot_path,
        trusted_snapshot_workspace,
        expected_snapshot_id,
        attached,
        plan,
        trusted_storage_root,
        current_journal,
        expected_volume_path,
    )?;
    write_new_sandbox_config_file(&plan.run_directory, &config)
}

/// Exact two-mapping template validator. Arbitrary XML and arbitrary writable
/// host paths are not accepted; path text is decoded and round-trip checked.
fn validate_sandbox_config_with_result_policy(config: &str) -> Result<(), String> {
    const PREFIX: &str = r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>"#;
    const BETWEEN: &str = r#"</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>"#;
    const SUFFIX: &str = r#"</HostFolder>
      <SandboxFolder>C:\FRIDAY\Results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#;
    let Some(body) = config.strip_prefix(PREFIX).and_then(|rest| rest.strip_suffix(SUFFIX)) else {
        return Err("The two-folder Windows Sandbox configuration does not match FRIDAY's exact isolation template.".into());
    };
    let Some((snapshot_encoded, result_encoded)) = body.split_once(BETWEEN) else {
        return Err("The Sandbox configuration must contain exactly one read-only project mapping followed by one result mapping.".into());
    };
    for encoded in [snapshot_encoded, result_encoded] {
        if encoded.is_empty() {
            return Err("A Sandbox mapped-folder host path is empty.".into());
        }
        let decoded = decode_xml_text(encoded)
            .ok_or_else(|| "A Sandbox mapped-folder path contains invalid XML text.".to_string())?;
        if decoded.is_empty() || decoded.chars().any(char::is_control) || xml_escape(&decoded) != encoded {
            return Err("A Sandbox mapped-folder path is not safely XML-escaped.".into());
        }
    }
    Ok(())
}

fn decode_xml_text(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        rest = &rest[index..];
        let (entity, replacement) = if rest.starts_with("&amp;") {
            ("&amp;", "&")
        } else if rest.starts_with("&lt;") {
            ("&lt;", "<")
        } else if rest.starts_with("&gt;") {
            ("&gt;", ">")
        } else if rest.starts_with("&quot;") {
            ("&quot;", "\"")
        } else if rest.starts_with("&apos;") {
            ("&apos;", "'")
        } else {
            return None;
        };
        decoded.push_str(replacement);
        rest = &rest[entity.len()..];
    }
    decoded.push_str(rest);
    Some(decoded)
}

#[tauri::command]
pub async fn coding_sandbox_recover_workspaces(app: AppHandle) -> Result<CodingSandboxRecovery, String> {
    let app_data = app.path().app_local_data_dir()
        .map_err(|_| "FRIDAY could not locate its private application data folder.".to_string())?;
    let workspace_root = app_data.join("coding-sandbox-workspaces");
    let root_meta = match fs::symlink_metadata(&workspace_root) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodingSandboxRecovery {
                removed_snapshots: 0,
                preserved_unverified_entries: 0,
                message: "No previous Coding Assistant sandbox workspaces were found.".into(),
            });
        }
        Err(_) => return Err("FRIDAY could not inspect its private sandbox workspace.".into()),
    };
    if !root_meta.is_dir() || is_reparse_or_symlink(&root_meta) {
        return Err("The private sandbox workspace is not a normal directory; recovery was refused.".into());
    }
    // Re-assert the private ACL before enumerating old workspace entries.
    secure_private_workspace_directory(&workspace_root)?;
    let entries = fs::read_dir(&workspace_root)
        .map_err(|_| "FRIDAY could not list its private sandbox workspace.".to_string())?;
    let mut removed = 0usize;
    let mut preserved = 0usize;
    for entry in entries {
        let Ok(entry) = entry else { preserved += 1; continue; };
        let entry_name = entry.file_name();
        let Some(name) = entry_name.to_str().map(str::to_string) else { preserved += 1; continue; };
        if !valid_snapshot_id(&name) {
            preserved += 1;
            continue;
        }
        // A component remount must not delete a snapshot still owned by this process.
        let current_session_owns_it = OWNED_SNAPSHOTS.lock()
            .map(|owned| owned.contains_key(&name))
            .unwrap_or(true);
        if current_session_owns_it {
            preserved += 1;
            continue;
        }
        match crate::coding_sandbox_fs::cleanup_snapshot_from_workspace_path(
            &workspace_root,
            &name,
            (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
        ) {
            Ok(()) => {
                removed += 1;
                if let Ok(mut owned) = OWNED_SNAPSHOTS.lock() { owned.remove(&name); }
            }
            Err(_) => preserved += 1,
        }
    }
    Ok(CodingSandboxRecovery {
        removed_snapshots: removed,
        preserved_unverified_entries: preserved,
        message: format!("Recovery removed {removed} verified FRIDAY-owned snapshot(s) and preserved {preserved} unverified or unknown entrie(s). Original project folders are never scanned or deleted."),
    })
}

#[tauri::command]
pub async fn coding_sandbox_config_preview(snapshot_id: String) -> Result<String, String> {
    if !valid_snapshot_id(&snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let owned = OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?;
    let path = owned.get(&snapshot_id)
        .ok_or_else(|| "This snapshot is not owned by the current FRIDAY session.".to_string())?;
    build_sandbox_config(path)
}

#[tauri::command]
pub async fn coding_sandbox_discard_snapshot(snapshot_id: String) -> Result<String, String> {
    if !valid_snapshot_id(&snapshot_id) {
        return Err("The snapshot identifier is invalid.".into());
    }
    let mut owned = OWNED_SNAPSHOTS.lock()
        .map_err(|_| "Snapshot ownership tracking is temporarily unavailable.".to_string())?;
    let path = owned.get(&snapshot_id).cloned()
        .ok_or_else(|| "This snapshot is not owned by the current FRIDAY session.".to_string())?;
    let parent = path.parent()
        .ok_or_else(|| "The snapshot folder has no verified parent.".to_string())?;
    crate::coding_sandbox_fs::cleanup_snapshot_from_workspace_path(
        parent,
        &snapshot_id,
        (MAX_SNAPSHOT_DEPTH, MAX_SNAPSHOT_FILES, MAX_SNAPSHOT_FILE_BYTES, MAX_SNAPSHOT_TOTAL_BYTES),
    ).map_err(|_| "The snapshot could not be safely verified and removed. Its data has been preserved.".to_string())?;
    owned.remove(&snapshot_id);
    Ok("The isolated snapshot copy was removed. The original project was not changed.".into())
}


const GUEST_RESULT_GPT_SECTOR_BYTES: u64 = 512;
const GUEST_RESULT_GPT_ALIGNMENT_SECTORS: u64 = 2048;
const GUEST_RESULT_GPT_METADATA_SECTORS: u64 = 34;
const GUEST_RESULT_PARTITION_TYPE_GUID: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";
const GUEST_RESULT_FILESYSTEM_LABEL: &str = "FRIDAY_RESULTS";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultPartitionPlan {
    run_id: String,
    disk_capacity_bytes: u64,
    logical_sector_bytes: u32,
    partition_number: u32,
    first_lba: u64,
    last_lba: u64,
    partition_type_guid: String,
    partition_unique_guid: String,
    filesystem: String,
    filesystem_label: String,
}

fn build_guest_result_partition_plan(
    run_id: &str,
    disk_capacity_bytes: u64,
    logical_sector_bytes: u32,
) -> Result<GuestResultPartitionPlan, String> {
    if !valid_snapshot_id(run_id)
        || disk_capacity_bytes != GUEST_RESULT_VOLUME_CAPACITY_BYTES
        || logical_sector_bytes as u64 != GUEST_RESULT_GPT_SECTOR_BYTES
        || disk_capacity_bytes % GUEST_RESULT_GPT_SECTOR_BYTES != 0
    {
        return Err("The attached result disk does not match FRIDAY's fixed GPT layout policy.".into());
    }
    let total_sectors = disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    if total_sectors <= GUEST_RESULT_GPT_METADATA_SECTORS * 2 {
        return Err("The result disk is too small for a safe GPT layout.".into());
    }
    let first_lba = GUEST_RESULT_GPT_ALIGNMENT_SECTORS;
    let last_lba = total_sectors - GUEST_RESULT_GPT_METADATA_SECTORS - 1;
    if first_lba > last_lba {
        return Err("The result disk has no usable aligned GPT partition range.".into());
    }
    // Bind the unique partition GUID to the already-generated per-run identifier.
    let compact = run_id.to_ascii_lowercase();
    let partition_unique_guid = format!(
        "{}-{}-{}-{}-{}",
        &compact[0..8], &compact[8..12], &compact[12..16],
        &compact[16..20], &compact[20..32]
    ).to_ascii_uppercase();
    Ok(GuestResultPartitionPlan {
        run_id: run_id.to_string(),
        disk_capacity_bytes,
        logical_sector_bytes,
        partition_number: 1,
        first_lba,
        last_lba,
        partition_type_guid: GUEST_RESULT_PARTITION_TYPE_GUID.into(),
        partition_unique_guid,
        filesystem: "NTFS".into(),
        filesystem_label: GUEST_RESULT_FILESYSTEM_LABEL.into(),
    })
}

fn validate_guest_result_partition_plan(
    plan: &GuestResultPartitionPlan,
    expected_run_id: &str,
) -> Result<(), String> {
    let expected = build_guest_result_partition_plan(
        expected_run_id,
        GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        GUEST_RESULT_GPT_SECTOR_BYTES as u32,
    )?;
    if plan != &expected {
        return Err("The result partition plan differs from FRIDAY's fixed per-run GPT and NTFS policy.".into());
    }
    Ok(())
}


const GUEST_RESULT_GPT_HEADER_BYTES: usize = 92;
const GUEST_RESULT_GPT_ENTRY_COUNT: usize = 128;
const GUEST_RESULT_GPT_ENTRY_BYTES: usize = 128;
const GUEST_RESULT_GPT_ARRAY_BYTES: usize = GUEST_RESULT_GPT_ENTRY_COUNT * GUEST_RESULT_GPT_ENTRY_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
struct GuestResultGptMetadata {
    protective_mbr: Vec<u8>,
    primary_header: Vec<u8>,
    primary_entries: Vec<u8>,
    backup_entries: Vec<u8>,
    backup_header: Vec<u8>,
    disk_guid: String,
}

fn parse_canonical_guid_bytes(value: &str) -> Result<[u8; 16], String> {
    let compact = value.replace('-', "");
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("A GPT identifier is not a canonical hexadecimal GUID.".into());
    }
    let mut raw = [0u8; 16];
    for (index, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16)
            .map_err(|_| "A GPT identifier contains invalid hexadecimal data.".to_string())?;
    }
    // GPT stores GUID fields using the Windows/UEFI mixed-endian GUID layout.
    raw[..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    Ok(raw)
}

fn gpt_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn write_gpt_header(
    sector: &mut [u8],
    current_lba: u64,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: &[u8; 16],
    entries_lba: u64,
    entries_crc: u32,
) -> Result<(), String> {
    if sector.len() != GUEST_RESULT_GPT_SECTOR_BYTES as usize {
        return Err("The GPT header buffer does not match the fixed logical sector size.".into());
    }
    sector.fill(0);
    sector[0..8].copy_from_slice(b"EFI PART");
    sector[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    sector[12..16].copy_from_slice(&(GUEST_RESULT_GPT_HEADER_BYTES as u32).to_le_bytes());
    sector[24..32].copy_from_slice(&current_lba.to_le_bytes());
    sector[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    sector[40..48].copy_from_slice(&first_usable_lba.to_le_bytes());
    sector[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    sector[56..72].copy_from_slice(disk_guid);
    sector[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    sector[80..84].copy_from_slice(&(GUEST_RESULT_GPT_ENTRY_COUNT as u32).to_le_bytes());
    sector[84..88].copy_from_slice(&(GUEST_RESULT_GPT_ENTRY_BYTES as u32).to_le_bytes());
    sector[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = gpt_crc32(&sector[..GUEST_RESULT_GPT_HEADER_BYTES]);
    sector[16..20].copy_from_slice(&header_crc.to_le_bytes());
    Ok(())
}

fn build_guest_result_gpt_metadata(
    plan: &GuestResultPartitionPlan,
) -> Result<GuestResultGptMetadata, String> {
    validate_guest_result_partition_plan(plan, &plan.run_id)?;
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_header_lba = total_sectors - 1;
    let backup_entries_lba = backup_header_lba - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    if plan.first_lba < GUEST_RESULT_GPT_ALIGNMENT_SECTORS
        || plan.last_lba >= backup_entries_lba
        || backup_entries_lba <= plan.last_lba
    {
        return Err("The planned partition overlaps reserved GPT metadata.".into());
    }

    let partition_type = parse_canonical_guid_bytes(&plan.partition_type_guid)?;
    let partition_id = parse_canonical_guid_bytes(&plan.partition_unique_guid)?;
    let compact_run = plan.run_id.to_ascii_lowercase();
    let mut disk_compact = compact_run.clone().into_bytes();
    disk_compact[0] = if disk_compact[0] == b'0' { b'1' } else { b'0' };
    let disk_guid_text = format!(
        "{}-{}-{}-{}-{}",
        String::from_utf8_lossy(&disk_compact[0..8]),
        String::from_utf8_lossy(&disk_compact[8..12]),
        String::from_utf8_lossy(&disk_compact[12..16]),
        String::from_utf8_lossy(&disk_compact[16..20]),
        String::from_utf8_lossy(&disk_compact[20..32]),
    ).to_ascii_uppercase();
    let disk_guid = parse_canonical_guid_bytes(&disk_guid_text)?;
    if disk_guid == partition_id || disk_guid.iter().all(|byte| *byte == 0) {
        return Err("The generated GPT disk identifier is not unique.".into());
    }

    let mut entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let entry = &mut entries[..GUEST_RESULT_GPT_ENTRY_BYTES];
    entry[0..16].copy_from_slice(&partition_type);
    entry[16..32].copy_from_slice(&partition_id);
    entry[32..40].copy_from_slice(&plan.first_lba.to_le_bytes());
    entry[40..48].copy_from_slice(&plan.last_lba.to_le_bytes());
    let name: Vec<u16> = "FRIDAY_RESULTS".encode_utf16().collect();
    for (index, unit) in name.iter().enumerate() {
        let offset = 56 + index * 2;
        entry[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
    }
    let entries_crc = gpt_crc32(&entries);
    let mut primary_header = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    let mut backup_header = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    // The final 32 sectors before the backup header hold the 128-entry
    // partition array. The last usable LBA is therefore total_sectors - 34.
    let last_usable_lba = total_sectors
        .checked_sub(GUEST_RESULT_GPT_METADATA_SECTORS)
        .ok_or_else(|| "The GPT last usable LBA calculation underflowed.".to_string())?;
    write_gpt_header(
        &mut primary_header, 1, backup_header_lba, GUEST_RESULT_GPT_METADATA_SECTORS,
        last_usable_lba, &disk_guid, 2, entries_crc,
    )?;
    write_gpt_header(
        &mut backup_header, backup_header_lba, 1, GUEST_RESULT_GPT_METADATA_SECTORS,
        last_usable_lba, &disk_guid, backup_entries_lba, entries_crc,
    )?;

    let mut protective_mbr = vec![0u8; GUEST_RESULT_GPT_SECTOR_BYTES as usize];
    let mbr_entry = &mut protective_mbr[446..462];
    mbr_entry[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr_entry[4] = 0xee;
    mbr_entry[5..8].copy_from_slice(&[0xff, 0xff, 0xff]);
    mbr_entry[8..12].copy_from_slice(&1u32.to_le_bytes());
    let protective_sectors = (total_sectors - 1).min(u32::MAX as u64) as u32;
    mbr_entry[12..16].copy_from_slice(&protective_sectors.to_le_bytes());
    protective_mbr[510] = 0x55;
    protective_mbr[511] = 0xaa;

    Ok(GuestResultGptMetadata {
        protective_mbr,
        primary_header,
        primary_entries: entries.clone(),
        backup_entries: entries,
        backup_header,
        disk_guid: disk_guid_text,
    })
}


#[cfg(windows)]
#[repr(C)]
struct FridayDiskGeometry {
    cylinders: i64,
    media_type: u32,
    tracks_per_cylinder: u32,
    sectors_per_track: u32,
    bytes_per_sector: u32,
}

#[cfg(windows)]
#[repr(C)]
struct FridayDiskGeometryEx {
    geometry: FridayDiskGeometry,
    disk_size: i64,
    data: [u8; 1],
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const std::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn DeviceIoControl(
        device: *mut std::ffi::c_void,
        control_code: u32,
        input_buffer: *const std::ffi::c_void,
        input_buffer_size: u32,
        output_buffer: *mut std::ffi::c_void,
        output_buffer_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn ReadFile(
        file: *mut std::ffi::c_void,
        buffer: *mut u8,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn WriteFile(
        file: *mut std::ffi::c_void,
        buffer: *const u8,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn SetFilePointerEx(
        file: *mut std::ffi::c_void,
        distance: i64,
        new_position: *mut i64,
        move_method: u32,
    ) -> i32;
    fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
struct OwnedRawGuestDisk(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for OwnedRawGuestDisk {
    fn drop(&mut self) {
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0); }
            self.0 = std::ptr::null_mut();
        }
    }
}

#[cfg(windows)]
impl OwnedRawGuestDisk {
    fn open_attached_device(attached: &OwnedAttachedGuestVhdx) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let current_path = guest_vhdx_physical_path(attached.handle.0)?;
        if current_path != attached.physical_path {
            return Err("The attached VHDX physical-device identity changed; raw disk access was refused.".into());
        }
        let wide: Vec<u16> = std::ffi::OsStr::new(&current_path)
            .encode_wide().chain(std::iter::once(0)).collect();
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const OPEN_EXISTING: u32 = 3;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(), GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(),
                OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err("Windows refused raw access to FRIDAY's exact attached result disk.".into());
        }
        let owned = Self(handle);
        owned.verify_fixed_geometry()?;
        Ok(owned)
    }

    fn verify_windows_partition_layout(
        &self,
        plan: &GuestResultPartitionPlan,
        metadata: &GuestResultGptMetadata,
    ) -> Result<(), String> {
        const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;
        const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
        // Raw GPT writes bypass the partition manager. Ask Windows to discard
        // any cached layout before requesting its independent interpretation.
        let mut update_returned = 0u32;
        let refreshed = unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_DISK_UPDATE_PROPERTIES,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut update_returned,
                std::ptr::null_mut(),
            )
        };
        if refreshed == 0 {
            return Err("Windows could not refresh the attached result disk's cached partition properties.".into());
        }
        let mut output = vec![0u64; 8192];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                std::ptr::null(),
                0,
                output.as_mut_ptr() as *mut std::ffi::c_void,
                (output.len() * std::mem::size_of::<u64>()) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err("Windows could not independently enumerate the result disk's partition layout.".into());
        }
        let returned = returned as usize;
        let output_bytes = unsafe {
            std::slice::from_raw_parts(output.as_ptr() as *const u8, output.len() * 8)
        };
        verify_guest_result_windows_partition_layout_bytes(output_bytes, returned, plan, metadata)
    }

    fn verify_fixed_geometry(&self) -> Result<(), String> {
        const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
        let mut output = [0u64; 8];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.0, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                std::ptr::null(), 0, output.as_mut_ptr() as *mut std::ffi::c_void,
                std::mem::size_of_val(&output) as u32, &mut returned, std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 32 {
            return Err("Windows could not verify the attached result disk geometry.".into());
        }
        let geometry = unsafe { &*(output.as_ptr() as *const FridayDiskGeometryEx) };
        if geometry.disk_size < 0
            || geometry.disk_size as u64 != GUEST_RESULT_VOLUME_CAPACITY_BYTES
            || geometry.geometry.bytes_per_sector as u64 != GUEST_RESULT_GPT_SECTOR_BYTES
        {
            return Err("The attached physical device does not match FRIDAY's exact 64 MiB / 512-byte disk policy.".into());
        }
        Ok(())
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        if buffer.is_empty() || buffer.len() > u32::MAX as usize || offset > i64::MAX as u64 {
            return Err("A raw result-disk read exceeded its fixed bounds.".into());
        }
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), 0) } == 0 {
            return Err("Windows could not position the result disk for verification.".into());
        }
        let mut read = 0u32;
        if unsafe { ReadFile(self.0, buffer.as_mut_ptr(), buffer.len() as u32, &mut read, std::ptr::null_mut()) } == 0
            || read as usize != buffer.len()
        {
            return Err("Windows returned an incomplete raw result-disk read.".into());
        }
        Ok(())
    }

    fn write_all_at(&self, offset: u64, buffer: &[u8]) -> Result<(), String> {
        if buffer.is_empty() || buffer.len() > u32::MAX as usize || offset > i64::MAX as u64 {
            return Err("A raw result-disk write exceeded its fixed bounds.".into());
        }
        if unsafe { SetFilePointerEx(self.0, offset as i64, std::ptr::null_mut(), 0) } == 0 {
            return Err("Windows could not position the result disk for a bounded write.".into());
        }
        let mut written = 0u32;
        if unsafe { WriteFile(self.0, buffer.as_ptr(), buffer.len() as u32, &mut written, std::ptr::null_mut()) } == 0
            || written as usize != buffer.len()
        {
            return Err("Windows returned an incomplete raw result-disk write; the disk must be preserved for recovery.".into());
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        if unsafe { FlushFileBuffers(self.0) } == 0 {
            return Err("Windows could not flush result-disk metadata; the disk must be preserved for recovery.".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn verify_zero_region(disk: &OwnedRawGuestDisk, first_lba: u64, sector_count: u64) -> Result<(), String> {
    let total_bytes = sector_count.checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The GPT blank-region size overflowed.".to_string())?;
    if total_bytes > GUEST_RESULT_VOLUME_CAPACITY_BYTES || total_bytes > 64 * 1024 {
        return Err("The GPT blank-region check exceeded its fixed bounded read size.".into());
    }
    let mut bytes = vec![0u8; total_bytes as usize];
    disk.read_exact_at(first_lba * GUEST_RESULT_GPT_SECTOR_BYTES, &mut bytes)?;
    if bytes.iter().any(|byte| *byte != 0) {
        return Err("The result disk already contains partition metadata or data; initialization was refused.".into());
    }
    Ok(())
}


const FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES: usize = 48;
const FRIDAY_PARTITION_INFORMATION_EX_BYTES: usize = 144;

/// Validates the layout reported by Windows' storage stack, independently of
/// the raw GPT-sector comparison. The byte offsets follow the documented
/// DRIVE_LAYOUT_INFORMATION_EX / PARTITION_INFORMATION_EX ABI.
fn verify_guest_result_windows_partition_layout_bytes(
    bytes: &[u8],
    returned_bytes: usize,
    plan: &GuestResultPartitionPlan,
    metadata: &GuestResultGptMetadata,
) -> Result<(), String> {
    let required = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES
        .checked_add(FRIDAY_PARTITION_INFORMATION_EX_BYTES)
        .ok_or_else(|| "The Windows partition-layout size overflowed.".to_string())?;
    if returned_bytes > bytes.len() || returned_bytes < required {
        return Err("Windows returned a truncated drive-layout structure.".into());
    }
    let data = &bytes[..returned_bytes];
    let read_u32 = |offset: usize| -> Result<u32, String> {
        let raw: [u8; 4] = data.get(offset..offset + 4)
            .ok_or_else(|| "Windows returned a truncated partition-layout field.".to_string())?
            .try_into().map_err(|_| "Windows returned an invalid partition-layout field.".to_string())?;
        Ok(u32::from_le_bytes(raw))
    };
    let read_u64 = |offset: usize| -> Result<u64, String> {
        let raw: [u8; 8] = data.get(offset..offset + 8)
            .ok_or_else(|| "Windows returned a truncated partition-layout field.".to_string())?
            .try_into().map_err(|_| "Windows returned an invalid partition-layout field.".to_string())?;
        Ok(u64::from_le_bytes(raw))
    };
    if read_u32(0)? != 1 || read_u32(4)? != 1 {
        return Err("Windows does not recognize exactly one GPT partition on the result disk.".into());
    }
    if data[8..24] != metadata.primary_header[56..72] {
        return Err("Windows reported a GPT disk identifier different from FRIDAY's recorded layout.".into());
    }
    // DRIVE_LAYOUT_INFORMATION_GPT reports the disk-wide GPT usable
    // interval (FirstUsableLBA=34 through LastUsableLBA=total-34), not the
    // aligned start/end of FRIDAY's individual partition. The partition entry
    // below is checked independently against the exact 1 MiB-aligned plan.
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let expected_usable_start = GUEST_RESULT_GPT_METADATA_SECTORS
        .checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The GPT usable-range start overflowed.".to_string())?;
    // First usable LBA is 34; last usable LBA is total_sectors - 34.
    // The inclusive count is total_sectors - 67.
    let expected_usable_length = total_sectors
        .checked_sub(GUEST_RESULT_GPT_METADATA_SECTORS * 2 - 1)
        .and_then(|sectors| sectors.checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES))
        .ok_or_else(|| "The GPT usable-range length overflowed.".to_string())?;
    let observed_usable_start = read_u64(24)?;
    let observed_usable_length = read_u64(32)?;
    let observed_max_partitions = read_u32(40)?;
    if observed_usable_start != expected_usable_start
        || observed_usable_length != expected_usable_length
        || observed_max_partitions != GUEST_RESULT_GPT_ENTRY_COUNT as u32
    {
        return Err(format!(
            "Windows reported an unexpected GPT usable range or partition capacity (start={observed_usable_start}, length={observed_usable_length}, max_partitions={observed_max_partitions}; expected start={expected_usable_start}, length={expected_usable_length}, max_partitions={}).",
            GUEST_RESULT_GPT_ENTRY_COUNT
        ));
    }

    let entry = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES;
    if read_u32(entry)? != 1
        || read_u64(entry + 8)? != plan.first_lba * GUEST_RESULT_GPT_SECTOR_BYTES
        || read_u64(entry + 16)? != (plan.last_lba - plan.first_lba + 1) * GUEST_RESULT_GPT_SECTOR_BYTES
        || read_u32(entry + 24)? != 1
        || data[entry + 28] != 0
        || data[entry + 29] != 0
        || data[entry + 32..entry + 48] != metadata.primary_entries[0..16]
        || data[entry + 48..entry + 64] != metadata.primary_entries[16..32]
        || read_u64(entry + 64)? != 0
    {
        return Err("Windows' partition entry differs from FRIDAY's exact partition type, identity, bounds, or attributes.".into());
    }
    let mut expected_name = [0u8; 72];
    for (index, unit) in "FRIDAY_RESULTS".encode_utf16().enumerate() {
        expected_name[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    if data[entry + 72..entry + 144] != expected_name {
        return Err("Windows reported an unexpected result-partition label.".into());
    }
    Ok(())
}

fn verify_gpt_header_crc(header: &[u8]) -> bool {
    if header.len() < GUEST_RESULT_GPT_HEADER_BYTES {
        return false;
    }
    let stored = u32::from_le_bytes(match header[16..20].try_into() {
        Ok(value) => value,
        Err(_) => return false,
    });
    let mut crc_input = header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
    crc_input[16..20].fill(0);
    gpt_crc32(&crc_input) == stored
}

#[cfg(windows)]
fn verify_guest_result_gpt_readback(
    disk: &OwnedRawGuestDisk,
    plan: &GuestResultPartitionPlan,
    expected: &GuestResultGptMetadata,
) -> Result<(), String> {
    let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_entries_lba = total_sectors - 1 - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    let mut mbr = vec![0u8; 512];
    let mut primary_header = vec![0u8; 512];
    let mut primary_entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let mut backup_entries = vec![0u8; GUEST_RESULT_GPT_ARRAY_BYTES];
    let mut backup_header = vec![0u8; 512];
    disk.read_exact_at(0, &mut mbr)?;
    disk.read_exact_at(512, &mut primary_header)?;
    disk.read_exact_at(2 * 512, &mut primary_entries)?;
    disk.read_exact_at(backup_entries_lba * 512, &mut backup_entries)?;
    disk.read_exact_at((total_sectors - 1) * 512, &mut backup_header)?;
    if mbr != expected.protective_mbr
        || primary_header != expected.primary_header
        || primary_entries != expected.primary_entries
        || backup_entries != expected.backup_entries
        || backup_header != expected.backup_header
        || gpt_crc32(&primary_entries) != u32::from_le_bytes(primary_header[88..92].try_into().unwrap())
        || gpt_crc32(&backup_entries) != u32::from_le_bytes(backup_header[88..92].try_into().unwrap())
        || !verify_gpt_header_crc(&primary_header)
        || !verify_gpt_header_crc(&backup_header)
    {
        return Err("The GPT metadata read-back differs from FRIDAY's exact layout or CRC policy.".into());
    }
    Ok(())
}

/// Initializes only the verified FRIDAY-owned attached VHDX. It refuses any
/// nonblank metadata region, journals intent before the first write, writes
/// backup structures before primary structures, flushes each phase, and
/// independently reads back every GPT structure before recording success.
#[cfg(windows)]
fn initialize_guest_result_gpt(
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    journal_root: &Path,
    current_journal: &GuestResultStorageJournal,
) -> Result<GuestResultStorageJournal, String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let partition_plan = build_guest_result_partition_plan(
        &plan.run_id, plan.capacity_bytes, GUEST_RESULT_GPT_SECTOR_BYTES as u32,
    )?;
    let metadata = build_guest_result_gpt_metadata(&partition_plan)?;
    let identifier = verify_guest_result_vhdx_identity(plan, trusted_storage_root)?;
    if current_journal.run_id != plan.run_id || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::Attached
    {
        return Err("The GPT initialization journal is not bound to this attached FRIDAY result disk.".into());
    }
    let disk = OwnedRawGuestDisk::open_attached_device(attached)?;
    verify_zero_region(&disk, 0, 34)?;
    let total_sectors = plan.capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
    let backup_entries_lba = total_sectors - 1 - (GUEST_RESULT_GPT_ARRAY_BYTES as u64 / GUEST_RESULT_GPT_SECTOR_BYTES);
    verify_zero_region(&disk, backup_entries_lba, 33)?;

    let in_progress = append_and_persist_guest_result_storage_transition(
        journal_root, current_journal, GuestResultStorageEvent::BeginGptInitialization, true, false,
    )?;
    let write_result = (|| -> Result<(), String> {
        disk.write_all_at(backup_entries_lba * 512, &metadata.backup_entries)?;
        disk.write_all_at((total_sectors - 1) * 512, &metadata.backup_header)?;
        disk.flush()?;
        disk.write_all_at(2 * 512, &metadata.primary_entries)?;
        disk.write_all_at(512, &metadata.primary_header)?;
        disk.flush()?;
        disk.write_all_at(0, &metadata.protective_mbr)?;
        disk.flush()?;
        verify_guest_result_gpt_readback(&disk, &partition_plan, &metadata)?;
        disk.verify_fixed_geometry()?;
        disk.verify_windows_partition_layout(&partition_plan, &metadata)?;
        if guest_vhdx_physical_path(attached.handle.0)? != attached.physical_path {
            return Err("The attached physical-device identity changed after GPT initialization.".into());
        }
        if verify_attached_guest_result_vhdx_identity(
            &attached.handle, &attached.physical_path, plan, trusted_storage_root,
        )? != identifier {
            return Err("The attached VHDX persistent identity changed after GPT initialization.".into());
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = append_and_persist_guest_result_storage_transition(
            journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, false,
        );
        return Err(format!("{error} The result disk has been preserved for recovery; no automatic retry will occur."));
    }
    append_and_persist_guest_result_storage_transition(
        journal_root, &in_progress, GuestResultStorageEvent::CompleteGptInitialization, true, false,
    )
}


/// Bounded report returned by the fixed Windows Storage-module formatting script.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuestResultNtfsFormatReport {
    disk_number: u32,
    disk_guid: String,
    partition_guid: String,
    partition_offset: u64,
    partition_size: u64,
    file_system: String,
    file_system_label: String,
    volume_path: String,
    volume_size: u64,
    size_remaining: u64,
    drive_letter: Option<String>,
}

fn valid_windows_volume_guid_path(path: &str) -> bool {
    const PREFIX: &str = r"\\?\Volume{";
    if !path.starts_with(PREFIX) || !path.ends_with(r"}\") {
        return false;
    }
    parse_canonical_guid_bytes(&path[PREFIX.len()..path.len() - 2]).is_ok()
}

fn validate_guest_result_ntfs_format_report(
    report: &GuestResultNtfsFormatReport,
    expected_disk_number: u32,
    metadata: &GuestResultGptMetadata,
    partition_plan: &GuestResultPartitionPlan,
) -> Result<(), String> {
    let expected_offset = partition_plan.first_lba.checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The expected NTFS partition offset overflowed.".to_string())?;
    let expected_size = (partition_plan.last_lba - partition_plan.first_lba + 1)
        .checked_mul(GUEST_RESULT_GPT_SECTOR_BYTES)
        .ok_or_else(|| "The expected NTFS partition size overflowed.".to_string())?;
    if report.disk_number != expected_disk_number
        || report.disk_guid.to_ascii_uppercase() != metadata.disk_guid.to_ascii_uppercase()
        || report.partition_guid.to_ascii_uppercase() != partition_plan.partition_unique_guid.to_ascii_uppercase()
        || report.partition_offset != expected_offset
        || report.partition_size != expected_size
        || !report.file_system.eq_ignore_ascii_case("NTFS")
        || report.file_system_label != GUEST_RESULT_FILESYSTEM_LABEL
        || !valid_windows_volume_guid_path(&report.volume_path)
        // NTFS reserves a small amount of partition space for filesystem metadata.
        // Keep the reported volume strictly within the exact partition and bound
        // that difference so a mismatched or unexpectedly smaller volume fails closed.
        || report.volume_size == 0
        || report.volume_size > expected_size
        || expected_size.saturating_sub(report.volume_size) > 1024 * 1024
        || report.size_remaining > report.volume_size
        || report.drive_letter.as_ref().is_some_and(|value| !value.trim().is_empty())
    {
        return Err(format!(
            "Windows' NTFS format report did not satisfy the fixed FRIDAY storage policy (disk={}, disk_guid={}, partition_guid={}, offset={}, partition_size={}, filesystem={}, label={}, volume_path={}, volume_size={}, remaining={}, drive_letter={:?}).",
            report.disk_number, report.disk_guid, report.partition_guid, report.partition_offset,
            report.partition_size, report.file_system, report.file_system_label, report.volume_path,
            report.volume_size, report.size_remaining, report.drive_letter
        ));
    }
    Ok(())
}

#[cfg(windows)]
const MAX_STORAGE_OPERATION_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(windows)]
const STORAGE_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(windows)]
fn read_bounded_storage_output<R: Read>(mut reader: R) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let remaining = MAX_STORAGE_OPERATION_OUTPUT_BYTES.saturating_sub(retained.len());
                retained.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    retained
}

#[cfg(windows)]
fn run_bounded_storage_operation(mut command: Command) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null())
        .spawn().map_err(|_| "The fixed Windows storage operation could not be started.".to_string())?;
    let stdout = child.stdout.take().ok_or_else(|| "The Windows storage output pipe was unavailable.".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "The Windows storage diagnostic pipe was unavailable.".to_string())?;
    let stdout_reader = thread::spawn(move || read_bounded_storage_output(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_storage_output(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < STORAGE_OPERATION_TIMEOUT => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill(); let _ = child.wait();
                let _ = stdout_reader.join(); let _ = stderr_reader.join();
                return Err("The NTFS format operation exceeded FRIDAY's fixed 120-second deadline; the result disk requires recovery.".into());
            }
            Err(_) => {
                let _ = child.kill(); let _ = child.wait();
                let _ = stdout_reader.join(); let _ = stderr_reader.join();
                return Err("FRIDAY could not supervise the NTFS format operation; the result disk requires recovery.".into());
            }
        }
    };
    let stdout = stdout_reader.join().map_err(|_| "The NTFS format output could not be collected.".to_string())?;
    let stderr = stderr_reader.join().map_err(|_| "The NTFS format diagnostics could not be collected.".to_string())?;
    Ok((status, stdout, stderr))
}

#[cfg(windows)]
fn format_guest_result_ntfs(
    attached: &OwnedAttachedGuestVhdx,
    plan: &GuestResultVolumePlan,
    trusted_storage_root: &Path,
    journal_root: &Path,
    current_journal: &GuestResultStorageJournal,
) -> Result<(GuestResultStorageJournal, String), String> {
    validate_guest_result_volume_plan(plan, trusted_storage_root, &plan.run_id)?;
    let partition_plan = build_guest_result_partition_plan(&plan.run_id, plan.capacity_bytes, GUEST_RESULT_GPT_SECTOR_BYTES as u32)?;
    let metadata = build_guest_result_gpt_metadata(&partition_plan)?;
    let identifier = verify_attached_guest_result_vhdx_identity(&attached.handle, &attached.physical_path, plan, trusted_storage_root)?;
    if current_journal.run_id != plan.run_id || current_journal.vhdx_identifier != identifier
        || validate_guest_result_storage_journal(current_journal)? != GuestResultStorageState::Partitioned {
        return Err("The NTFS format journal is not bound to this verified partitioned FRIDAY result disk.".into());
    }
    const PHYSICAL_DRIVE_PREFIX: &str = r"\\.\PhysicalDrive";
    let suffix = attached.physical_path.strip_prefix(PHYSICAL_DRIVE_PREFIX)
        .ok_or_else(|| "The attached result disk path is outside the PhysicalDrive namespace.".to_string())?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("The attached result disk number is invalid.".into());
    }
    let disk_number: u32 = suffix.parse().map_err(|_| "The attached result disk number is outside the supported range.".to_string())?;
    let in_progress = append_and_persist_guest_result_storage_transition(
        journal_root, current_journal, GuestResultStorageEvent::BeginNtfsFormat, true, false,
    )?;
    let script = r#"$ErrorActionPreference = 'Stop'
$diskNumber = [uint32]$env:FRIDAY_RESULT_DISK_NUMBER
$expectedDiskGuid = [guid]$env:FRIDAY_RESULT_DISK_GUID
$expectedPartitionGuid = [guid]$env:FRIDAY_RESULT_PARTITION_GUID
$expectedPartitionOffset = [uint64]$env:FRIDAY_RESULT_PARTITION_OFFSET
$expectedPartitionSize = [uint64]$env:FRIDAY_RESULT_PARTITION_SIZE
$expectedLabel = 'FRIDAY_RESULTS'
$disks = @(Get-Disk -Number $diskNumber -ErrorAction Stop)
if ($disks.Count -ne 1) { throw 'FRIDAY_DISK_CARDINALITY' }
$disk = $disks[0]
if ($disk.Number -ne $diskNumber -or [uint64]$disk.Size -ne 67108864 -or [string]$disk.PartitionStyle -ne 'GPT' -or
    [guid]$disk.Guid -ne $expectedDiskGuid -or [bool]$disk.IsBoot -or [bool]$disk.IsSystem -or
    [bool]$disk.IsReadOnly -or [bool]$disk.IsOffline) { throw 'FRIDAY_DISK_IDENTITY_OR_STATE_MISMATCH' }
$partitions = @(Get-Partition -DiskNumber $diskNumber -ErrorAction Stop)
if ($partitions.Count -ne 1) { throw 'FRIDAY_PARTITION_CARDINALITY' }
$partition = $partitions[0]
$partitionDriveLetter = ([string]$partition.DriveLetter).Trim([char]0)
$actualPartition = [pscustomobject]@{
  number = [uint32]$partition.PartitionNumber
  guid = [string]$partition.Guid
  offset = [uint64]$partition.Offset
  size = [uint64]$partition.Size
  gptType = [string]$partition.GptType
  isReadOnly = [bool]$partition.IsReadOnly
  isOffline = [bool]$partition.IsOffline
  driveLetter = if ([string]::IsNullOrEmpty($partitionDriveLetter)) { $null } else { $partitionDriveLetter }
}
if ($partition.PartitionNumber -ne 1 -or [guid]$partition.Guid -ne $expectedPartitionGuid -or
    [uint64]$partition.Offset -ne $expectedPartitionOffset -or [uint64]$partition.Size -ne $expectedPartitionSize -or
    [guid]$partition.GptType -ne [guid]'EBD0A0A2-B9E5-4433-87C0-68B6B72699C7' -or
    [bool]$partition.IsReadOnly -or [bool]$partition.IsOffline -or
    -not [string]::IsNullOrEmpty($partitionDriveLetter)) {
  throw ('FRIDAY_PARTITION_IDENTITY_OR_STATE_MISMATCH ' + ($actualPartition | ConvertTo-Json -Compress))
}
$formatted = @(Format-Volume -Partition $partition -FileSystem NTFS -NewFileSystemLabel $expectedLabel -AllocationUnitSize 4096 -Force:$false -Confirm:$false -ErrorAction Stop)
if ($formatted.Count -ne 1) { throw 'FRIDAY_FORMAT_RESULT_CARDINALITY' }
$volume = Get-Volume -Partition $partition -ErrorAction Stop
if ($null -eq $volume) { throw 'FRIDAY_VOLUME_NOT_DISCOVERED' }
$volumePath = [string]$volume.Path
$volumeDriveLetter = ([string]$volume.DriveLetter).Trim([char]0)
$driveLetter = if ([string]::IsNullOrEmpty($volumeDriveLetter)) { $null } else { $volumeDriveLetter }
[pscustomobject]@{
  diskNumber = [uint32]$disk.Number
  diskGuid = ([guid]$disk.Guid).ToString('D')
  partitionGuid = ([guid]$partition.Guid).ToString('D')
  partitionOffset = [uint64]$partition.Offset
  partitionSize = [uint64]$partition.Size
  fileSystem = [string]$volume.FileSystem
  fileSystemLabel = [string]$volume.FileSystemLabel
  volumePath = $volumePath
  volumeSize = [uint64]$volume.Size
  sizeRemaining = [uint64]$volume.SizeRemaining
  driveLetter = $driveLetter
} | ConvertTo-Json -Compress"#;
    let system_root = env::var_os("SystemRoot").ok_or_else(|| "The Windows system directory is unavailable for NTFS formatting.".to_string())?;
    let powershell = PathBuf::from(system_root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    if !powershell.is_absolute() || !powershell.is_file() {
        let _ = append_and_persist_guest_result_storage_transition(journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, false);
        return Err("The fixed Windows PowerShell executable is unavailable; the result disk was preserved for recovery.".into());
    }
    let mut command = Command::new(powershell);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script]);
    command.env("FRIDAY_RESULT_DISK_NUMBER", disk_number.to_string());
    command.env("FRIDAY_RESULT_DISK_GUID", &metadata.disk_guid);
    command.env("FRIDAY_RESULT_PARTITION_GUID", &partition_plan.partition_unique_guid);
    command.env("FRIDAY_RESULT_PARTITION_OFFSET", (partition_plan.first_lba * GUEST_RESULT_GPT_SECTOR_BYTES).to_string());
    command.env("FRIDAY_RESULT_PARTITION_SIZE", ((partition_plan.last_lba - partition_plan.first_lba + 1) * GUEST_RESULT_GPT_SECTOR_BYTES).to_string());
    let result = (|| -> Result<String, String> {
        let (status, stdout, stderr) = run_bounded_storage_operation(command)?;
        if !status.success() {
            let diagnostic = String::from_utf8_lossy(&stderr).chars().take(1200).collect::<String>();
            return Err(format!("Windows refused the fixed NTFS format operation (exit {}). {diagnostic}", status.code().unwrap_or(-1)));
        }
        if stdout.is_empty() || stdout.len() > MAX_STORAGE_OPERATION_OUTPUT_BYTES {
            return Err("Windows returned an empty or oversized NTFS format report.".into());
        }
        let report: GuestResultNtfsFormatReport = serde_json::from_slice(&stdout)
            .map_err(|_| "Windows returned malformed or unsupported NTFS format evidence.".to_string())?;
        validate_guest_result_ntfs_format_report(&report, disk_number, &metadata, &partition_plan)?;
        let volume_path = report.volume_path.clone();
        if guest_vhdx_physical_path(attached.handle.0)? != attached.physical_path
            || verify_attached_guest_result_vhdx_identity(&attached.handle, &attached.physical_path, plan, trusted_storage_root)? != identifier {
            return Err("The attached result disk identity changed during NTFS formatting.".into());
        }
        let raw_disk = OwnedRawGuestDisk::open_attached_device(attached)?;
        raw_disk.verify_fixed_geometry()?;
        raw_disk.verify_windows_partition_layout(&partition_plan, &metadata)?;
        Ok(volume_path)
    })();
    let volume_path = match result {
        Ok(volume_path) => volume_path,
        Err(error) => {
            let _ = append_and_persist_guest_result_storage_transition(journal_root, &in_progress, GuestResultStorageEvent::PreserveForRecovery, false, false);
            return Err(format!("{error} The result disk has been preserved for recovery; no automatic retry will occur."));
        }
    };
    let formatted_journal = append_and_persist_guest_result_storage_transition(
        journal_root, &in_progress, GuestResultStorageEvent::CompleteNtfsFormat, true, false,
    )?;
    Ok((formatted_journal, volume_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_directory(label: &str) -> PathBuf {
        let mut random = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut random);
        let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let path = env::temp_dir().join(format!("friday-sandbox-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("test directory should be created");
        path
    }


    fn guest_result_json(status: &str, exit_code: &str) -> Vec<u8> {
        format!(
            r#"{{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","status":"{status}"{exit_code},"stdout":"ok","stderr":"","truncated":false,"durationMs":25}}"#
        ).into_bytes()
    }


    fn valid_guest_request_json() -> Vec<u8> {
        br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":120000,"maxOutputBytes":1048576}"#.to_vec()
    }

    fn runtime_fixture_digest(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn runtime_tree_verification_requires_exact_manifest_file_and_directory_closure() {
        let path = make_test_directory("runtime-tree-exact");
        fs::create_dir_all(path.join("bin")).unwrap();
        let cargo = b"trusted cargo executable";
        let support = b"trusted runtime dependency";
        fs::write(path.join("bin").join("cargo.exe"), cargo).unwrap();
        fs::write(path.join("bin").join("support.dll"), support).unwrap();
        let root = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
        let manifest = GuestRuntimeArtifactManifest {
            schema_version: 1,
            operation: GuestRunOperation::RustCargoTest,
            artifacts: vec![
                GuestRuntimeArtifactSpec { relative_path: "bin\\cargo.exe".into(), sha256: runtime_fixture_digest(cargo), max_bytes: 128 },
                GuestRuntimeArtifactSpec { relative_path: "bin\\support.dll".into(), sha256: runtime_fixture_digest(support), max_bytes: 128 },
            ],
        };
        verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).unwrap();
        drop(root);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn runtime_tree_verification_rejects_unlisted_missing_and_tampered_files() {
        let path = make_test_directory("runtime-tree-mismatch");
        fs::create_dir_all(path.join("bin")).unwrap();
        let cargo = b"trusted cargo executable";
        fs::write(path.join("bin").join("cargo.exe"), cargo).unwrap();
        let root = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
        let manifest = GuestRuntimeArtifactManifest {
            schema_version: 1,
            operation: GuestRunOperation::RustCargoTest,
            artifacts: vec![GuestRuntimeArtifactSpec {
                relative_path: "bin\\cargo.exe".into(),
                sha256: runtime_fixture_digest(cargo),
                max_bytes: 128,
            }],
        };
        verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).unwrap();
        fs::write(path.join("bin").join("extra.dll"), b"unlisted").unwrap();
        assert!(verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).is_err());
        fs::remove_file(path.join("bin").join("extra.dll")).unwrap();
        fs::write(path.join("bin").join("cargo.exe"), b"tampered").unwrap();
        assert!(verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).is_err());
        fs::remove_file(path.join("bin").join("cargo.exe")).unwrap();
        assert!(verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).is_err());
        drop(root);
        fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_tree_verification_rejects_unlisted_symlinks() {
        use std::os::unix::fs::symlink;
        let path = make_test_directory("runtime-tree-symlink");
        fs::create_dir_all(path.join("bin")).unwrap();
        let cargo = b"trusted cargo executable";
        fs::write(path.join("bin").join("cargo.exe"), cargo).unwrap();
        symlink(path.join("bin").join("cargo.exe"), path.join("bin").join("alias.exe")).unwrap();
        let root = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
        let manifest = GuestRuntimeArtifactManifest {
            schema_version: 1,
            operation: GuestRunOperation::RustCargoTest,
            artifacts: vec![GuestRuntimeArtifactSpec {
                relative_path: "bin\\cargo.exe".into(),
                sha256: runtime_fixture_digest(cargo),
                max_bytes: 128,
            }],
        };
        assert!(verify_guest_runtime_artifact_tree(&root, GuestRunOperation::RustCargoTest, &manifest).is_err());
        drop(root);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn guest_request_rejects_duplicate_json_fields() {
        let duplicate = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":120000,"maxDurationMs":1,"maxOutputBytes":1048576}"#;
        assert!(parse_guest_run_request(duplicate).is_err());
    }

    #[test]
    fn guest_result_rejects_duplicate_json_fields() {
        let duplicate = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","status":"passed","status":"failed","exitCode":0,"stdout":"ok","stderr":"","truncated":false,"durationMs":25}"#;
        assert!(validate_guest_result_json(
            duplicate,
            "0123456789abcdef0123456789abcdef",
            "abcdef0123456789abcdef0123456789",
            GuestRunOperation::RustCargoTest,
        ).is_err());
    }



    #[test]
    fn gpt_metadata_contains_matching_primary_backup_tables_and_crc_values() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512,
        ).unwrap();
        let metadata = build_guest_result_gpt_metadata(&plan).unwrap();
        assert_eq!(&metadata.protective_mbr[510..512], &[0x55, 0xaa]);
        assert_eq!(&metadata.primary_header[0..8], b"EFI PART");
        assert_eq!(&metadata.backup_header[0..8], b"EFI PART");
        assert_eq!(metadata.primary_entries, metadata.backup_entries);
        assert_eq!(
            gpt_crc32(&metadata.primary_entries),
            u32::from_le_bytes(metadata.primary_header[88..92].try_into().unwrap()),
        );
        assert_eq!(
            gpt_crc32(&metadata.backup_entries),
            u32::from_le_bytes(metadata.backup_header[88..92].try_into().unwrap()),
        );
        assert_eq!(
            u32::from_le_bytes(metadata.primary_header[16..20].try_into().unwrap()),
            gpt_crc32(&{
                let mut header = metadata.primary_header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
                header[16..20].fill(0);
                header
            }),
        );
        assert_eq!(
            u32::from_le_bytes(metadata.backup_header[16..20].try_into().unwrap()),
            gpt_crc32(&{
                let mut header = metadata.backup_header[..GUEST_RESULT_GPT_HEADER_BYTES].to_vec();
                header[16..20].fill(0);
                header
            }),
        );
        assert_ne!(metadata.disk_guid, plan.partition_unique_guid);
        assert_eq!(
            u64::from_le_bytes(metadata.primary_header[24..32].try_into().unwrap()), 1,
        );
        assert_eq!(
            u64::from_le_bytes(metadata.backup_header[24..32].try_into().unwrap()),
            GUEST_RESULT_VOLUME_CAPACITY_BYTES / 512 - 1,
        );
    }

    #[test]
    fn windows_partition_layout_parser_accepts_only_the_exact_single_gpt_partition() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512).unwrap();
        let metadata = build_guest_result_gpt_metadata(&plan).unwrap();
        let mut bytes = vec![0u8; FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES + FRIDAY_PARTITION_INFORMATION_EX_BYTES];
        bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8..24].copy_from_slice(&metadata.primary_header[56..72]);
        let total_sectors = plan.disk_capacity_bytes / GUEST_RESULT_GPT_SECTOR_BYTES;
        bytes[24..32].copy_from_slice(&(GUEST_RESULT_GPT_METADATA_SECTORS * 512).to_le_bytes());
        bytes[32..40].copy_from_slice(&((total_sectors - GUEST_RESULT_GPT_METADATA_SECTORS * 2 + 1) * 512).to_le_bytes());
        bytes[40..44].copy_from_slice(&128u32.to_le_bytes());
        let entry = FRIDAY_DRIVE_LAYOUT_EX_HEADER_BYTES;
        bytes[entry..entry + 4].copy_from_slice(&1u32.to_le_bytes());
        bytes[entry + 8..entry + 16].copy_from_slice(&(plan.first_lba * 512).to_le_bytes());
        bytes[entry + 16..entry + 24].copy_from_slice(&((plan.last_lba - plan.first_lba + 1) * 512).to_le_bytes());
        bytes[entry + 24..entry + 28].copy_from_slice(&1u32.to_le_bytes());
        bytes[entry + 32..entry + 48].copy_from_slice(&metadata.primary_entries[0..16]);
        bytes[entry + 48..entry + 64].copy_from_slice(&metadata.primary_entries[16..32]);
        for (index, unit) in "FRIDAY_RESULTS".encode_utf16().enumerate() {
            bytes[entry + 72 + index * 2..entry + 74 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        assert!(verify_guest_result_windows_partition_layout_bytes(&bytes, bytes.len(), &plan, &metadata).is_ok());

        let mut altered = bytes.clone();
        altered[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[entry + 8..entry + 16].copy_from_slice(&0u64.to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[entry + 96] ^= 0x80;
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        assert!(verify_guest_result_windows_partition_layout_bytes(&bytes, 80, &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[24..32].copy_from_slice(&(35u64 * 512).to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
        let mut altered = bytes.clone();
        altered[40..44].copy_from_slice(&64u32.to_le_bytes());
        assert!(verify_guest_result_windows_partition_layout_bytes(&altered, altered.len(), &plan, &metadata).is_err());
    }

    #[test]
    fn gpt_lifecycle_requires_journaled_intent_before_completion() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Attached, E::CompleteGptInitialization, true, false).is_err());
        assert_eq!(
            advance_guest_result_storage_state(S::Attached, E::BeginGptInitialization, true, false).unwrap(),
            S::GptWriteInProgress,
        );
        assert_eq!(
            advance_guest_result_storage_state(S::GptWriteInProgress, E::CompleteGptInitialization, true, false).unwrap(),
            S::Partitioned,
        );
        assert!(advance_guest_result_storage_state(S::GptWriteInProgress, E::BeginMount, true, false).is_err());
        assert!(advance_guest_result_storage_state(S::Formatted, E::CompleteMount, true, false).is_err());
        assert!(advance_guest_result_storage_state(S::ShareRevoked, E::DetachVerified, true, false).is_err());
        assert_eq!(
            advance_guest_result_storage_state(S::GptWriteInProgress, E::PreserveForRecovery, false, false).unwrap(),
            S::RecoveryRequired,
        );
    }

    #[test]
    fn guest_operation_commands_are_fixed_and_have_no_shell_or_user_arguments() {
        let cases = [
            (GuestRunOperation::RustCargoTest, "cargo", &["test", "--locked", "--offline"][..]),
            (GuestRunOperation::NodeNpmTest, "npm", &["test"][..]),
            (GuestRunOperation::PythonUnittest, "python", &["-m", "unittest", "discover", "-v"][..]),
        ];
        for (operation, expected_program, expected_arguments) in cases {
            let plan = guest_operation_command(operation);
            assert_eq!(plan.program, expected_program);
            assert_eq!(plan.arguments, expected_arguments);
            assert!(!plan.program.is_empty());
            assert!(plan.arguments.iter().all(|argument| !argument.is_empty()));
            assert!(!plan.arguments.iter().any(|argument| {
                argument.contains('&')
                    || argument.contains('|')
                    || argument.contains(';')
                    || argument.contains('\n')
                    || argument.contains('\r')
            }));
        }
    }

    #[test]
    fn guest_runtime_launch_profiles_use_only_pinned_guest_paths_and_fixed_arguments() {
        let cases = [
            (
                GuestRunOperation::RustCargoTest,
                r"C:\FRIDAY\Toolchains\Rust\bin\cargo.exe",
                vec!["test", "--locked", "--offline"],
            ),
            (
                GuestRunOperation::NodeNpmTest,
                r"C:\FRIDAY\Toolchains\Node\node.exe",
                vec![
                    r"C:\FRIDAY\Toolchains\Node\node_modules\npm\bin\npm-cli.js",
                    "test",
                ],
            ),
            (
                GuestRunOperation::PythonUnittest,
                r"C:\FRIDAY\Toolchains\Python\python.exe",
                vec!["-m", "unittest", "discover", "-v"],
            ),
        ];
        for (operation, expected_program, expected_arguments) in cases {
            let plan = build_guest_runtime_launch_plan(operation);
            assert_eq!(plan.program, PathBuf::from(expected_program));
            assert_eq!(plan.arguments, expected_arguments.iter().map(|value| value.to_string()).collect::<Vec<_>>());
            assert_eq!(plan.working_directory, PathBuf::from(GUEST_WORKSPACE_ROOT));
            assert!(plan.program.to_string_lossy().starts_with(r"C:\FRIDAY"));
            assert!(!plan.program.to_string_lossy().to_ascii_lowercase().ends_with(".cmd"));
            assert!(!plan.program.to_string_lossy().to_ascii_lowercase().ends_with(".bat"));
            assert!(plan.arguments.iter().all(|argument| argument.chars().all(|character| !matches!(character, '&' | '|' | ';' | '\n' | '\r'))));
        }
    }

    #[test]
    fn guest_runtime_environment_is_operation_specific_and_explicit() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let plan = build_guest_runtime_environment_plan(operation);
            assert!(validate_guest_runtime_environment_plan(operation, &plan).is_ok());
            assert_eq!(plan.variables.get("SystemRoot").map(String::as_str), Some(GUEST_WINDOWS_ROOT));
            assert_eq!(plan.variables.get("WINDIR").map(String::as_str), Some(GUEST_WINDOWS_ROOT));
            assert_eq!(plan.variables.get("TEMP").map(String::as_str), Some(GUEST_TEMP_ROOT));
            assert_eq!(plan.variables.get("TMP").map(String::as_str), Some(GUEST_TEMP_ROOT));
            assert!(plan.variables.get("PATH").unwrap().contains(r"C:\Windows\System32"));
            assert!(!plan.variables.keys().any(|key| key.eq_ignore_ascii_case("USERPROFILE")));
            assert!(!plan.variables.keys().any(|key| key.eq_ignore_ascii_case("APPDATA")));
            assert!(!plan.variables.keys().any(|key| key.eq_ignore_ascii_case("COMSPEC")));
        }
        assert!(build_guest_runtime_environment_plan(GuestRunOperation::RustCargoTest).variables.contains_key("CARGO_HOME"));
        assert!(build_guest_runtime_environment_plan(GuestRunOperation::RustCargoTest).variables.contains_key("RUSTUP_HOME"));
        assert!(build_guest_runtime_environment_plan(GuestRunOperation::NodeNpmTest).variables.contains_key("npm_config_cache"));
        assert!(build_guest_runtime_environment_plan(GuestRunOperation::PythonUnittest).variables.contains_key("PYTHONHOME"));
    }

    fn test_runtime_artifact(
        relative_path: &str,
        bytes: &[u8],
    ) -> GuestRuntimeArtifactSpec {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        GuestRuntimeArtifactSpec {
            relative_path: relative_path.into(),
            sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
            max_bytes: bytes.len() as u64,
        }
    }

    fn test_runtime_manifest(
        operation: GuestRunOperation,
    ) -> GuestRuntimeArtifactManifest {
        let mut artifacts = match operation {
            GuestRunOperation::RustCargoTest => vec![
                test_runtime_artifact("bin\\cargo.exe", b"cargo-fixture"),
            ],
            GuestRunOperation::NodeNpmTest => vec![
                test_runtime_artifact("node.exe", b"node-fixture"),
                test_runtime_artifact("node_modules\\npm\\bin\\npm-cli.js", b"npm-fixture"),
            ],
            GuestRunOperation::PythonUnittest => vec![
                test_runtime_artifact("python.exe", b"python-fixture"),
            ],
        };
        artifacts.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        GuestRuntimeArtifactManifest { schema_version: 1, operation, artifacts }
    }

    fn test_runtime_manifest_trust_anchor(
        operation: GuestRunOperation,
        manifest: &GuestRuntimeArtifactManifest,
    ) -> GuestRuntimeManifestTrustAnchor {
        let mut anchor = GuestRuntimeManifestTrustAnchor {
            schema_version: 1,
            operation,
            bundle_id: "friday-test-runtime-bundle".into(),
            toolchain_version: "1.0.0".into(),
            platform: "windows".into(),
            architecture: "x86_64".into(),
            manifest_sha256: String::new(),
        };
        anchor.manifest_sha256 = canonical_guest_runtime_bundle_sha256(manifest, &anchor).unwrap();
        anchor
    }


    #[test]
    fn authenticated_runtime_provisioner_stages_only_exact_digest_verified_sources() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-provision-from-bytes");
        let final_root = root.join("published-runtime");
        let plan = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root,
        ).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let files = vec![(
            PathBuf::from("bin/cargo.exe"),
            b"cargo-fixture".to_vec(),
        )];
        let proof = provision_authenticated_guest_runtime_bundle_from_bytes(
            &root, &parent, &manifest, &anchor, &plan, &files,
        ).unwrap();
        assert_eq!(proof.bundle_id, anchor.bundle_id);
        assert_eq!(proof.manifest_sha256, anchor.manifest_sha256);
        assert_eq!(proof.artifact_count, manifest.artifacts.len());
        assert!(plan.staging_root.join("bin/cargo.exe").is_file());
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authenticated_runtime_provisioner_rejects_tampered_missing_and_unlisted_sources_before_staging() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-provision-reject");
        let plan = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, root.join("published-runtime"),
        ).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();

        let tampered = vec![(PathBuf::from("bin/cargo.exe"), b"tampered".to_vec())];
        assert!(provision_authenticated_guest_runtime_bundle_from_bytes(
            &root, &parent, &manifest, &anchor, &plan, &tampered,
        ).is_err());
        assert!(!plan.staging_root.exists());

        let missing = Vec::new();
        assert!(provision_authenticated_guest_runtime_bundle_from_bytes(
            &root, &parent, &manifest, &anchor, &plan, &missing,
        ).is_err());
        assert!(!plan.staging_root.exists());

        let unlisted = vec![
            (PathBuf::from("bin/cargo.exe"), b"cargo-fixture".to_vec()),
            (PathBuf::from("unexpected.dll"), b"extra".to_vec()),
        ];
        assert!(provision_authenticated_guest_runtime_bundle_from_bytes(
            &root, &parent, &manifest, &anchor, &plan, &unlisted,
        ).is_err());
        assert!(!plan.staging_root.exists());
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_runtime_verification_binds_marker_exact_tree_and_hashes() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let path = make_test_directory("staged-runtime-verify");
        let test_final_root = path.join("published-runtime");
        let plan = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, test_final_root.clone(),
        ).unwrap();
        let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
        let stage_path = plan.staging_root.clone();
        let staging_name = stage_path.file_name().unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
        let files = manifest.artifacts.iter().map(|artifact| {
            let bytes = match artifact.relative_path.as_str() {
                "bin\\cargo.exe" => b"cargo-fixture".to_vec(),
                _ => panic!("unexpected runtime fixture"),
            };
            (PathBuf::from(artifact.relative_path.replace('\\', "/")), bytes)
        }).collect::<Vec<_>>();
        crate::coding_sandbox_fs::create_bounded_runtime_destination(
            &parent, staging_name,
            &files, &marker, 8, 1024, 4096, 8,
        ).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, staging_name,
        ).unwrap();
        let proof = verify_staged_guest_runtime_bundle_with_final_root(
            &stage_path, &stage, &manifest, &anchor, &plan, test_final_root,
        ).unwrap();
        assert_eq!(proof.bundle_id, anchor.bundle_id);
        assert_eq!(proof.artifact_count, manifest.artifacts.len());
        assert_ne!(proof.root_identity, (0, 0));
        drop(stage);
        drop(parent);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn staged_runtime_verification_rejects_tampered_marker_and_unlisted_files() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let path = make_test_directory("staged-runtime-reject");
        let test_final_root = path.join("published-runtime");
        let plan = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, test_final_root.clone(),
        ).unwrap();
        let stage_path = plan.staging_root.clone();
        let staging_name = stage_path.file_name().unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
        let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
        let files = vec![(PathBuf::from("bin/cargo.exe"), b"cargo-fixture".to_vec())];
        crate::coding_sandbox_fs::create_bounded_runtime_destination(
            &parent, staging_name,
            &files, &marker, 8, 1024, 4096, 8,
        ).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, staging_name,
        ).unwrap();
        fs::write(stage_path.join(".friday-runtime-owner.json"), b"{\"schemaVersion\":1}").unwrap();
        assert!(verify_staged_guest_runtime_bundle_with_final_root(
            &stage_path, &stage, &manifest, &anchor, &plan, test_final_root.clone(),
        ).is_err());
        fs::write(stage_path.join(".friday-runtime-owner.json"), &marker).unwrap();
        fs::write(stage_path.join("unexpected.dll"), b"untrusted").unwrap();
        assert!(verify_staged_guest_runtime_bundle_with_final_root(
            &stage_path, &stage, &manifest, &anchor, &plan, test_final_root,
        ).is_err());
        drop(stage);
        drop(parent);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn authenticated_runtime_bundle_proof_requires_manifest_and_disk_to_match() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
            let path = make_test_directory("authenticated-runtime-proof");
            for artifact in &manifest.artifacts {
                let components = artifact.relative_path.split('\\').collect::<Vec<_>>();
                let file_name = components.last().unwrap();
                let mut file_path = path.clone();
                for component in &components[..components.len() - 1] {
                    file_path.push(component);
                }
                fs::create_dir_all(&file_path).unwrap();
                file_path.push(file_name);
                let bytes: &[u8] = match artifact.relative_path.as_str() {
                    "bin\\cargo.exe" => b"cargo-fixture",
                    "node.exe" => b"node-fixture",
                    "node_modules\\npm\\bin\\npm-cli.js" => b"npm-fixture",
                    "python.exe" => b"python-fixture",
                    _ => panic!("unexpected runtime fixture"),
                };
                fs::write(file_path, bytes).unwrap();
            }
            let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
            fs::write(path.join(".friday-runtime-owner.json"), marker).unwrap();
            let root = crate::coding_sandbox_fs::open_verified_root_directory(&path).unwrap();
            let proof = verify_authenticated_guest_runtime_bundle(&path, &root, operation, &manifest, &anchor).unwrap();
            assert_eq!(proof.operation, operation);
            assert_eq!(proof.root_path, path);
            assert!(validate_verified_guest_runtime_bundle_root(&proof, &path, &root).is_err(),
                "temporary test roots are not the fixed production installation path");
            assert_ne!(proof.root_identity, (0, 0));
            assert_eq!(proof.bundle_id, anchor.bundle_id);
            let mut substituted_identity = proof.clone();
            substituted_identity.root_identity.1 ^= 1;
            assert!(validate_verified_guest_runtime_bundle_root(&substituted_identity, &path, &root).is_err(),
                "a proof with a substituted directory identity must be rejected");
            assert_eq!(proof.toolchain_version, anchor.toolchain_version);
            assert_eq!(proof.platform, "windows");
            assert_eq!(proof.architecture, "x86_64");
            assert_eq!(proof.manifest_sha256, anchor.manifest_sha256);
            assert_eq!(proof.artifact_count, manifest.artifacts.len());

            let mut wrong_anchor = anchor.clone();
            wrong_anchor.bundle_id = "different-bundle".into();
            assert!(verify_authenticated_guest_runtime_bundle(&path, &root, operation, &manifest, &wrong_anchor).is_err());
            let unrelated_path = path.with_file_name(format!("{}-other", path.file_name().unwrap().to_string_lossy()));
            fs::create_dir_all(&unrelated_path).unwrap();
            assert!(verify_authenticated_guest_runtime_bundle(&unrelated_path, &root, operation, &manifest, &anchor).is_err());
            fs::remove_dir_all(unrelated_path).unwrap();

            let mut wrong_version = anchor.clone();
            wrong_version.toolchain_version = "9.9.9".into();
            assert!(verify_authenticated_guest_runtime_bundle(&path, &root, operation, &manifest, &wrong_version).is_err());

            drop(root);
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn guest_runtime_manifest_authentication_binds_metadata_and_all_artifacts() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
            assert!(authenticate_guest_runtime_manifest(operation, &manifest, &anchor).is_ok());

            let mut reordered = manifest.clone();
            reordered.artifacts.reverse();
            assert!(authenticate_guest_runtime_manifest(operation, &reordered, &anchor).is_ok());

            let mut tampered = manifest.clone();
            tampered.artifacts[0].sha256 = "a".repeat(64);
            assert!(authenticate_guest_runtime_manifest(operation, &tampered, &anchor).is_err());

            let mut wrong_operation = anchor.clone();
            wrong_operation.operation = match operation {
                GuestRunOperation::RustCargoTest => GuestRunOperation::NodeNpmTest,
                GuestRunOperation::NodeNpmTest => GuestRunOperation::PythonUnittest,
                GuestRunOperation::PythonUnittest => GuestRunOperation::RustCargoTest,
            };
            assert!(authenticate_guest_runtime_manifest(operation, &manifest, &wrong_operation).is_err());

            let mut unpinned_version = anchor.clone();
            unpinned_version.toolchain_version.clear();
            assert!(authenticate_guest_runtime_manifest(operation, &manifest, &unpinned_version).is_err());

            let mut wrong_platform = anchor.clone();
            wrong_platform.platform = "linux".into();
            assert!(authenticate_guest_runtime_manifest(operation, &manifest, &wrong_platform).is_err());
        }
    }

    #[test]
    fn guest_runtime_artifact_manifests_validate_for_each_operation() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let manifest = test_runtime_manifest(operation);
            assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_ok());
            for artifact in &manifest.artifacts {
                let bytes = match artifact.relative_path.as_str() {
                    "bin\\cargo.exe" => b"cargo-fixture".as_slice(),
                    "node.exe" => b"node-fixture".as_slice(),
                    "node_modules\\npm\\bin\\npm-cli.js" => b"npm-fixture".as_slice(),
                    "python.exe" => b"python-fixture".as_slice(),
                    _ => panic!("unexpected test artifact"),
                };
                assert!(verify_guest_runtime_artifact_bytes(artifact, bytes).is_ok());
            }
        }
    }

    #[test]
    fn guest_runtime_artifact_manifests_reject_unsafe_or_incomplete_entries() {
        let operation = GuestRunOperation::NodeNpmTest;
        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts[0].relative_path = "..\\node.exe".into();
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts[1].relative_path = "NODE.EXE".into();
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts.push(test_runtime_artifact(".friday-runtime-owner.json", b"spoofed-marker"));
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts.push(test_runtime_artifact("node.exe\\child.dll", b"nested"));
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts.push(test_runtime_artifact("node_modules\\npm\\bin\\npm-cli.js\\child", b"nested"));
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts.pop();
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts[0].sha256 = "z".repeat(64);
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());

        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts[0].max_bytes = MAX_GUEST_RUNTIME_ARTIFACT_BYTES + 1;
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());
    }

    #[test]
    fn guest_runtime_artifact_manifest_rejects_file_directory_collisions_in_either_order() {
        let operation = GuestRunOperation::NodeNpmTest;

        let mut file_then_child = test_runtime_manifest(operation);
        file_then_child.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe", b"file"),
        );
        file_then_child.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe\child.dll", b"child"),
        );
        assert!(validate_guest_runtime_artifact_manifest(operation, &file_then_child).is_err());

        let mut child_then_file = test_runtime_manifest(operation);
        child_then_file.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe\child.dll", b"child"),
        );
        child_then_file.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe", b"file"),
        );
        assert!(validate_guest_runtime_artifact_manifest(operation, &child_then_file).is_err());

        let mut case_variant_collision = test_runtime_manifest(operation);
        case_variant_collision.artifacts.push(
            test_runtime_artifact(r"Tools\Runtime.exe\child.dll", b"child"),
        );
        case_variant_collision.artifacts.push(
            test_runtime_artifact(r"tools\runtime.EXE", b"file"),
        );
        assert!(validate_guest_runtime_artifact_manifest(operation, &case_variant_collision).is_err());
    }

    #[test]
    fn guest_runtime_artifact_manifest_allows_distinct_sibling_prefix_names() {
        let operation = GuestRunOperation::NodeNpmTest;
        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe", b"file"),
        );
        manifest.artifacts.push(
            test_runtime_artifact(r"tools\runtime.exe.extra", b"sibling"),
        );
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_ok());
    }

    #[test]
    fn guest_runtime_artifact_manifest_rejects_case_variant_reserved_owner_marker() {
        let operation = GuestRunOperation::NodeNpmTest;
        let mut manifest = test_runtime_manifest(operation);
        manifest.artifacts[0].relative_path = ".FRIDAY-RUNTIME-OWNER.JSON".into();
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_err());
    }

    #[test]
    fn guest_runtime_artifact_manifest_rejects_windows_reserved_device_names_and_trailing_dots() {
        let operation = GuestRunOperation::NodeNpmTest;
        for component in [
            "CON", "con.txt", "PRN.log", "AUX", "NUL.bin",
            "COM1.dll", "COM9", "LPT1.txt", "LPT9",
            "CONIN$", "CONOUT$", "cache.",
        ] {
            let mut manifest = test_runtime_manifest(operation);
            manifest.artifacts.push(test_runtime_artifact(
                &format!(r"tools\{component}"),
                b"reserved",
            ));
            assert!(
                validate_guest_runtime_artifact_manifest(operation, &manifest).is_err(),
                "Windows-reserved component should be rejected: {component}",
            );
        }
    }

    #[test]
    fn guest_runtime_artifact_manifest_allows_non_reserved_device_name_prefixes() {
        let operation = GuestRunOperation::NodeNpmTest;
        let mut manifest = test_runtime_manifest(operation);
        for component in ["COM0", "COM10", "LPT0", "LPT10", "CONSOLE", "cache.data"] {
            manifest.artifacts.push(test_runtime_artifact(
                &format!(r"tools\{component}"),
                b"ordinary",
            ));
        }
        assert!(validate_guest_runtime_artifact_manifest(operation, &manifest).is_ok());
    }

    #[test]
    fn guest_runtime_artifact_manifest_enforces_windows_path_component_and_total_path_limits() {
        let operation = GuestRunOperation::NodeNpmTest;

        let mut oversized_component = test_runtime_manifest(operation);
        oversized_component.artifacts.push(test_runtime_artifact(
            &format!(r"tools\{}", "a".repeat(MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES + 1)),
            b"oversized component",
        ));
        assert!(validate_guest_runtime_artifact_manifest(operation, &oversized_component).is_err());

        let mut maximum_component = test_runtime_manifest(operation);
        maximum_component.artifacts.push(test_runtime_artifact(
            &format!(r"tools\{}", "a".repeat(MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES)),
            b"maximum component",
        ));
        assert!(validate_guest_runtime_artifact_manifest(operation, &maximum_component).is_ok());

        let mut oversized_path = test_runtime_manifest(operation);
        let long_path = vec!["a".repeat(205); 5].join(r"\");
        assert!(long_path.len() > MAX_GUEST_RUNTIME_ARTIFACT_PATH_BYTES);
        oversized_path.artifacts.push(test_runtime_artifact(&long_path, b"oversized path"));
        assert!(validate_guest_runtime_artifact_manifest(operation, &oversized_path).is_err());

        let mut maximum_path = test_runtime_manifest(operation);
        let bounded_path = vec!["a".repeat(255); 4].join(r"\");
        assert!(bounded_path.len() <= MAX_GUEST_RUNTIME_ARTIFACT_PATH_BYTES);
        maximum_path.artifacts.push(test_runtime_artifact(&bounded_path, b"bounded path"));
        assert!(validate_guest_runtime_artifact_manifest(operation, &maximum_path).is_ok());
    }

    #[test]
    fn guest_runtime_artifact_digest_verification_rejects_tampering_and_size_overrun() {
        let artifact = test_runtime_artifact("python.exe", b"trusted-fixture");
        assert!(verify_guest_runtime_artifact_bytes(&artifact, b"trusted-fixture").is_ok());
        assert!(verify_guest_runtime_artifact_bytes(&artifact, b"tampered-fixture").is_err());

        let mut too_small = artifact.clone();
        too_small.max_bytes = 2;
        assert!(verify_guest_runtime_artifact_bytes(&too_small, b"trusted-fixture").is_err());
    }

    #[test]
    fn guest_runtime_artifact_root_verifier_checks_nested_file_bytes() {
        let directory = make_test_directory("runtime-artifact-root");
        fs::create_dir_all(directory.join("bin")).unwrap();
        fs::write(directory.join("bin").join("cargo.exe"), b"trusted cargo fixture").unwrap();
        let root = crate::coding_sandbox_fs::open_verified_root_directory(&directory).unwrap();
        let artifact = test_runtime_artifact("bin\\cargo.exe", b"trusted cargo fixture");
        assert!(verify_guest_runtime_artifact_from_root(&root, &artifact).is_ok());
        let changed = test_runtime_artifact("bin\\cargo.exe", b"different trusted bytes");
        assert!(verify_guest_runtime_artifact_from_root(&root, &changed).is_err());
        drop(root);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn guest_runtime_environment_block_is_sorted_and_double_nul_terminated() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let plan = build_guest_runtime_environment_plan(operation);
            let block = serialize_guest_runtime_environment_block(operation, &plan).unwrap();
            assert!(block.len() >= 2);
            assert_eq!(&block[block.len() - 2..], &[0, 0]);
            let entries = String::from_utf16(&block[..block.len() - 1]).unwrap();
            let names = entries.split('\0').filter_map(|entry| entry.split_once('=').map(|(name, _)| name))
                .collect::<Vec<_>>();
            let mut sorted = names.clone();
            sorted.sort_by_key(|name| name.to_ascii_lowercase());
            assert_eq!(names, sorted);
            assert!(!entries.contains("USERPROFILE="));
            assert!(!entries.contains("APPDATA="));
            assert!(!entries.contains("COMSPEC="));
        }
    }

    #[test]
    fn guest_runtime_environment_block_rejects_non_profile_values() {
        let operation = GuestRunOperation::NodeNpmTest;
        let mut plan = build_guest_runtime_environment_plan(operation);
        plan.variables.insert("COMSPEC".into(), r"C:\Windows\System32\cmd.exe".into());
        assert!(serialize_guest_runtime_environment_block(operation, &plan).is_err());

        let mut plan = build_guest_runtime_environment_plan(operation);
        plan.variables.insert("PATH".into(), "C:\\Injected".into());
        assert!(serialize_guest_runtime_environment_block(operation, &plan).is_err());
    }

    #[test]
    fn guest_runtime_environment_validator_rejects_mutations_and_injected_variables() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let trusted = build_guest_runtime_environment_plan(operation);
            let mut changed_value = trusted.clone();
            changed_value.variables.insert("PATH".into(), r"C:\Attacker".into());
            assert!(validate_guest_runtime_environment_plan(operation, &changed_value).is_err());

            let mut injected = trusted.clone();
            injected.variables.insert("COMSPEC".into(), r"C:\Windows\System32\cmd.exe".into());
            assert!(validate_guest_runtime_environment_plan(operation, &injected).is_err());

            let mut inherited = trusted.clone();
            inherited.variables.insert("USERPROFILE".into(), r"C:\Users\Someone".into());
            assert!(validate_guest_runtime_environment_plan(operation, &inherited).is_err());
        }
    }

    #[test]
    fn guest_runtime_launch_plan_validator_rejects_each_substituted_field() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let trusted = build_guest_runtime_launch_plan(operation);
            assert!(validate_guest_runtime_launch_plan(operation, &trusted).is_ok());

            let mut changed_program = trusted.clone();
            changed_program.program = PathBuf::from(r"C:\\Windows\\System32\\cmd.exe");
            assert!(validate_guest_runtime_launch_plan(operation, &changed_program).is_err());

            let mut changed_arguments = trusted.clone();
            changed_arguments.arguments.push("--unreviewed".into());
            assert!(validate_guest_runtime_launch_plan(operation, &changed_arguments).is_err());

            let mut changed_working_directory = trusted.clone();
            changed_working_directory.working_directory = PathBuf::from(r"C:\\FRIDAY\\OtherWorkspace");
            assert!(validate_guest_runtime_launch_plan(operation, &changed_working_directory).is_err());
        }
    }

    #[test]
    fn node_runtime_profile_never_invokes_a_command_script() {
        let plan = build_guest_runtime_launch_plan(GuestRunOperation::NodeNpmTest);
        assert!(plan.program.to_string_lossy().ends_with("node.exe"));
        assert_eq!(plan.arguments.len(), 2);
        assert!(plan.arguments[0].ends_with(r"npm-cli.js"));
        assert_eq!(plan.arguments[1], "test");
        assert!(!plan.arguments.iter().any(|argument| argument.to_ascii_lowercase().ends_with(".cmd")));
    }

    #[test]
    fn guest_operation_contract_covers_every_deserializable_operation() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let plan = guest_operation_command(operation);
            assert!(!plan.arguments.is_empty());
        }
    }

    #[test]
    fn execution_plan_binds_validated_request_to_fixed_operation_command() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let plan = build_guest_run_execution_plan(request).unwrap();
        assert_eq!(plan.run_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(plan.snapshot_id, "abcdef0123456789abcdef0123456789");
        assert_eq!(plan.operation, GuestRunOperation::RustCargoTest);
        assert_eq!(plan.command.program, "cargo");
        assert_eq!(plan.command.arguments, &["test", "--locked", "--offline"]);
        assert_eq!(plan.max_duration, Duration::from_millis(MAX_GUEST_REQUEST_DURATION_MS));
        assert_eq!(plan.max_output_bytes, MAX_GUEST_REQUEST_OUTPUT_BYTES);
    }

    #[test]
    fn approved_execution_plan_serializes_to_exact_bounded_guest_request() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let plan = build_guest_run_execution_plan(request).unwrap();
        let bytes = serialize_guest_run_execution_request(&plan).unwrap();
        assert!(bytes.len() <= 4096);
        let parsed = parse_guest_run_request(&bytes).unwrap();
        assert_eq!(parsed.run_id, plan.run_id);
        assert_eq!(parsed.snapshot_id, plan.snapshot_id);
        assert_eq!(parsed.operation, plan.operation);
        assert_eq!(parsed.max_duration_ms, plan.max_duration.as_millis() as u64);
        assert_eq!(parsed.max_output_bytes, plan.max_output_bytes);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 6);
        assert!(json.get("command").is_none());
        assert!(json.get("executable").is_none());
        assert!(json.get("arguments").is_none());
        assert!(json.get("workingDirectory").is_none());
        assert!(json.get("environment").is_none());
    }

    #[test]
    fn approved_execution_plan_serialization_rejects_mutated_or_oversized_policy() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let mut plan = build_guest_run_execution_plan(request).unwrap();
        plan.command = GuestOperationCommand { program: "cmd.exe", arguments: &["/c", "whoami"] };
        assert!(serialize_guest_run_execution_request(&plan).is_err());
        plan.command = guest_operation_command(GuestRunOperation::RustCargoTest);
        plan.max_duration = Duration::ZERO;
        assert!(serialize_guest_run_execution_request(&plan).is_err());
    }

    #[test]
    fn execution_plan_revalidates_limits_before_creating_a_plan() {
        let mut request: GuestRunRequest =
            serde_json::from_slice(&valid_guest_request_json()).unwrap();
        request.max_duration_ms = MAX_GUEST_REQUEST_DURATION_MS + 1;
        assert!(build_guest_run_execution_plan(request).is_err());

        let mut request: GuestRunRequest =
            serde_json::from_slice(&valid_guest_request_json()).unwrap();
        request.max_output_bytes = MAX_GUEST_REQUEST_OUTPUT_BYTES + 1;
        assert!(build_guest_run_execution_plan(request).is_err());
    }

    #[test]
    fn execution_plan_keeps_run_and_snapshot_identity_separate() {
        let mut request: GuestRunRequest =
            serde_json::from_slice(&valid_guest_request_json()).unwrap();
        request.run_id = "11111111111111111111111111111111".into();
        request.snapshot_id = "22222222222222222222222222222222".into();
        let plan = build_guest_run_execution_plan(request).unwrap();
        assert_eq!(plan.run_id, "11111111111111111111111111111111");
        assert_eq!(plan.snapshot_id, "22222222222222222222222222222222");
        assert_ne!(plan.run_id, plan.snapshot_id);
    }

    #[test]
    fn guest_run_approval_is_bound_to_the_exact_execution_plan() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let (token, mut grant) = issue_guest_run_approval(&plan, 1_000).unwrap();
        assert!(consume_guest_run_approval(&mut grant, &token, &plan, 1_001).is_ok());
        assert!(grant.consumed);
    }

    #[test]
    fn guest_run_approval_rejects_a_different_run_snapshot_or_operation() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let (token, mut grant) = issue_guest_run_approval(&plan, 1_000).unwrap();

        let mut changed_run = plan.clone();
        changed_run.run_id = "11111111111111111111111111111111".into();
        assert!(consume_guest_run_approval(&mut grant, &token, &changed_run, 1_001).is_err());

        let mut changed_snapshot = plan.clone();
        changed_snapshot.snapshot_id = "22222222222222222222222222222222".into();
        assert!(consume_guest_run_approval(&mut grant, &token, &changed_snapshot, 1_001).is_err());

        let mut changed_operation = plan.clone();
        changed_operation.operation = GuestRunOperation::PythonUnittest;
        changed_operation.command = guest_operation_command(changed_operation.operation);
        assert!(consume_guest_run_approval(&mut grant, &token, &changed_operation, 1_001).is_err());
        assert!(!grant.consumed);
    }

    #[test]
    fn guest_run_approval_expires_and_cannot_be_replayed() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let (token, mut expired) = issue_guest_run_approval(&plan, 1_000).unwrap();
        assert!(consume_guest_run_approval(
            &mut expired, &token, &plan, 1_000 + GUEST_RUN_APPROVAL_TTL_SECONDS,
        ).is_err());
        assert!(!expired.consumed);

        let (token, mut used) = issue_guest_run_approval(&plan, 2_000).unwrap();
        assert!(consume_guest_run_approval(&mut used, &token, &plan, 2_001).is_ok());
        assert!(consume_guest_run_approval(&mut used, &token, &plan, 2_002).is_err());
    }

    #[test]
    fn guest_run_approval_rejects_tampered_tokens_and_invalid_plan_policy() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let (token, mut grant) = issue_guest_run_approval(&plan, 1_000).unwrap();
        let mut altered = token.clone().into_bytes();
        altered[0] = if altered[0] == b'0' { b'1' } else { b'0' };
        let altered = String::from_utf8(altered).unwrap();
        assert!(consume_guest_run_approval(&mut grant, &altered, &plan, 1_001).is_err());
        assert!(!grant.consumed);

        let mut invalid_plan = plan.clone();
        invalid_plan.command.arguments = &["cmd.exe", "/c", "whoami"];
        assert!(issue_guest_run_approval(&invalid_plan, 1_000).is_err());
    }

    #[test]
    fn approval_store_replaces_prior_token_for_same_run() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();
        let first = store.issue(&plan, 1_000).unwrap();
        let second = store.issue(&plan, 1_001).unwrap();
        assert_ne!(first, second);
        assert_eq!(store.pending_count(), 1);
        assert!(store.consume(&plan, &first, 1_002).is_err());
        assert_eq!(store.pending_count(), 0);
        assert!(store.consume(&plan, &second, 1_002).is_err());
    }

    #[test]
    fn approval_store_consumes_mismatches_and_prevents_replay() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();
        let token = store.issue(&plan, 1_000).unwrap();
        let mut changed = plan.clone();
        changed.snapshot_id = "22222222222222222222222222222222".into();
        assert!(store.consume(&changed, &token, 1_001).is_err());
        assert_eq!(store.pending_count(), 0);
        assert!(store.consume(&plan, &token, 1_002).is_err());

        let token = store.issue(&plan, 2_000).unwrap();
        assert!(store.consume(&plan, &token, 2_001).is_ok());
        assert!(store.consume(&plan, &token, 2_002).is_err());
    }

    #[test]
    fn approval_store_prunes_expired_entries_and_enforces_capacity() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();
        for index in 0..MAX_PENDING_GUEST_RUN_APPROVALS {
            let mut distinct = plan.clone();
            distinct.run_id = format!("{index:032x}");
            store.issue(&distinct, 1_000).unwrap();
        }
        assert_eq!(store.pending_count(), MAX_PENDING_GUEST_RUN_APPROVALS);
        let mut extra = plan.clone();
        extra.run_id = "ffffffffffffffffffffffffffffffff".into();
        assert!(store.issue(&extra, 1_001).is_err());
        assert!(store.issue(&extra, 1_000 + GUEST_RUN_APPROVAL_TTL_SECONDS).is_ok());
        assert_eq!(store.pending_count(), 1);
    }

    #[test]
    fn approval_store_revokes_only_the_requested_run() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut other = plan.clone();
        other.run_id = "11111111111111111111111111111111".into();
        let mut store = GuestRunApprovalStore::default();
        store.issue(&plan, 1_000).unwrap();
        let other_token = store.issue(&other, 1_000).unwrap();
        assert!(store.revoke(&plan.run_id));
        assert!(!store.revoke(&plan.run_id));
        assert_eq!(store.pending_count(), 1);
        assert!(store.consume(&other, &other_token, 1_001).is_ok());
        assert_eq!(store.pending_count(), 0);
    }



    #[test]
    fn guest_request_accepts_only_fixed_allowlisted_bounded_contract() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        assert_eq!(request.operation, GuestRunOperation::RustCargoTest);
        assert_eq!(request.max_duration_ms, MAX_GUEST_REQUEST_DURATION_MS);
        assert_eq!(request.max_output_bytes, MAX_GUEST_REQUEST_OUTPUT_BYTES);
    }

    #[test]
    fn guest_request_rejects_unknown_fields_and_duplicate_keys() {
        let unknown = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":1000,"maxOutputBytes":1000,"command":"cmd.exe"}"#;
        assert!(parse_guest_run_request(unknown).is_err());
        let duplicate = br#"{"schemaVersion":1,"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","maxDurationMs":1000,"maxOutputBytes":1000}"#;
        assert!(parse_guest_run_request(duplicate).is_err());
    }

    #[test]
    fn guest_request_rejects_invalid_ids_versions_and_resource_limits() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_guest_request_json()).unwrap();
        value["schemaVersion"] = serde_json::json!(2);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["schemaVersion"] = serde_json::json!(1);
        value["runId"] = serde_json::json!("../outside");
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["runId"] = serde_json::json!("0123456789abcdef0123456789abcdef");
        value["snapshotId"] = serde_json::json!("../../project");
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["snapshotId"] = serde_json::json!("abcdef0123456789abcdef0123456789");
        value["maxDurationMs"] = serde_json::json!(MAX_GUEST_REQUEST_DURATION_MS + 1);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxDurationMs"] = serde_json::json!(0);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxDurationMs"] = serde_json::json!(1000);
        value["maxOutputBytes"] = serde_json::json!(MAX_GUEST_REQUEST_OUTPUT_BYTES + 1);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
        value["maxOutputBytes"] = serde_json::json!(0);
        assert!(parse_guest_run_request(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn guest_request_rejects_oversized_documents() {
        assert!(parse_guest_run_request(&vec![b' '; 4097]).is_err());
    }


    #[cfg(windows)]
    #[test]
    fn open_source_file_opens_windows_symlink_itself_not_its_target() {
        use std::os::windows::fs::symlink_file;
        let directory = make_test_directory("windows-no-follow-open");
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");
        fs::write(&target, b"trusted target").unwrap();
        symlink_file(&target, &link)
            .expect("Windows CI must permit creating a file symlink for the no-follow regression test");
        let opened = open_source_file(&link).expect("opening the reparse point itself should succeed");
        let metadata = opened.metadata().unwrap();
        assert!(is_reparse_or_symlink(&metadata), "opened handle must identify the reparse point itself");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn open_source_file_rejects_symlinks_without_following_them() {
        let directory = make_test_directory("no-follow-open");
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");
        fs::write(&target, b"trusted target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(open_source_file(&link).is_err());
        let mut opened = open_source_file(&target).unwrap();
        let mut contents = Vec::new();
        opened.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"trusted target");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn guest_result_file_reader_accepts_only_bounded_regular_result_file() {
        let directory = make_test_directory("guest-result");
        let run_id = "0123456789abcdef0123456789abcdef";
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), guest_result_json("passed", r#","exitCode":0"#)).unwrap();
        let result = read_verified_guest_result_file(
            &directory, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
        ).unwrap();
        assert_eq!(result.status, GuestRunStatus::Passed);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn guest_result_file_reader_rejects_extra_missing_oversized_and_linked_entries() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let directory = make_test_directory("guest-result-invalid");
        assert!(read_verified_guest_result_file(
            &directory, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
        ).is_err());
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), guest_result_json("passed", r#","exitCode":0"#)).unwrap();
        fs::write(directory.join("unexpected.txt"), b"extra").unwrap();
        assert!(read_verified_guest_result_file(
            &directory, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
        ).is_err());
        fs::remove_file(directory.join("unexpected.txt")).unwrap();
        fs::write(directory.join(GUEST_RESULT_FILE_NAME), vec![b'x'; MAX_GUEST_RESULT_BYTES + 1]).unwrap();
        assert!(read_verified_guest_result_file(
            &directory, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
        ).is_err());
        fs::remove_file(directory.join(GUEST_RESULT_FILE_NAME)).unwrap();
        #[cfg(unix)]
        {
            let outside = directory.join("outside.json");
            fs::write(&outside, guest_result_json("passed", r#","exitCode":0"#)).unwrap();
            std::os::unix::fs::symlink(&outside, directory.join(GUEST_RESULT_FILE_NAME)).unwrap();
            assert!(read_verified_guest_result_file(
                &directory, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest, MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
            ).is_err());
        }
        fs::remove_dir_all(directory).unwrap();
    }


    #[cfg(unix)]
    #[test]
    fn guest_result_file_reader_rejects_non_text_entry_names() {
        use std::os::unix::ffi::OsStringExt;
        let directory = make_test_directory("guest-result-non-text-name");
        let invalid_name = std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', 0xff]);
        fs::write(directory.join(invalid_name), b"unexpected").unwrap();
        assert!(read_verified_guest_result_file(
            &directory,
            "0123456789abcdef0123456789abcdef",
            "abcdef0123456789abcdef0123456789",
            GuestRunOperation::RustCargoTest,
            MAX_GUEST_RESULT_BYTES as u64, MAX_GUEST_RESULT_DURATION_MS,
        ).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn guest_result_file_reader_rejects_symlinked_result_directory() {
        let parent = make_test_directory("guest-result-directory-link");
        let real_directory = parent.join("real");
        fs::create_dir(&real_directory).unwrap();
        let linked_directory = parent.join("linked");
        std::os::unix::fs::symlink(&real_directory, &linked_directory).unwrap();
        assert!(read_verified_guest_result_file(
            &linked_directory,
            "0123456789abcdef0123456789abcdef",
            "abcdef0123456789abcdef0123456789",
            GuestRunOperation::RustCargoTest,
            MAX_GUEST_RESULT_BYTES as u64,
            MAX_GUEST_RESULT_DURATION_MS,
        ).is_err());
        fs::remove_file(linked_directory).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn guest_result_accepts_only_matching_bounded_success_contract() {
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        let result = validate_guest_result_json(
            &bytes,
            "0123456789abcdef0123456789abcdef",
            "abcdef0123456789abcdef0123456789",
            GuestRunOperation::RustCargoTest,
        ).unwrap();
        assert_eq!(result.status, GuestRunStatus::Passed);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn guest_result_rejects_malformed_unknown_duplicate_and_mismatched_data() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        assert!(validate_guest_result_json(b"{", run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        let unknown = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":1,"extra":true}"#;
        assert!(validate_guest_result_json(unknown, run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        let duplicate = br#"{"schemaVersion":1,"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":1}"#;
        assert!(validate_guest_result_json(duplicate, run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        assert!(validate_guest_result_json(&bytes, "ffffffffffffffffffffffffffffffff", "abcdef0123456789abcdef0123456789", operation).is_err());
        assert!(validate_guest_result_json(&bytes, run_id, "abcdef0123456789abcdef0123456789", GuestRunOperation::PythonUnittest).is_err());
        let wrong_snapshot = "11111111111111111111111111111111";
        assert!(validate_guest_result_json(&bytes, run_id, wrong_snapshot, operation).is_err());
    }

    #[test]
    fn guest_result_rejects_inconsistent_status_exit_and_excessive_limits() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        assert!(validate_guest_result_json(&guest_result_json("passed", r#","exitCode":1"#), run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        assert!(validate_guest_result_json(&guest_result_json("failed", r#","exitCode":0"#), run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        assert!(validate_guest_result_json(&guest_result_json("timed_out", r#","exitCode":124"#), run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        let long_output = "x".repeat(MAX_GUEST_RESULT_BYTES + 1);
        let oversized = serde_json::json!({
            "schemaVersion": 1, "runId": run_id, "snapshotId": "abcdef0123456789abcdef0123456789", "operation": "rust-cargo-test",
            "status": "passed", "exitCode": 0, "stdout": long_output,
            "stderr": "", "truncated": true, "durationMs": 1
        });
        assert!(validate_guest_result_json(&serde_json::to_vec(&oversized).unwrap(), run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
        let long_duration = br#"{"schemaVersion":1,"runId":"0123456789abcdef0123456789abcdef","snapshotId":"abcdef0123456789abcdef0123456789","operation":"rust-cargo-test","status":"passed","exitCode":0,"truncated":false,"durationMs":120001}"#;
        assert!(validate_guest_result_json(long_duration, run_id, "abcdef0123456789abcdef0123456789", operation).is_err());
    }

    #[test]
    fn guest_result_enforces_the_exact_per_run_output_budget() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let operation = GuestRunOperation::RustCargoTest;
        let value = serde_json::json!({
            "schemaVersion": 1,
            "runId": run_id,
            "snapshotId": "abcdef0123456789abcdef0123456789",
            "operation": "rust-cargo-test",
            "status": "passed",
            "exitCode": 0,
            "stdout": "123456",
            "stderr": "7890",
            "truncated": false,
            "durationMs": 1
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, "abcdef0123456789abcdef0123456789", operation, 10).is_ok());
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, "abcdef0123456789abcdef0123456789", operation, 9).is_err());
        assert!(validate_guest_result_json_with_limit(&bytes, run_id, "abcdef0123456789abcdef0123456789", operation, 0).is_err());
        assert!(validate_guest_result_json_with_limit(
            &bytes, run_id, "abcdef0123456789abcdef0123456789", operation, MAX_GUEST_REQUEST_OUTPUT_BYTES + 1
        ).is_err());
    }

    #[test]
    fn guest_result_enforces_the_exact_per_run_duration_budget() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let snapshot_id = "abcdef0123456789abcdef0123456789";
        let mut value = serde_json::json!({
            "schemaVersion": 1,
            "runId": run_id,
            "snapshotId": snapshot_id,
            "operation": "rust-cargo-test",
            "status": "passed",
            "exitCode": 0,
            "stdout": "",
            "stderr": "",
            "truncated": false,
            "durationMs": 250
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(validate_guest_result_json_with_limits(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 1024, 250,
        ).is_ok());
        assert!(validate_guest_result_json_with_limits(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 1024, 249,
        ).is_err());

        value["durationMs"] = serde_json::json!(1);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(validate_guest_result_json_with_limits(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 1024, 0,
        ).is_err());
        assert!(validate_guest_result_json_with_limits(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 1024,
            MAX_GUEST_RESULT_DURATION_MS + 1,
        ).is_err());
    }

    #[test]
    fn guest_result_accepts_each_terminal_status_only_with_its_matching_exit_contract() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let snapshot_id = "abcdef0123456789abcdef0123456789";
        let operation = GuestRunOperation::RustCargoTest;
        let cases = [
            (GuestRunStatus::Passed, "passed", Some(0)),
            (GuestRunStatus::Failed, "failed", Some(1)),
            (GuestRunStatus::TimedOut, "timed_out", None),
            (GuestRunStatus::Cancelled, "cancelled", None),
            (GuestRunStatus::Unsupported, "unsupported", None),
            (GuestRunStatus::RunnerError, "runner_error", None),
        ];

        for (expected_status, status, exit_code) in cases {
            let value = serde_json::json!({
                "schemaVersion": 1,
                "runId": run_id,
                "snapshotId": snapshot_id,
                "operation": "rust-cargo-test",
                "status": status,
                "exitCode": exit_code,
                "stdout": null,
                "stderr": null,
                "truncated": true,
                "durationMs": MAX_GUEST_RESULT_DURATION_MS
            });
            let bytes = serde_json::to_vec(&value).unwrap();
            let result = validate_guest_result_json(
                &bytes, run_id, snapshot_id, operation,
            ).unwrap();
            assert_eq!(result.status, expected_status);
            assert_eq!(result.exit_code, exit_code);
            assert_eq!(result.run_id, run_id);
            assert_eq!(result.snapshot_id, snapshot_id);
            assert!(result.truncated);
            assert_eq!(result.duration_ms, MAX_GUEST_RESULT_DURATION_MS);
        }
    }

    #[test]
    fn guest_result_output_budget_counts_utf8_bytes_not_characters() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let snapshot_id = "abcdef0123456789abcdef0123456789";
        let value = serde_json::json!({
            "schemaVersion": 1,
            "runId": run_id,
            "snapshotId": snapshot_id,
            "operation": "rust-cargo-test",
            "status": "passed",
            "exitCode": 0,
            "stdout": "é",
            "stderr": "",
            "truncated": false,
            "durationMs": 1
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(validate_guest_result_json_with_limit(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 1,
        ).is_err());
        assert!(validate_guest_result_json_with_limit(
            &bytes, run_id, snapshot_id, GuestRunOperation::RustCargoTest, 2,
        ).is_ok());
    }

    #[test]
    fn guest_result_rejects_unknown_status_and_invalid_terminal_combinations() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let snapshot_id = "abcdef0123456789abcdef0123456789";
        let operation = GuestRunOperation::RustCargoTest;
        for (status, exit_code) in [
            ("complete", Some(0)),
            ("passed", None),
            ("failed", Some(0)),
            ("timed_out", Some(124)),
            ("cancelled", Some(130)),
            ("unsupported", Some(1)),
            ("runner_error", Some(1)),
        ] {
            let value = serde_json::json!({
                "schemaVersion": 1,
                "runId": run_id,
                "snapshotId": snapshot_id,
                "operation": "rust-cargo-test",
                "status": status,
                "exitCode": exit_code,
                "truncated": false,
                "durationMs": 1
            });
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(
                validate_guest_result_json(&bytes, run_id, snapshot_id, operation).is_err(),
                "status {status} with exit code {exit_code:?} must be rejected"
            );
        }
    }

    #[test]
    fn guest_result_rejects_oversized_or_invalid_expected_identifiers() {
        let bytes = guest_result_json("passed", r#","exitCode":0"#);
        assert!(validate_guest_result_json(&vec![b'x'; MAX_GUEST_RESULT_BYTES + 1], "0123456789abcdef0123456789abcdef", "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest).is_err());
        assert!(validate_guest_result_json(&bytes, "../bad", "abcdef0123456789abcdef0123456789", GuestRunOperation::RustCargoTest).is_err());
    }


    #[test]
    fn private_workspace_permissions_are_restricted_before_use() {
        let workspace = make_test_directory("private-workspace-permissions");
        secure_private_workspace_directory(&workspace).unwrap();
        let metadata = fs::symlink_metadata(&workspace).unwrap();
        assert!(metadata.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
        fs::remove_dir(&workspace).unwrap();
    }

    #[test]
    fn ownership_marker_allows_cleanup_only_for_matching_snapshot() {
        let workspace = make_test_directory("owned-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        write_snapshot_ownership_marker(&snapshot, id).unwrap();
        assert_eq!(verified_owned_snapshot_path(&workspace, id).unwrap(), fs::canonicalize(&snapshot).unwrap());
        remove_verified_owned_snapshot(&workspace, id).unwrap();
        assert!(!snapshot.exists());
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn ownership_recovery_refuses_missing_malformed_or_mismatched_markers() {
        let workspace = make_test_directory("bad-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), b"{malformed").unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), serde_json::to_vec(&SnapshotOwnershipMarker {
            schema_version: SNAPSHOT_OWNER_SCHEMA,
            snapshot_id: "ffffffffffffffffffffffffffffffff".into(),
            owner: "FRIDAY-CodingAssistant".into(),
        }).unwrap()).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(snapshot.exists(), "unverified folders must be preserved");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn ownership_recovery_refuses_oversized_marker_files() {
        let workspace = make_test_directory("oversized-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let snapshot = workspace.join(id);
        fs::create_dir(&snapshot).unwrap();
        fs::write(snapshot.join(SNAPSHOT_OWNER_MARKER), vec![b'x'; 1025]).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(snapshot.exists(), "oversized ownership evidence must not authorise deletion");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_recovery_refuses_symlinked_snapshot_directories() {
        let workspace = make_test_directory("linked-marker");
        let id = "0123456789abcdef0123456789abcdef";
        let real = workspace.join("real");
        fs::create_dir(&real).unwrap();
        write_snapshot_ownership_marker(&real, id).unwrap();
        std::os::unix::fs::symlink(&real, workspace.join(id)).unwrap();
        assert!(verified_owned_snapshot_path(&workspace, id).is_err());
        assert!(real.exists(), "the target of a redirected path must never be removed");
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn snapshot_copy_only_copies_eligible_regular_source_files() {
        let base = make_test_directory("allowlist");
        let source = base.join("project");
        let destination = base.join("snapshot");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(source.join("node_modules")).unwrap();
        fs::create_dir_all(source.join(".git")).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(source.join(".env"), "API_KEY=not-for-copy").unwrap();
        fs::write(source.join("node_modules/ignored.rs"), "ignored").unwrap();
        fs::write(source.join(".git/config"), "ignored").unwrap();
        fs::write(source.join("picture.png"), [0u8, 1, 2, 3]).unwrap();
        fs::write(source.join("oversized.rs"), vec![b'x'; (MAX_SNAPSHOT_FILE_BYTES + 1) as usize]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join("src/main.rs"), source.join("src/linked.rs")).unwrap();

        let canonical_source = fs::canonicalize(&source).unwrap();
        let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
        copy_snapshot_tree(&canonical_source, &canonical_source, &destination, 0, &mut budget).unwrap();

        assert_eq!(budget.files, 1);
        assert_eq!(fs::read_to_string(destination.join("src/main.rs")).unwrap(), "fn main() {}");
        assert!(!destination.join(".env").exists());
        assert!(!destination.join("node_modules").exists());
        assert!(!destination.join(".git").exists());
        assert!(!destination.join("picture.png").exists());
        assert!(!destination.join("oversized.rs").exists());
        #[cfg(unix)]
        assert!(!destination.join("src/linked.rs").exists());
        assert_eq!(fs::read_to_string(source.join("src/main.rs")).unwrap(), "fn main() {}");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn snapshot_copy_enforces_the_file_count_cap() {
        let base = make_test_directory("file-cap");
        let source = base.join("project");
        let destination = base.join("snapshot");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        for index in 0..=MAX_SNAPSHOT_FILES {
            fs::write(source.join(format!("file-{index:04}.rs")), "x").unwrap();
        }
        let canonical_source = fs::canonicalize(&source).unwrap();
        let mut budget = SnapshotBudget { files: 0, bytes: 0, truncated: false };
        copy_snapshot_tree(&canonical_source, &canonical_source, &destination, 0, &mut budget).unwrap();
        assert_eq!(budget.files, MAX_SNAPSHOT_FILES);
        assert!(budget.truncated);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_disables_networking_and_shares_snapshot_read_only() {
        let base = make_test_directory("wsb-config");
        let snapshot = base.join("snapshot");
        fs::create_dir(&snapshot).unwrap();
        let canonical = fs::canonicalize(&snapshot).unwrap();
        let config = build_sandbox_config(&canonical).unwrap();
        assert!(config.contains("<Networking>Disable</Networking>"));
        assert!(config.contains("<VGpu>Disable</VGpu>"));
        assert!(config.contains("<AudioInput>Disable</AudioInput>"));
        assert!(config.contains("<VideoInput>Disable</VideoInput>"));
        assert!(config.contains("<ClipboardRedirection>Disable</ClipboardRedirection>"));
        assert!(config.contains("<PrinterRedirection>Disable</PrinterRedirection>"));
        assert!(config.contains("<ProtectedClient>Enable</ProtectedClient>"));
        assert!(config.contains("<ReadOnly>true</ReadOnly>"));
        validate_sandbox_config_policy(&config).unwrap();
        assert!(config.contains(r"<SandboxFolder>C:\FRIDAY\Project</SandboxFolder>"));
        assert!(config.contains(canonical.to_str().unwrap()));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_staging_writes_exclusive_bounded_file_and_verifies_exact_readback() {
        let base = make_test_directory("wsb-config-stage");
        let config = safe_sandbox_config_with_result_fixture(
            r"C:\FRIDAY\Snapshots\abc",
            r"C:\FRIDAY\Runs\abc\result-mount",
        );
        let path = write_new_sandbox_config_file(&base, &config).unwrap();
        assert_eq!(path.file_name(), Some(OsStr::new(SANDBOX_CONFIG_FILE_NAME)));
        assert_eq!(fs::read_to_string(&path).unwrap(), config);
        assert!(validate_sandbox_config_with_result_policy(&fs::read_to_string(&path).unwrap()).is_ok());
        assert!(write_new_sandbox_config_file(&base, &config).is_err(),
            "a second staging attempt must never overwrite the first file");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_staging_rejects_invalid_oversized_and_non_directory_targets() {
        let base = make_test_directory("wsb-config-stage-invalid");
        let safe = safe_sandbox_config_with_result_fixture(
            r"C:\FRIDAY\Snapshots\abc",
            r"C:\FRIDAY\Runs\abc\result-mount",
        );
        assert!(write_new_sandbox_config_file(&base, &safe.replace(
            "<Networking>Disable</Networking>", "<Networking>Enable</Networking>",
        )).is_err());
        assert!(write_new_sandbox_config_file(&base, &"x".repeat(MAX_SANDBOX_CONFIG_BYTES + 1)).is_err());
        let file = base.join("not-a-directory");
        fs::write(&file, b"file").unwrap();
        assert!(write_new_sandbox_config_file(&file, &safe).is_err());
        assert!(!base.join(SANDBOX_CONFIG_FILE_NAME).exists(),
            "invalid input must not leave a staged configuration");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_config_staging_preserves_existing_or_interrupted_files() {
        let base = make_test_directory("wsb-config-stage-preserve");
        let path = base.join(SANDBOX_CONFIG_FILE_NAME);
        fs::write(&path, b"<partial").unwrap();
        let safe = safe_sandbox_config_with_result_fixture(
            r"C:\FRIDAY\Snapshots\abc",
            r"C:\FRIDAY\Runs\abc\result-mount",
        );
        assert!(write_new_sandbox_config_file(&base, &safe).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"<partial");
        fs::remove_dir_all(base).unwrap();
    }

    fn safe_sandbox_config_with_result_fixture(snapshot: &str, result: &str) -> String {
        format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#, xml_escape(snapshot), xml_escape(result))
    }

    #[test]
    fn two_folder_sandbox_policy_allows_only_read_only_project_and_bounded_result_mapping() {
        let safe = safe_sandbox_config_with_result_fixture(
            r"C:\FRIDAY\Snapshots\abc",
            r"C:\FRIDAY\Runs\abc\result-mount",
        );
        validate_sandbox_config_with_result_policy(&safe).unwrap();
        assert!(safe.contains("<ReadOnly>true</ReadOnly>"));
        assert!(safe.contains("<ReadOnly>false</ReadOnly>"));
        assert!(safe.contains(r"<SandboxFolder>C:\FRIDAY\Project</SandboxFolder>"));
        assert!(safe.contains(r"<SandboxFolder>C:\FRIDAY\Results</SandboxFolder>"));
    }

    #[test]
    fn two_folder_sandbox_policy_rejects_writable_project_and_read_only_results() {
        let safe = safe_sandbox_config_with_result_fixture(
            r"C:\FRIDAY\Snapshots\abc",
            r"C:\FRIDAY\Runs\abc\result-mount",
        );
        assert!(validate_sandbox_config_with_result_policy(
            &safe.replace("<ReadOnly>true</ReadOnly>", "<ReadOnly>false</ReadOnly>")
        ).is_err());
        assert!(validate_sandbox_config_with_result_policy(
            &safe.replace("<ReadOnly>false</ReadOnly>", "<ReadOnly>true</ReadOnly>")
        ).is_err());
    }

    #[test]
    fn two_folder_sandbox_policy_rejects_extra_mappings_and_network_or_xml_injection() {
        let safe = safe_sandbox_config_with_result_fixture(
            r#"C:\FRIDAY\Snapshot & <Project>"#,
            r#"C:\FRIDAY\Runs\abc\result-mount"#,
        );
        validate_sandbox_config_with_result_policy(&safe).unwrap();
        for mutation in [
            safe.replace("<Networking>Disable</Networking>", "<Networking>Enable</Networking>"),
            safe.replace("</MappedFolders>", "<MappedFolder></MappedFolder></MappedFolders>"),
            safe.replace("<SandboxFolder>C:\\FRIDAY\\Results</SandboxFolder>",
                "<SandboxFolder>C:\\Other</SandboxFolder>"),
            safe.replace("&amp;", "&bogus;"),
            safe.replace("</Configuration>", "<LogonCommand/></Configuration>"),
            safe.replace("<ReadOnly>false</ReadOnly>", "<ReadOnly>false</ReadOnly><ReadOnly>true</ReadOnly>"),
        ] {
            assert!(validate_sandbox_config_with_result_policy(&mutation).is_err());
        }
    }

    #[test]
    fn two_folder_sandbox_config_builder_rejects_unsafe_snapshot_and_result_paths() {
        let base = make_test_directory("wsb-dual-config");
        let snapshot = base.join("snapshot");
        fs::create_dir(&snapshot).unwrap();
        let canonical = fs::canonicalize(&snapshot).unwrap();
        let relative_result = PathBuf::from("relative-result-mount");
        assert!(build_sandbox_config_with_result_paths(&canonical, &relative_result).is_err(),
            "result mapping must be an absolute path");
        let result = base.join("run").join("result-mount");
        let config = build_sandbox_config_with_result_paths(&canonical, &result).unwrap();
        validate_sandbox_config_with_result_policy(&config).unwrap();
        assert!(config.contains("<ReadOnly>true</ReadOnly>"));
        assert!(config.contains("<ReadOnly>false</ReadOnly>"));
        fs::remove_dir_all(base).unwrap();
    }

    fn safe_sandbox_config_fixture(host_folder: &str) -> String {
        format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#, xml_escape(host_folder))
    }

    #[test]
    fn sandbox_config_policy_rejects_network_or_writable_share_regressions() {
        let safe = safe_sandbox_config_fixture(r"C:\FRIDAY\Snapshot");
        validate_sandbox_config_policy(&safe).unwrap();
        for (from, to) in [
            ("<Networking>Disable</Networking>", "<Networking>Enable</Networking>"),
            ("<ReadOnly>true</ReadOnly>", "<ReadOnly>false</ReadOnly>"),
            ("<ProtectedClient>Enable</ProtectedClient>", "<ProtectedClient>Disable</ProtectedClient>"),
        ] {
            assert!(validate_sandbox_config_policy(&safe.replace(from, to)).is_err());
        }
        assert!(validate_sandbox_config_policy(&safe.replace(
            "</MappedFolders>", "<MappedFolder></MappedFolder></MappedFolders>"
        )).is_err());
    }

    #[test]
    fn sandbox_config_policy_rejects_any_non_template_xml_structure() {
        let safe = safe_sandbox_config_fixture(r"C:\FRIDAY\Snapshot");
        for mutation in [
            safe.replace("<Networking>", "<Networking enabled='true'>"),
            safe.replace("</Configuration>", "<VGpu>Enable</VGpu></Configuration>"),
            safe.replace("</Configuration>", "<LogonCommand/></Configuration>"),
            safe.replace("</Configuration>", "<!-- injected --></Configuration>"),
            safe.replace("<HostFolder>", "<HostFolder><![CDATA["),
            safe.replace("</HostFolder>", "]]></HostFolder>"),
            safe.replace("<ReadOnly>true</ReadOnly>", "<ReadOnly>true</ReadOnly><ReadOnly>false</ReadOnly>"),
            safe.replace("<VGpu>Disable</VGpu>", "<Unknown>value</Unknown><VGpu>Disable</VGpu>"),
        ] {
            assert!(validate_sandbox_config_policy(&mutation).is_err(),
                "non-template XML structure was accepted");
        }
    }

    #[test]
    fn sandbox_config_policy_accepts_escaped_path_text_and_rejects_bad_entities() {
        let safe = safe_sandbox_config_fixture(r#"C:\FRIDAY\Work & <Finance> "Q1""#);
        validate_sandbox_config_policy(&safe).unwrap();
        let bad_entity = safe.replace("&amp;", "&bogus;");
        assert!(validate_sandbox_config_policy(&bad_entity).is_err());
        let raw_ampersand = safe.replace("&amp;", "&");
        assert!(validate_sandbox_config_policy(&raw_ampersand).is_err());
        let raw_tag = safe.replace("&lt;Finance&gt;", "<Networking>Enable</Networking>");
        assert!(validate_sandbox_config_policy(&raw_tag).is_err());
    }

    #[test]
    fn sandbox_config_escapes_xml_path_characters() {
        assert_eq!(xml_escape(r#"C:\\Work & <Test> "Folder""#),
            "C:\\\\Work &amp; &lt;Test&gt; &quot;Folder&quot;");
    }

    #[test]
    fn sandbox_config_refuses_missing_or_non_directory_paths() {
        let base = make_test_directory("wsb-invalid");
        let file = base.join("not-a-folder.rs");
        fs::write(&file, "x").unwrap();
        assert!(build_sandbox_config(&file).is_err());
        assert!(build_sandbox_config(&base.join("missing")).is_err());
        fs::remove_dir_all(base).unwrap();
    }


    fn valid_result_storage_journal() -> GuestResultStorageJournal {
        let journal = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        append_guest_result_storage_journal_entry(
            &journal, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap()
    }

    #[test]
    fn result_storage_journal_round_trips_and_preserves_recovery_state() {
        let journal = append_guest_result_storage_journal_entry(
            &valid_result_storage_journal(),
            GuestResultStorageEvent::PreserveForRecovery, true, false,
        ).unwrap();
        let bytes = encode_guest_result_storage_journal(&journal).unwrap();
        let parsed = parse_guest_result_storage_journal(&bytes).unwrap();
        assert_eq!(parsed, journal);
        assert_eq!(validate_guest_result_storage_journal(&parsed).unwrap(), GuestResultStorageState::RecoveryRequired);
        assert!(append_guest_result_storage_journal_entry(
            &parsed, GuestResultStorageEvent::DeleteVerified, true, true,
        ).is_err());
    }

    #[test]
    fn result_storage_journal_rejects_reordering_and_forged_transitions() {
        let mut journal = valid_result_storage_journal();
        journal.entries[0].sequence = 9;
        assert!(validate_guest_result_storage_journal(&journal).is_err());
        let mut journal = valid_result_storage_journal();
        journal.entries[0].next_state = GuestResultStorageState::Removed;
        assert!(validate_guest_result_storage_journal(&journal).is_err());
        let mut journal = valid_result_storage_journal();
        journal.run_id = "../foreign-run".into();
        assert!(validate_guest_result_storage_journal(&journal).is_err());
    }

    #[test]
    fn result_storage_journal_persists_immutable_generations_and_recovers_latest() {
        let root = make_test_directory("result-journal-durable");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let second = append_guest_result_storage_journal_entry(
            &first, GuestResultStorageEvent::PreserveForRecovery, true, false,
        ).unwrap();
        persist_guest_result_storage_journal(&root, &second).unwrap();
        let loaded = load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).unwrap();
        assert_eq!(loaded, second);
        assert!(persist_guest_result_storage_journal(&root, &second).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_preserves_interrupted_generation_for_recovery() {
        let root = make_test_directory("result-journal-interrupted");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let interrupted = guest_result_storage_journal_generation_path(&root, 2);
        fs::create_dir(&interrupted).unwrap();
        secure_private_workspace_directory(&interrupted).unwrap();
        assert!(load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).is_err());
        assert!(interrupted.exists(), "incomplete journal evidence must be preserved");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_rejects_generation_gaps_and_identity_substitution() {
        let root = make_test_directory("result-journal-gap");
        let first = valid_result_storage_journal();
        persist_guest_result_storage_journal(&root, &first).unwrap();
        let later = guest_result_storage_journal_generation_path(&root, 3);
        fs::create_dir(&later).unwrap();
        secure_private_workspace_directory(&later).unwrap();
        assert!(load_guest_result_storage_journal(
            &root, &first.run_id, &first.vhdx_identifier,
        ).is_err());
        assert!(load_guest_result_storage_journal(
            &root, "fedcba9876543210fedcba9876543210", &first.vhdx_identifier,
        ).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_appends_only_from_latest_persisted_state() {
        let root = make_test_directory("result-journal-append-gateway");
        let empty = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        let created = append_and_persist_guest_result_storage_transition(
            &root, &empty, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap();
        assert_eq!(validate_guest_result_storage_journal(&created).unwrap(), GuestResultStorageState::Created);
        let attached = append_and_persist_guest_result_storage_transition(
            &root, &created, GuestResultStorageEvent::AttachVerified, true, false,
        ).unwrap();
        assert_eq!(validate_guest_result_storage_journal(&attached).unwrap(), GuestResultStorageState::Attached);
        assert_eq!(load_guest_result_storage_journal(
            &root, &attached.run_id, &attached.vhdx_identifier,
        ).unwrap(), attached);
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &created, GuestResultStorageEvent::BeginMount, true, false,
        ).is_err(), "a stale branch must not append over a newer persisted generation");
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &attached, GuestResultStorageEvent::GrantShare, false, false,
        ).is_err(), "unverified ownership must not be journaled as a successful transition");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_append_gateway_refuses_preexisting_incomplete_generation() {
        let root = make_test_directory("result-journal-append-incomplete");
        let empty = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            vhdx_identifier: "12345678-1234-5678-9abc-def012345678".into(),
            entries: Vec::new(),
        };
        let interrupted = guest_result_storage_journal_generation_path(&root, 1);
        fs::create_dir(&interrupted).unwrap();
        secure_private_workspace_directory(&interrupted).unwrap();
        assert!(append_and_persist_guest_result_storage_transition(
            &root, &empty, GuestResultStorageEvent::CreateVerified, true, false,
        ).is_err());
        assert!(interrupted.exists(), "uncertain recovery evidence must be preserved");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_storage_journal_rejects_oversized_and_unknown_field_documents() {
        assert!(parse_guest_result_storage_journal(
            &vec![b'x'; MAX_GUEST_RESULT_STORAGE_JOURNAL_BYTES + 1]
        ).is_err());
        let mut bytes = encode_guest_result_storage_journal(&valid_result_storage_journal()).unwrap();
        bytes.pop();
        bytes.extend_from_slice(br#","unexpected":true}"#);
        assert!(parse_guest_result_storage_journal(&bytes).is_err());
    }

    #[test]
    fn result_storage_lifecycle_accepts_only_verified_ordered_happy_path() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        let mut state = S::Planned;
        for event in [E::CreateVerified, E::AttachVerified, E::BeginGptInitialization, E::CompleteGptInitialization,
            E::BeginNtfsFormat, E::CompleteNtfsFormat, E::BeginMount, E::CompleteMount,
            E::GrantShare, E::ConfirmGuestStopped, E::RevokeShare, E::BeginUnmount,
            E::CompleteUnmount, E::DetachVerified, E::DeleteVerified] {
            state = advance_guest_result_storage_state(state, event, true, event == E::ConfirmGuestStopped)
                .expect("valid lifecycle transition should succeed");
        }
        assert_eq!(state, S::Removed);
    }

    #[test]
    fn result_storage_lifecycle_refuses_cleanup_before_guest_stop_and_share_revocation() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Shared, E::RevokeShare, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Shared, E::DetachVerified, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::ShareRevoked, E::DetachVerified, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::UnmountInProgress, E::DetachVerified, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Mounted, E::DeleteVerified, true, true).is_err());
    }

    #[test]
    fn result_storage_lifecycle_requires_verified_ownership_for_every_action() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Planned, E::CreateVerified, false, false).is_err());
        assert!(advance_guest_result_storage_state(S::Detached, E::DeleteVerified, false, true).is_err());
    }

    #[test]
    fn result_storage_lifecycle_requires_positive_guest_stop_evidence() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert!(advance_guest_result_storage_state(S::Shared, E::ConfirmGuestStopped, true, false).is_err());
        assert_eq!(advance_guest_result_storage_state(S::Shared, E::ConfirmGuestStopped, true, true).unwrap(), S::GuestStopped);
    }

    #[test]
    fn result_storage_recovery_state_preserves_resources_and_blocks_automatic_actions() {
        use GuestResultStorageEvent as E;
        use GuestResultStorageState as S;
        assert_eq!(advance_guest_result_storage_state(S::Shared, E::PreserveForRecovery, false, false).unwrap(), S::RecoveryRequired);
        assert!(advance_guest_result_storage_state(S::RecoveryRequired, E::RevokeShare, true, true).is_err());
        assert!(advance_guest_result_storage_state(S::Removed, E::PreserveForRecovery, true, true).is_err());
    }

    fn valid_test_sandbox_identity() -> SandboxProcessIdentity {
        SandboxProcessIdentity {
            session_id: "0123456789abcdef0123456789abcdef".into(),
            process_id: 4321,
            process_creation_time: 987654321,
            image_path: r"C:\Windows\System32\WindowsSandbox.exe".into(),
        }
    }

    fn empty_test_sandbox_session_journal() -> SandboxSessionJournal {
        SandboxSessionJournal {
            schema_version: SANDBOX_SESSION_JOURNAL_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            entries: Vec::new(),
        }
    }

    #[test]
    fn sandbox_session_journal_persists_and_reloads_exact_owned_lifecycle() {
        use SandboxSessionEvent as E;
        let root = make_test_directory("session-journal");
        let identity = valid_test_sandbox_identity();
        let mut journal = empty_test_sandbox_session_journal();
        for (event, observed, owned, stopped) in [
            (E::RequestLaunch, None, false, false),
            (E::ObserveProcess, Some(identity.clone()), false, false),
            (E::ConfirmSession, Some(identity.clone()), true, false),
            (E::ConfirmRunning, Some(identity.clone()), true, false),
            (E::RequestStop, Some(identity.clone()), true, false),
            (E::ConfirmStopped, Some(identity.clone()), true, true),
            (E::RemoveOwnershipRecord, None, true, true),
        ] {
            journal = append_and_persist_sandbox_session_transition(
                &root, &journal, event, observed.as_ref(), owned, stopped,
            ).unwrap();
        }
        assert_eq!(validate_sandbox_session_journal(&journal).unwrap(), SandboxSessionState::Removed);
        assert_eq!(load_sandbox_session_journal(&root, &journal.run_id).unwrap(), journal);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_session_journal_rejects_tampering_gaps_and_stale_writers() {
        use SandboxSessionEvent as E;
        let root = make_test_directory("session-journal-tamper");
        let identity = valid_test_sandbox_identity();
        let mut journal = empty_test_sandbox_session_journal();
        journal = append_and_persist_sandbox_session_transition(
            &root, &journal, E::RequestLaunch, None, false, false,
        ).unwrap();
        let stale = empty_test_sandbox_session_journal();
        let next = append_and_persist_sandbox_session_transition(
            &root, &journal, E::ObserveProcess, Some(&identity), false, false,
        ).unwrap();
        assert!(append_and_persist_sandbox_session_transition(
            &root, &stale, E::RequestLaunch, None, false, false,
        ).is_err());
        assert_eq!(load_sandbox_session_journal(&root, &next.run_id).unwrap(), next);
        fs::remove_dir_all(sandbox_session_journal_generation_path(&root, 1)).unwrap();
        assert!(load_sandbox_session_journal(&root, &next.run_id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_session_journal_rejects_identity_substitution_and_unknown_fields() {
        use SandboxSessionEvent as E;
        let identity = valid_test_sandbox_identity();
        let mut journal = empty_test_sandbox_session_journal();
        journal = append_sandbox_session_journal_entry(
            &journal, E::RequestLaunch, None, false, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, E::ObserveProcess, Some(&identity), false, false,
        ).unwrap();
        let mut replacement = identity.clone();
        replacement.process_creation_time += 1;
        assert!(append_sandbox_session_journal_entry(
            &journal, E::ConfirmSession, Some(&replacement), true, false,
        ).is_err());
        let bytes = encode_sandbox_session_journal(&journal).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(parse_sandbox_session_journal(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn sandbox_session_accepts_only_exact_owned_lifecycle() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        let mut state = S::Planned;
        state = advance_sandbox_session(state, E::RequestLaunch, None, None, false, false).unwrap();
        state = advance_sandbox_session(state, E::ObserveProcess, None, Some(&identity), false, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmSession, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmRunning, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::RequestStop, Some(&identity), Some(&identity), true, false).unwrap();
        state = advance_sandbox_session(state, E::ConfirmStopped, Some(&identity), Some(&identity), true, true).unwrap();
        state = advance_sandbox_session(state, E::RemoveOwnershipRecord, Some(&identity), None, true, true).unwrap();
        assert_eq!(state, S::Removed);
    }

    #[test]
    fn sandbox_session_rejects_invalid_process_identity_and_image_path() {
        let mut identity = valid_test_sandbox_identity();
        assert!(validate_sandbox_process_identity(&identity).is_ok());
        identity.process_id = 0;
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.process_id = 4321;
        identity.process_creation_time = 0;
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.process_creation_time = 1;
        identity.image_path = r"C:\\Windows\\System32\\cmd.exe".into();
        assert!(validate_sandbox_process_identity(&identity).is_err());
        identity.image_path = r"C:\\Windows\\..\\Other\\WindowsSandbox.exe".into();
        assert!(validate_sandbox_process_identity(&identity).is_err());
    }

    #[test]
    fn sandbox_session_rejects_pid_reuse_and_foreign_process_substitution() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        let mut replacement = identity.clone();
        replacement.process_creation_time += 1;
        assert!(advance_sandbox_session(S::ProcessObserved, E::ConfirmSession,
            Some(&identity), Some(&replacement), true, false).is_err());
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&replacement), true, false).is_err());
        replacement = identity.clone();
        replacement.session_id = "fedcba9876543210fedcba9876543210".into();
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&replacement), true, false).is_err());
    }

    #[test]
    fn sandbox_session_refuses_stop_without_ownership_or_exit_evidence() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        assert!(advance_sandbox_session(S::Running, E::RequestStop,
            Some(&identity), Some(&identity), false, false).is_err());
        assert!(advance_sandbox_session(S::StopRequested, E::ConfirmStopped,
            Some(&identity), Some(&identity), true, false).is_err());
        assert!(advance_sandbox_session(S::StopRequested, E::ConfirmStopped,
            Some(&identity), None, true, true).is_err());
    }

    #[test]
    fn sandbox_session_recovery_state_blocks_all_automatic_actions() {
        use SandboxSessionEvent as E;
        use SandboxSessionState as S;
        let identity = valid_test_sandbox_identity();
        assert_eq!(advance_sandbox_session(S::Running, E::PreserveForRecovery,
            Some(&identity), Some(&identity), false, false).unwrap(), S::RecoveryRequired);
        assert!(advance_sandbox_session(S::RecoveryRequired, E::RequestStop,
            Some(&identity), Some(&identity), true, true).is_err());
        assert!(advance_sandbox_session(S::Removed, E::PreserveForRecovery,
            Some(&identity), Some(&identity), true, true).is_err());
    }

    #[test]
    fn sandbox_preflight_never_advertises_execution_or_network_isolation_early() {
        let policy = CodingSandboxPreflight {
            platform_supported: true,
            runtime_detected: true,
            execution_enabled: false,
            network_isolation_enforced: false,
            windows_edition: "Professional".into(),
            windows_build: "26100".into(),
            sandbox_feature_state: "Enabled".into(),
            wsb_cli_available: true,
            wsb_cli_version: "1.0.0".into(),
            virtualization_firmware_enabled: Some(true),
            sandbox_process_detected: false,
            virtual_disk_api_available: true,
            manage_volume_privilege_enabled: true,
            hyperv_cmdlets_available: true,
            mapped_folders_policy: "Not configured".into(),
            mapped_folder_write_policy: "Not configured".into(),
            result_storage_ready: false,
            message: "Preflight only".into(),
        };
        assert!(!policy.execution_enabled);
        assert!(!policy.network_isolation_enforced);
    }

    #[test]
    fn sandbox_policy_probe_distinguishes_blocked_allowed_and_unconfigured() {
        assert_eq!(sandbox_policy_state(None), "Not configured");
        assert_eq!(sandbox_policy_state(Some(&serde_json::Value::Null)), "Not configured");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(0))), "Blocked");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(1))), "Allowed");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!(2))), "Unknown");
        assert_eq!(sandbox_policy_state(Some(&serde_json::json!("1"))), "Unknown");
    }

    #[test]
    fn volume_management_privilege_probe_requires_exact_enabled_privilege() {
        let enabled = "Privilege Name Description State\nSeManageVolumePrivilege Perform volume maintenance tasks Enabled\n";
        let disabled = "SeManageVolumePrivilege Perform volume maintenance tasks Disabled\n";
        assert!(privilege_output_has_enabled(enabled, "SeManageVolumePrivilege"));
        assert!(!privilege_output_has_enabled(disabled, "SeManageVolumePrivilege"));
        assert!(!privilege_output_has_enabled(enabled, "SeBackupPrivilege"));
        assert!(!privilege_output_has_enabled("", "SeManageVolumePrivilege"));
    }

    #[test]
    fn storage_preflight_never_marks_unverified_storage_ready() {
        let policy = CodingSandboxPreflight {
            platform_supported: true, runtime_detected: true, execution_enabled: false,
            network_isolation_enforced: false, windows_edition: "Professional".into(),
            windows_build: "26100".into(), sandbox_feature_state: "Enabled".into(),
            wsb_cli_available: true, wsb_cli_version: "1.0.0".into(),
            virtualization_firmware_enabled: Some(true), sandbox_process_detected: false,
            virtual_disk_api_available: true, manage_volume_privilege_enabled: true,
            hyperv_cmdlets_available: true,
            mapped_folders_policy: "Not configured".into(),
            mapped_folder_write_policy: "Not configured".into(),
            result_storage_ready: false,
            message: "Detection only.".into(),
        };
        assert!(!policy.result_storage_ready);
        assert!(!policy.execution_enabled);
    }

    #[test]
    fn windows_sandbox_detection_never_treats_a_missing_root_as_available() {
        assert!(!detect_windows_sandbox_at(None));
    }

    #[test]
    fn snapshot_allowlist_excludes_secrets_and_binary_assets() {
        assert!(safe_project_file_name("main.rs"));
        assert!(safe_project_file_name("package.json"));
        assert!(!safe_project_file_name(".env"));
        assert!(!safe_project_file_name("client-secret.json"));
        assert!(!safe_project_file_name("id_rsa"));
        assert!(!safe_project_file_name("image.png"));
        assert!(!safe_project_file_name("application.exe"));
    }

    #[test]
    fn snapshot_skips_hidden_and_dependency_directories() {
        for name in [".git", ".vscode", "node_modules", "target", "dist", "vendor"] {
            assert!(excluded_directory(name));
        }
        assert!(!excluded_directory("src"));
    }

    #[test]
    fn snapshot_paths_must_be_relative_normal_components() {
        assert!(safe_relative_path(Path::new("src/main.rs")));
        assert!(!safe_relative_path(Path::new("../outside.rs")));
        assert!(!safe_relative_path(Path::new("/absolute/path")));
    }

    #[test]
    fn result_volume_plan_uses_fixed_capacity_and_guest_mount() {
        let root = make_test_directory("result-volume-plan");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        assert_eq!(plan.run_id, run_id);
        assert_eq!(plan.storage_root, fs::canonicalize(&root).unwrap());
        assert_eq!(plan.run_directory, plan.storage_root.join(run_id));
        assert_eq!(plan.vhdx_path, plan.run_directory.join("result.vhdx"));
        assert_eq!(plan.capacity_bytes, 64 * 1024 * 1024);
        assert_eq!(plan.max_result_bytes, MAX_GUEST_RESULT_BYTES as u64);
        assert_eq!(plan.guest_mount_path, r"C:\FRIDAY\Results");
        validate_guest_result_volume_plan(&plan, &root, run_id).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_plan_rejects_untrusted_paths_and_reused_run_directories() {
        let root = make_test_directory("result-volume-invalid");
        assert!(build_guest_result_volume_plan(&root, "../outside").is_err());
        assert!(build_guest_result_volume_plan(Path::new("relative"), "0123456789abcdef0123456789abcdef").is_err());
        let run_id = "0123456789abcdef0123456789abcdef";
        fs::create_dir(root.join(run_id)).unwrap();
        assert!(build_guest_result_volume_plan(&root, run_id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_plan_rejects_mutated_capacity_paths_and_guest_mount() {
        let root = make_test_directory("result-volume-tamper");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        let mut changed = plan.clone();
        changed.capacity_bytes *= 2;
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        changed = plan.clone();
        changed.vhdx_path = root.join("outside.vhdx");
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        changed = plan.clone();
        changed.guest_mount_path = r"C:\FRIDAY\Project".into();
        assert!(validate_guest_result_volume_plan(&changed, &root, run_id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_volume_plan_rejects_symlinked_storage_root() {
        let base = make_test_directory("result-volume-link");
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(build_guest_result_volume_plan(&link, "0123456789abcdef0123456789abcdef").is_err());
        fs::remove_dir_all(base).unwrap();
    }


    #[test]
    fn result_volume_directory_is_exclusively_reserved_and_verified() {
        let root = make_test_directory("result-volume-reserve");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        let reserved = reserve_guest_result_volume_directory(&plan, &root).unwrap();
        assert_eq!(reserved, plan.run_directory);
        let directory_metadata = fs::symlink_metadata(&reserved).unwrap();
        assert!(directory_metadata.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(directory_metadata.permissions().mode() & 0o077, 0);
        }
        assert!(reserved.join(GUEST_RESULT_VOLUME_OWNER_MARKER).is_file());
        assert_eq!(verify_guest_result_volume_directory(&plan, &root).unwrap(), reserved);
        assert!(reserve_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_result_vhdx_creation_produces_fixed_private_vhdx_and_refuses_reuse() {
        let root = make_test_directory("native-result-vhdx");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let created = create_guest_result_vhdx(&plan, &root).unwrap();
        assert_eq!(created, plan.vhdx_path);
        let metadata = fs::symlink_metadata(&created).unwrap();
        assert!(metadata.is_file());
        assert!(!is_reparse_or_symlink(&metadata));
        assert!(metadata.len() >= 8);
        let mut file = open_source_file(&created).unwrap();
        let mut signature = [0u8; 8];
        file.read_exact(&mut signature).unwrap();
        assert_eq!(&signature, b"vhdxfile");
        let identifier = verify_guest_result_vhdx_capacity(&created, GUEST_RESULT_VOLUME_CAPACITY_BYTES).unwrap();
        assert!(valid_guest_vhdx_identifier(&identifier));
        assert_eq!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some(&identifier)).unwrap(), identifier);
        assert_eq!(verify_guest_result_vhdx_identity(&plan, &root).unwrap(), identifier);
        let attached = attach_guest_result_vhdx(&plan, &root).unwrap();
        assert!(attached.physical_path().starts_with(r"\\.\PhysicalDrive"));
        let journal_root = root.join("journal");
        fs::create_dir(&journal_root).unwrap();
        secure_private_workspace_directory(&journal_root).unwrap();
        let empty_journal = GuestResultStorageJournal {
            schema_version: GUEST_RESULT_STORAGE_JOURNAL_SCHEMA,
            run_id: run_id.into(),
            vhdx_identifier: identifier.clone(),
            entries: Vec::new(),
        };
        let created_journal = append_and_persist_guest_result_storage_transition(
            &journal_root, &empty_journal, GuestResultStorageEvent::CreateVerified, true, false,
        ).unwrap();
        let attached_journal = append_and_persist_guest_result_storage_transition(
            &journal_root, &created_journal, GuestResultStorageEvent::AttachVerified, true, false,
        ).unwrap();
        let partitioned_journal = initialize_guest_result_gpt(
            &attached, &plan, &root, &journal_root, &attached_journal,
        ).unwrap();
        assert_eq!(
            validate_guest_result_storage_journal(&partitioned_journal).unwrap(),
            GuestResultStorageState::Partitioned,
        );
        let (formatted_journal, volume_path) = format_guest_result_ntfs(
            &attached, &plan, &root, &journal_root, &partitioned_journal,
        ).unwrap();
        assert_eq!(
            validate_guest_result_storage_journal(&formatted_journal).unwrap(),
            GuestResultStorageState::Formatted,
        );
        assert!(valid_windows_volume_guid_path(&volume_path));

        let mount_directory = reserve_guest_result_host_mount_directory(&plan, &root).unwrap();
        let mounted_journal = mount_guest_result_volume_journaled(
            &attached, &plan, &root, &journal_root, &formatted_journal, &volume_path,
        ).unwrap();
        assert_eq!(
            validate_guest_result_storage_journal(&mounted_journal).unwrap(),
            GuestResultStorageState::Mounted,
        );
        verify_guest_result_volume_mount(&plan, &root, &volume_path).unwrap();
        let marker = mount_directory.join("mount-roundtrip.txt");
        fs::write(&marker, b"FRIDAY result volume mount verification").unwrap();
        assert_eq!(fs::read(&marker).unwrap(), b"FRIDAY result volume mount verification");
        fs::remove_file(&marker).unwrap();
        unmount_guest_result_volume_from_private_directory(&plan, &root, &volume_path).unwrap();
        let mount_path = format!("{}\\", mount_directory.to_string_lossy().trim_end_matches(['\\', '/']));
        let mount_wide = windows_wide_null(&mount_path).unwrap();
        let mut observed_volume = [0u16; 1024];
        assert_eq!(unsafe { GetVolumeNameForVolumeMountPointW(mount_wide.as_ptr(), observed_volume.as_mut_ptr(), observed_volume.len() as u32) }, 0);
        attached.detach().unwrap();
        assert!(mount_directory.is_dir());
        assert_eq!(fs::read_dir(&mount_directory).unwrap().count(), 0);

        assert!(create_guest_result_vhdx(&plan, &root).is_err(),
            "a pre-existing result VHDX must never be replaced");
        drop(file);
        fs::remove_file(created).unwrap();
        fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn result_host_mount_directory_is_private_empty_and_exact() {
        let root = make_test_directory("result-host-mount");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let mount = reserve_guest_result_host_mount_directory(&plan, &root).unwrap();
        assert_eq!(mount, plan.run_directory.join("result-mount"));
        assert_eq!(verify_guest_result_host_mount_directory(&plan, &root).unwrap(), mount);
        fs::write(mount.join("unexpected.txt"), b"not empty").unwrap();
        assert!(verify_guest_result_host_mount_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_host_mount_directory_refuses_preexisting_path() {
        let root = make_test_directory("result-host-mount-existing");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        fs::create_dir(&plan.host_mount_directory).unwrap();
        assert!(reserve_guest_result_host_mount_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_vhdx_file_path_requires_exact_owned_regular_vhdx_file() {
        let root = make_test_directory("result-vhdx-file-path");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        fs::write(&plan.vhdx_path, b"vhdxfile-test").unwrap();
        assert_eq!(
            verify_guest_result_vhdx_file_path(&plan, &root).unwrap(),
            fs::canonicalize(&plan.vhdx_path).unwrap()
        );
        fs::remove_file(&plan.vhdx_path).unwrap();
        fs::write(plan.run_directory.join("unexpected.vhdx"), b"vhdxfile-test").unwrap();
        assert!(verify_guest_result_vhdx_file_path(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_vhdx_file_path_rejects_symlink_substitution() {
        let root = make_test_directory("result-vhdx-file-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let outside = root.join("outside-result.vhdx");
        fs::write(&outside, b"vhdxfile-test").unwrap();
        std::os::unix::fs::symlink(&outside, &plan.vhdx_path).unwrap();
        assert!(verify_guest_result_vhdx_file_path(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_vhdx_identity_marker_binds_exact_run_capacity_and_identifier() {
        let marker = GuestResultVhdxOwnershipMarker {
            schema_version: GUEST_RESULT_VHDX_OWNER_SCHEMA,
            run_id: "0123456789abcdef0123456789abcdef".into(),
            owner: "FRIDAY-CodingAssistant-ResultVhdx".into(),
            identifier: "12345678-1234-5678-9abc-def012345678".into(),
            capacity_bytes: GUEST_RESULT_VOLUME_CAPACITY_BYTES,
        };
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "0123456789abcdef0123456789abcdef", Some("12345678-1234-5678-9abc-def012345678")
        ).is_ok());
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "ffffffffffffffffffffffffffffffff", None
        ).is_err());
        assert!(validate_guest_result_vhdx_ownership_marker(
            &marker, "0123456789abcdef0123456789abcdef", Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        ).is_err());
        let mut wrong_capacity = marker;
        wrong_capacity.capacity_bytes += 512;
        assert!(validate_guest_result_vhdx_ownership_marker(
            &wrong_capacity, "0123456789abcdef0123456789abcdef", None
        ).is_err());
    }

    #[test]
    fn result_vhdx_identity_marker_is_exclusive_and_rejects_tampering() {
        let root = make_test_directory("result-vhdx-identity");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let identifier = "12345678-1234-5678-9abc-def012345678";
        write_guest_result_vhdx_ownership_marker(&plan, &root, identifier).unwrap();
        assert_eq!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some(identifier)).unwrap(), identifier);
        assert!(write_guest_result_vhdx_ownership_marker(&plan, &root, identifier).is_err());
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")).is_err());
        fs::remove_file(plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER)).unwrap();
        fs::write(plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER),
            br#"{"schemaVersion":1,"runId":"ffffffffffffffffffffffffffffffff","owner":"FRIDAY-CodingAssistant-ResultVhdx","identifier":"12345678-1234-5678-9abc-def012345678","capacityBytes":67108864}"#).unwrap();
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_vhdx_identity_marker_rejects_symlink() {
        let root = make_test_directory("result-vhdx-identity-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        reserve_guest_result_volume_directory(&plan, &root).unwrap();
        let outside = root.join("outside-vhdx-marker");
        fs::write(&outside, b"not ownership evidence").unwrap();
        std::os::unix::fs::symlink(&outside, plan.run_directory.join(GUEST_RESULT_VHDX_OWNER_MARKER)).unwrap();
        assert!(verify_guest_result_vhdx_ownership_marker(&plan, &root, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_volume_directory_refuses_missing_or_tampered_ownership_evidence() {
        let root = make_test_directory("result-volume-marker");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        fs::create_dir(&plan.run_directory).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::write(plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER),
            br#"{"schemaVersion":1,"runId":"ffffffffffffffffffffffffffffffff","owner":"FRIDAY-CodingAssistant-ResultVolume"}"#).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn result_volume_directory_refuses_symlinked_ownership_marker() {
        let root = make_test_directory("result-volume-marker-link");
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_volume_plan(&root, run_id).unwrap();
        fs::create_dir(&plan.run_directory).unwrap();
        let outside = root.join("outside-marker");
        fs::write(&outside, b"not ownership evidence").unwrap();
        std::os::unix::fs::symlink(&outside, plan.run_directory.join(GUEST_RESULT_VOLUME_OWNER_MARKER)).unwrap();
        assert!(verify_guest_result_volume_directory(&plan, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn result_partition_plan_is_single_aligned_fixed_ntfs_partition() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        assert_eq!(plan.partition_number, 1);
        assert_eq!(plan.first_lba, 2048);
        assert_eq!(plan.last_lba, (GUEST_RESULT_VOLUME_CAPACITY_BYTES / 512) - 35);
        assert_eq!(plan.partition_type_guid, GUEST_RESULT_PARTITION_TYPE_GUID);
        assert_eq!(plan.partition_unique_guid, "01234567-89AB-CDEF-0123-456789ABCDEF");
        assert_eq!(plan.filesystem, "NTFS");
        assert_eq!(plan.filesystem_label, GUEST_RESULT_FILESYSTEM_LABEL);
        assert_eq!(plan.first_lba % GUEST_RESULT_GPT_ALIGNMENT_SECTORS, 0);
        validate_guest_result_partition_plan(&plan, run_id).unwrap();
    }

    #[test]
    fn result_partition_plan_rejects_capacity_sector_and_identifier_substitution() {
        let run_id = "0123456789abcdef0123456789abcdef";
        assert!(build_guest_result_partition_plan(run_id, 128 * 1024 * 1024, 512).is_err());
        assert!(build_guest_result_partition_plan(run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 4096).is_err());
        assert!(build_guest_result_partition_plan("../outside", GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512).is_err());
        let mut plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        plan.last_lba -= 1;
        assert!(validate_guest_result_partition_plan(&plan, run_id).is_err());
        plan = build_guest_result_partition_plan(
            run_id, GUEST_RESULT_VOLUME_CAPACITY_BYTES, 512
        ).unwrap();
        plan.partition_unique_guid = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".into();
        assert!(validate_guest_result_partition_plan(&plan, run_id).is_err());
    }


    #[test]
    fn windows_sandbox_cli_signature_evidence_requires_exact_valid_microsoft_publisher() {
        assert!(windows_sandbox_cli_signature_evidence_valid("Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_signature_evidence_valid("NotSigned", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_signature_evidence_valid("UnknownError", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_signature_evidence_valid("Valid", "Unknown Publisher"));
        assert!(!windows_sandbox_cli_signature_evidence_valid("valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_signature_evidence_valid("Valid", "Microsoft Corporation "));
    }

    #[test]
    fn windows_sandbox_cli_runtime_requires_supported_build_and_numeric_file_version() {
        let path = r"C:\Users\Friday\AppData\Local\Microsoft\WindowsApps\wsb.exe";
        assert!(valid_windows_sandbox_cli_executable_path(path));
        assert!(windows_sandbox_cli_runtime_supported(
            "26100", path, "10.0.26100.1", "Valid", "Microsoft Corporation",
        ));
        assert!(windows_sandbox_cli_runtime_supported(
            "26200", path, "10.0.26200.100", "Valid", "Microsoft Corporation",
        ));
        assert!(!windows_sandbox_cli_runtime_supported("26099", path, "10.0.26100.1", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("Unknown", path, "10.0.26100.1", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", "relative\\wsb.exe", "10.0.26100.1", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", r"C:\\Temp\\other.exe", "10.0.26100.1", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", path, "", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", path, "Unknown", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", path, "10.x.26100", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported("26100", path, "0.0.0.0", "Valid", "Microsoft Corporation"));
        assert!(!windows_sandbox_cli_runtime_supported(
            "26100", path, "10.0.26100.1", "NotSigned", "Microsoft Corporation",
        ));
        assert!(!windows_sandbox_cli_runtime_supported(
            "26100", path, "10.0.26100.1", "Valid", "Unknown Publisher",
        ));
        assert!(!windows_sandbox_cli_runtime_supported(
            "26100", path, "10.0.26100.1", "Unknown", "Microsoft Corporation",
        ));
    }

    #[test]
    fn windows_sandbox_cli_plan_uses_explicit_run_bound_session_and_fixed_verbs() {
        let executable = r"C:\Program Files\Windows Sandbox\wsb.exe";
        let run_id = "0123456789abcdef0123456789abcdef";
        let config = build_sandbox_config_with_result_paths(
            Path::new(r"C:\FRIDAY\snapshot"),
            Path::new(r"C:\FRIDAY\result-mount"),
        );
        // Exercise the exact production validator with a valid two-folder fixture.
        let config = config.unwrap_or_else(|_| {
            let snapshot = xml_escape(r"C:\FRIDAY\snapshot");
            let result = xml_escape(r"C:\FRIDAY\result-mount");
            format!(r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>{snapshot}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>{result}</HostFolder>
      <SandboxFolder>C:\FRIDAY\Results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#)
        });
        let start = build_windows_sandbox_cli_plan(
            executable, run_id, WindowsSandboxCliCommand::Start, Some(&config),
        ).unwrap();
        assert_eq!(start.sandbox_id, "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(&start.arguments[..5], ["start", "--raw", "--id", start.sandbox_id.as_str(), "--config"]);
        assert_eq!(start.arguments[5], config);
        validate_windows_sandbox_cli_plan(
            &start, executable, run_id, WindowsSandboxCliCommand::Start, Some(&config),
        ).unwrap();

        let list = build_windows_sandbox_cli_plan(
            executable, run_id, WindowsSandboxCliCommand::List, None,
        ).unwrap();
        assert_eq!(list.arguments, vec!["list", "--raw"]);

        let stop = build_windows_sandbox_cli_plan(
            executable, run_id, WindowsSandboxCliCommand::Stop, None,
        ).unwrap();
        assert_eq!(stop.arguments, vec!["stop".to_string(), "--raw".to_string(), "--id".to_string(), start.sandbox_id.clone()]);
    }

    #[test]
    fn windows_sandbox_cli_plan_rejects_substitution_and_unvalidated_config() {
        let executable = r"C:\WindowsApps\wsb.exe";
        let run_id = "0123456789abcdef0123456789abcdef";
        let valid_config = r#"<Configuration>
  <VGpu>Disable</VGpu>
  <Networking>Disable</Networking>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
  <ProtectedClient>Enable</ProtectedClient>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>C:\FRIDAY\snapshot</HostFolder>
      <SandboxFolder>C:\FRIDAY\Project</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>C:\FRIDAY\result-mount</HostFolder>
      <SandboxFolder>C:\FRIDAY\Results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"#;
        assert!(build_windows_sandbox_cli_plan(
            r"C:\WindowsApps\other.exe", run_id, WindowsSandboxCliCommand::List, None,
        ).is_err());
        assert!(build_windows_sandbox_cli_plan(
            executable, "../0123456789abcdef0123456789abcdef", WindowsSandboxCliCommand::List, None,
        ).is_err());
        let mut plan = build_windows_sandbox_cli_plan(
            executable, run_id, WindowsSandboxCliCommand::Start, Some(valid_config),
        ).unwrap();
        plan.arguments.push("--network".into());
        assert!(validate_windows_sandbox_cli_plan(
            &plan, executable, run_id, WindowsSandboxCliCommand::Start, Some(valid_config),
        ).is_err());
        assert!(build_windows_sandbox_cli_plan(
            executable, run_id, WindowsSandboxCliCommand::Start, Some("<Configuration/>"),
        ).is_err());
    }

    #[test]
    fn windows_sandbox_cli_list_parser_accepts_exact_session_inventory() {
        let bytes = br#"{"WindowsSandboxEnvironments":[{"Id":"01234567-89ab-cdef-0123-456789abcdef","Status":"Running"},{"Id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","State":"Stopped"}]}"#;
        let sessions = parse_windows_sandbox_cli_list(bytes).unwrap();
        assert_eq!(sessions.len(), 2);
        verify_windows_sandbox_cli_session(
            &sessions, "01234567-89ab-cdef-0123-456789abcdef",
            WindowsSandboxCliSessionStatus::Running,
        ).unwrap();
        verify_windows_sandbox_cli_session(
            &sessions, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            WindowsSandboxCliSessionStatus::Stopped,
        ).unwrap();
        assert!(verify_windows_sandbox_cli_session(
            &sessions, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            WindowsSandboxCliSessionStatus::Running,
        ).is_err());
    }


    #[test]
    fn bounded_command_output_retains_exact_limit_without_truncation() {
        let input = b"12345678";
        let output = read_bounded_command_output(&input[..], 8).unwrap();
        assert_eq!(output.bytes, input);
        assert!(!output.truncated);
    }

    #[test]
    fn bounded_command_output_marks_truncation_and_still_drains_input() {
        let input = vec![b'x'; 64 * 1024];
        let output = read_bounded_command_output(&input[..], 1024).unwrap();
        assert_eq!(output.bytes.len(), 1024);
        assert!(output.truncated);
        assert_eq!(output.bytes, vec![b'x'; 1024]);
    }

    #[test]
    fn bounded_command_output_handles_zero_capacity_by_draining_all_input() {
        let input = b"discard all";
        let output = read_bounded_command_output(&input[..], 0).unwrap();
        assert!(output.bytes.is_empty());
        assert!(output.truncated);
    }
    #[test]
fn windows_sandbox_image_path_must_be_exact_system_directory_image() {
        assert!(windows_sandbox_image_path_in_system_directory(
            r"C:\Windows\System32\WindowsSandbox.exe", r"C:\Windows\System32"));
        assert!(windows_sandbox_image_path_in_system_directory(
            r"c:/windows/system32/WindowsSandbox.exe", r"C:/Windows/System32/"));
        assert!(!windows_sandbox_image_path_in_system_directory(
            r"C:\Temp\WindowsSandbox.exe", r"C:\Windows\System32"));
        assert!(!windows_sandbox_image_path_in_system_directory(
            r"C:\Windows\System32\Other.exe", r"C:\Windows\System32"));
        assert!(!windows_sandbox_image_path_in_system_directory(
            r"C:\Windows\System32\..\Temp\WindowsSandbox.exe", r"C:\Windows\System32"));
    }

    fn sandbox_stop_reobservation_requires_exact_identity_and_authoritative_ownership() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let identity = SandboxProcessIdentity {
            session_id: run_id.into(),
            process_id: 4321,
            process_creation_time: 987654,
            image_path: r"C:\Windows\System32\WindowsSandbox.exe".into(),
        };
        let mut journal = SandboxSessionJournal {
            schema_version: SANDBOX_SESSION_JOURNAL_SCHEMA,
            run_id: run_id.into(),
            entries: Vec::new(),
        };
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::RequestLaunch, None, false, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ObserveProcess, Some(&identity), false, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ConfirmSession, Some(&identity), true, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ConfirmRunning, Some(&identity), true, false,
        ).unwrap();

        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &identity, true).is_ok());
        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &identity, false).is_err());

        let mut reused_pid = identity.clone();
        reused_pid.process_creation_time += 1;
        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &reused_pid, true).is_err());

        let mut substituted_pid = identity.clone();
        substituted_pid.process_id += 1;
        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &substituted_pid, true).is_err());

        let mut substituted_image = identity.clone();
        substituted_image.image_path = r"C:\Temp\WindowsSandbox.exe".into();
        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &substituted_image, true).is_err());

        let mut substituted_session = identity.clone();
        substituted_session.session_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
        assert!(verify_reobserved_sandbox_process_for_stop(&journal, &substituted_session, true).is_err());

        let mut invalid_state = journal.clone();
        invalid_state.entries.pop();
        assert!(verify_reobserved_sandbox_process_for_stop(&invalid_state, &identity, true).is_err());
    }

fn windows_sandbox_stop_plan_requires_running_owned_journal_and_exclusive_inventory() {
        let run_id = "0123456789abcdef0123456789abcdef";
        let session_id = windows_sandbox_cli_id_from_run_id(run_id).unwrap();
        let identity = SandboxProcessIdentity {
            session_id: run_id.into(),
            process_id: 4321,
            process_creation_time: 987654,
            image_path: r"C:\Windows\System32\WindowsSandbox.exe".into(),
        };
        let mut journal = SandboxSessionJournal {
            schema_version: SANDBOX_SESSION_JOURNAL_SCHEMA,
            run_id: run_id.into(),
            entries: Vec::new(),
        };
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::RequestLaunch, None, false, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ObserveProcess, Some(&identity), false, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ConfirmSession, Some(&identity), true, false,
        ).unwrap();
        journal = append_sandbox_session_journal_entry(
            &journal, SandboxSessionEvent::ConfirmRunning, Some(&identity), true, false,
        ).unwrap();

        let inventory = vec![WindowsSandboxCliSession {
            id: session_id.clone(),
            status: WindowsSandboxCliSessionStatus::Running,
        }];
        let plan = prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &journal, &inventory,
        ).unwrap();
        assert_eq!(plan.arguments, vec![
            "stop", "--raw", "--id", session_id.as_str(),
        ]);

        assert!(prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &journal, &[],
        ).is_err());
        let foreign = vec![
            inventory[0].clone(),
            WindowsSandboxCliSession {
                id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                status: WindowsSandboxCliSessionStatus::Running,
            },
        ];
        assert!(prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &journal, &foreign,
        ).is_err());
        let stopped = vec![WindowsSandboxCliSession {
            id: session_id,
            status: WindowsSandboxCliSessionStatus::Stopped,
        }];
        assert!(prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &journal, &stopped,
        ).is_err());

        let mut not_running = journal.clone();
        not_running.entries.pop();
        assert!(prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &not_running, &inventory,
        ).is_err());
        let mut mismatched = journal.clone();
        mismatched.run_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
        assert!(prepare_authorized_windows_sandbox_stop_plan(
            r"C:\Program Files\WindowsApps\wsb.exe", &mismatched, &inventory,
        ).is_err());
    }

    #[test]
    fn windows_sandbox_cli_post_launch_inventory_requires_one_exact_session() {
        let expected = WindowsSandboxCliSession {
            id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            status: WindowsSandboxCliSessionStatus::Running,
        };
        assert!(require_exclusive_windows_sandbox_cli_session(
            &[expected.clone()], &expected.id, WindowsSandboxCliSessionStatus::Running,
        ).is_ok());

        assert!(require_exclusive_windows_sandbox_cli_session(
            &[], &expected.id, WindowsSandboxCliSessionStatus::Running,
        ).is_err());
        assert!(require_exclusive_windows_sandbox_cli_session(
            &[expected.clone()], &expected.id, WindowsSandboxCliSessionStatus::Stopped,
        ).is_err());

        let foreign = WindowsSandboxCliSession {
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            status: WindowsSandboxCliSessionStatus::Running,
        };
        assert!(require_exclusive_windows_sandbox_cli_session(
            &[foreign.clone()], &expected.id, WindowsSandboxCliSessionStatus::Running,
        ).is_err());
        assert!(require_exclusive_windows_sandbox_cli_session(
            &[expected.clone(), foreign], &expected.id, WindowsSandboxCliSessionStatus::Running,
        ).is_err());
        assert!(require_exclusive_windows_sandbox_cli_session(
            &[expected.clone()], "not-a-session-id", WindowsSandboxCliSessionStatus::Running,
        ).is_err());
    }

    #[test]
    fn windows_sandbox_cli_launch_precondition_requires_an_empty_inventory() {
        assert!(require_empty_windows_sandbox_cli_inventory(&[]).is_ok());

        let running = vec![WindowsSandboxCliSession {
            id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            status: WindowsSandboxCliSessionStatus::Running,
        }];
        assert!(require_empty_windows_sandbox_cli_inventory(&running).is_err());

        let stopped = vec![WindowsSandboxCliSession {
            id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            status: WindowsSandboxCliSessionStatus::Stopped,
        }];
        assert!(require_empty_windows_sandbox_cli_inventory(&stopped).is_err());

        let mixed = vec![running[0].clone(), stopped[0].clone()];
        assert!(require_empty_windows_sandbox_cli_inventory(&mixed).is_err());
    }

    #[test]
    fn windows_sandbox_cli_inventory_enforces_fixed_session_count_boundary() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let accepted = format!(
            r#"{{"WindowsSandboxEnvironments":[{}]}}"#,
            (0..MAX_WINDOWS_SANDBOX_CLI_SESSIONS)
                .map(|index| format!(
                    r#"{{"Id":"{:08x}-89ab-cdef-0123-456789abcdef","Status":"Stopped"}}"#,
                    index + 1
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let parsed = parse_windows_sandbox_cli_list(accepted.as_bytes()).unwrap();
        assert_eq!(parsed.len(), MAX_WINDOWS_SANDBOX_CLI_SESSIONS);

        let rejected = format!(
            r#"{{"WindowsSandboxEnvironments":[{}]}}"#,
            (0..=MAX_WINDOWS_SANDBOX_CLI_SESSIONS)
                .map(|index| format!(
                    r#"{{"Id":"{:08x}-89ab-cdef-0123-456789abcdef","Status":"Stopped"}}"#,
                    index + 1
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse_windows_sandbox_cli_list(rejected.as_bytes()).is_err());
        assert!(valid_windows_sandbox_cli_id(id));
    }

    #[test]
    fn windows_sandbox_cli_list_parser_rejects_ambiguous_or_unsupported_inventory() {
        assert!(parse_windows_sandbox_cli_list(b"").is_err());
        assert!(parse_windows_sandbox_cli_list(b"not-json").is_err());
        assert!(parse_windows_sandbox_cli_list(
            br#"{"OtherSessions":[]}"#
        ).is_err());
        assert!(parse_windows_sandbox_cli_list(
            br#"{"WindowsSandboxEnvironments":[{"Id":"not-a-guid","Status":"Running"}]}"#
        ).is_err());
        assert!(parse_windows_sandbox_cli_list(
            br#"{"WindowsSandboxEnvironments":[{"Id":"01234567-89ab-cdef-0123-456789abcdef","Status":"Starting"}]}"#
        ).is_err());
        assert!(parse_windows_sandbox_cli_list(
            br#"{"WindowsSandboxEnvironments":[{"Id":"01234567-89ab-cdef-0123-456789abcdef","Status":"Running","Command":"evil"}]}"#
        ).is_err());
        assert!(parse_windows_sandbox_cli_list(
            br#"{"WindowsSandboxEnvironments":[{"Id":"01234567-89ab-cdef-0123-456789abcdef","Status":"Running"},{"Id":"01234567-89ab-cdef-0123-456789abcdef","Status":"Running"}]}"#
        ).is_err());
    }

    #[test]
    fn sandbox_launch_plan_uses_only_fixed_executable_and_staged_config() {
        let plan = build_windows_sandbox_launch_plan(
            r"C:\Windows",
            r"C:\Users\Friday\AppData\Local\FRIDAY\run-123",
        ).unwrap();
        assert_eq!(plan.executable_path, r"C:\Windows\System32\WindowsSandbox.exe");
        assert_eq!(plan.config_path, r"C:\Users\Friday\AppData\Local\FRIDAY\run-123\sandbox.wsb");
        assert_eq!(plan.arguments, vec![plan.config_path.clone()]);
        validate_windows_sandbox_launch_plan(
            &plan, r"C:\Windows", r"C:\Users\Friday\AppData\Local\FRIDAY\run-123",
        ).unwrap();
    }

    #[test]
    fn sandbox_launch_plan_rejects_argument_and_path_substitution() {
        let system_root = r"C:\Windows";
        let run_directory = r"C:\FRIDAY\run";
        let plan = build_windows_sandbox_launch_plan(system_root, run_directory).unwrap();
        let mut changed = plan.clone();
        changed.arguments.push("--network".into());
        assert!(validate_windows_sandbox_launch_plan(&changed, system_root, run_directory).is_err());
        changed = plan.clone();
        changed.executable_path = r"C:\Temp\WindowsSandbox.exe".into();
        assert!(validate_windows_sandbox_launch_plan(&changed, system_root, run_directory).is_err());
        changed = plan.clone();
        changed.config_path = r"C:\FRIDAY\other.wsb".into();
        assert!(validate_windows_sandbox_launch_plan(&changed, system_root, run_directory).is_err());
    }

    #[test]
    fn sandbox_launch_plan_rejects_relative_parent_and_injected_paths() {
        for invalid in ["relative", r"C:\FRIDAY\..\outside", "C:\\FRIDAY\\run\" --network"] {
            assert!(build_windows_sandbox_launch_plan(invalid, r"C:\FRIDAY\run").is_err());
            assert!(build_windows_sandbox_launch_plan(r"C:\Windows", invalid).is_err());
        }
    }


    fn unique_guest_run_request_json() -> (String, String) {
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        let run_id = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let mut value: serde_json::Value =
            serde_json::from_slice(&valid_guest_request_json()).unwrap();
        value["runId"] = serde_json::json!(run_id);
        (run_id, serde_json::to_string(&value).unwrap())
    }

    #[test]
    fn guest_approval_commands_issue_serializable_receipt_and_consume_once() {
        let (_run_id, request_json) = unique_guest_run_request_json();
        let receipt = issue_guest_run_execution_approval(request_json.clone()).unwrap();
        assert_eq!(receipt.token.len(), 48);
        assert!(receipt.token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(receipt.expires_at_unix_seconds > guest_run_now_unix_seconds().unwrap());

        let serialized = serde_json::to_value(&receipt).unwrap();
        assert_eq!(serialized["token"], receipt.token);
        assert_eq!(
            serialized["expiresAtUnixSeconds"],
            serde_json::json!(receipt.expires_at_unix_seconds),
        );
        assert!(serialized.get("expires_at_unix_seconds").is_none());

        assert!(consume_guest_run_execution_approval(
            request_json.clone(), receipt.token.clone(),
        ).is_ok());
        assert!(consume_guest_run_execution_approval(
            request_json, receipt.token,
        ).is_err());
    }

    #[test]
    fn guest_approval_command_bad_tokens_consume_the_pending_grant() {
        let (_run_id, request_json) = unique_guest_run_request_json();
        let receipt = issue_guest_run_execution_approval(request_json.clone()).unwrap();

        // A malformed token is still a terminal consume attempt. It must not
        // leave the real approval available for a later guess or retry.
        assert!(consume_guest_run_execution_approval(
            request_json.clone(), "not-a-hex-token".to_string(),
        ).is_err());
        assert!(consume_guest_run_execution_approval(
            request_json.clone(), receipt.token.clone(),
        ).is_err());

        let (_second_run_id, second_request) = unique_guest_run_request_json();
        let second_receipt = issue_guest_run_execution_approval(second_request.clone()).unwrap();
        let oversized_token = "a".repeat(4096);
        assert!(consume_guest_run_execution_approval(
            second_request.clone(), oversized_token,
        ).is_err());
        assert!(consume_guest_run_execution_approval(
            second_request, second_receipt.token,
        ).is_err());
    }

    #[test]
    fn guest_approval_commands_reject_mismatch_revoke_and_malformed_requests() {
        let (run_id, request_json) = unique_guest_run_request_json();
        let receipt = issue_guest_run_execution_approval(request_json.clone()).unwrap();

        let mut changed: serde_json::Value =
            serde_json::from_str(&request_json).unwrap();
        changed["snapshotId"] = serde_json::json!("11111111111111111111111111111111");
        let changed_request = serde_json::to_string(&changed).unwrap();
        assert!(consume_guest_run_execution_approval(
            changed_request, receipt.token.clone(),
        ).is_err());
        assert!(consume_guest_run_execution_approval(
            request_json, receipt.token,
        ).is_err());

        let (revoked_run_id, revoked_request) = unique_guest_run_request_json();
        let revoked = issue_guest_run_execution_approval(revoked_request.clone()).unwrap();
        assert!(revoke_guest_run_execution_approval(revoked_run_id.clone()).unwrap());
        assert!(!revoke_guest_run_execution_approval(revoked_run_id).unwrap());
        assert!(consume_guest_run_execution_approval(
            revoked_request, revoked.token,
        ).is_err());

        assert!(issue_guest_run_execution_approval(
            r#"{"schemaVersion":1,"unexpected":true}"#.to_string(),
        ).is_err());
        assert!(revoke_guest_run_execution_approval("not-a-run-id".to_string()).is_err());
        assert!(revoke_guest_run_execution_approval(run_id).is_ok());
    }


    #[test]
    fn approval_store_allows_exactly_one_concurrent_consumer() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut initial_store = GuestRunApprovalStore::default();
        let token = initial_store.issue(&plan, 1_000).unwrap();
        let store = std::sync::Arc::new(Mutex::new(initial_store));
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));

        let workers = (0..8).map(|_| {
            let store = std::sync::Arc::clone(&store);
            let start = std::sync::Arc::clone(&start);
            let plan = plan.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                start.wait();
                store.lock().unwrap().consume(&plan, &token, 1_001).is_ok()
            })
        }).collect::<Vec<_>>();

        let successful_consumers = workers.into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|succeeded| *succeeded)
            .count();
        assert_eq!(successful_consumers, 1);
        assert_eq!(store.lock().unwrap().pending_count(), 0);
    }


    #[test]
    fn approval_store_rejects_every_mutated_execution_plan_field() {
        let original = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();

        let mut mutations = Vec::new();

        let mut changed_duration = original.clone();
        changed_duration.max_duration += Duration::from_millis(1);
        mutations.push(changed_duration);

        let mut changed_output_limit = original.clone();
        changed_output_limit.max_output_bytes += 1;
        mutations.push(changed_output_limit);

        let mut changed_operation = original.clone();
        changed_operation.operation = GuestRunOperation::PythonUnittest;
        changed_operation.command = guest_operation_command(changed_operation.operation);
        mutations.push(changed_operation);

        let mut changed_command = original.clone();
        changed_command.command = GuestOperationCommand {
            program: "cmd.exe",
            arguments: &["/c", "whoami"],
        };
        mutations.push(changed_command);

        for mutated_plan in mutations {
            let mut store = GuestRunApprovalStore::default();
            let token = store.issue(&original, 1_000).unwrap();

            assert!(store.consume(&mutated_plan, &token, 1_001).is_err());
            assert_eq!(store.pending_count(), 0);
            assert!(store.consume(&original, &token, 1_002).is_err());
        }
    }



    #[test]
    fn approval_store_enforces_expiry_at_the_exact_ttl_boundary() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();

        let mut valid_store = GuestRunApprovalStore::default();
        let valid_token = valid_store.issue(&plan, 1_000).unwrap();
        assert!(valid_store.consume(&plan, &valid_token, 1_119).is_ok());
        assert_eq!(valid_store.pending_count(), 0);

        let mut expired_store = GuestRunApprovalStore::default();
        let expired_token = expired_store.issue(&plan, 1_000).unwrap();
        assert!(expired_store.consume(&plan, &expired_token, 1_120).is_err());
        assert_eq!(expired_store.pending_count(), 0);
        assert!(expired_store.consume(&plan, &expired_token, 1_121).is_err());
    }

    #[test]
    fn approval_store_replacement_invalidates_the_previous_token() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();

        let old_token = store.issue(&plan, 1_000).unwrap();
        let replacement_token = store.issue(&plan, 1_001).unwrap();
        assert_ne!(old_token, replacement_token);
        assert_eq!(store.pending_count(), 1);

        assert!(store.consume(&plan, &replacement_token, 1_002).is_ok());
        assert!(store.consume(&plan, &old_token, 1_003).is_err());
        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn approval_store_bounds_pending_grants_and_prunes_expired_entries() {
        let base_plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();

        for index in 0..MAX_PENDING_GUEST_RUN_APPROVALS {
            let mut plan = base_plan.clone();
            plan.run_id = format!("{:032x}", index + 1);
            assert!(store.issue(&plan, 1_000).is_ok());
        }
        assert_eq!(store.pending_count(), MAX_PENDING_GUEST_RUN_APPROVALS);

        let mut overflow_plan = base_plan.clone();
        overflow_plan.run_id = format!("{:032x}", MAX_PENDING_GUEST_RUN_APPROVALS + 1);
        assert!(store.issue(&overflow_plan, 1_001).is_err());
        assert_eq!(store.pending_count(), MAX_PENDING_GUEST_RUN_APPROVALS);

        // Every grant expires at 1,120. Issuing at that exact boundary must
        // prune the stale entries before applying the pending-grant limit.
        assert!(store.issue(&overflow_plan, 1_120).is_ok());
        assert_eq!(store.pending_count(), 1);
    }



    #[test]
    fn approval_store_rejects_clock_rollback_without_leaving_a_grant() {
        let plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();
        let token = store.issue(&plan, 2_000).unwrap();

        // A wall-clock rollback must not make a newly issued grant appear to
        // have more than its fixed TTL remaining.
        assert!(store.consume(&plan, &token, 1_999).is_err());
        assert_eq!(store.pending_count(), 0);
        assert!(store.consume(&plan, &token, 2_001).is_err());
    }

    #[test]
    fn failed_replacement_issue_does_not_replace_an_existing_valid_grant() {
        let original = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut store = GuestRunApprovalStore::default();
        let original_token = store.issue(&original, 3_000).unwrap();

        let mut invalid_replacement = original.clone();
        invalid_replacement.command = GuestOperationCommand {
            program: "cmd.exe",
            arguments: &["/c", "whoami"],
        };
        assert!(store.issue(&invalid_replacement, 3_001).is_err());
        assert_eq!(store.pending_count(), 1);

        // Only a successfully issued replacement supersedes the old grant.
        assert!(store.consume(&original, &original_token, 3_002).is_ok());
        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn approval_store_revocation_is_idempotent_and_run_scoped() {
        let base_plan = build_guest_run_execution_plan(
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
        ).unwrap();
        let mut first_plan = base_plan.clone();
        first_plan.run_id = "11111111111111111111111111111111".into();
        let mut second_plan = base_plan;
        second_plan.run_id = "22222222222222222222222222222222".into();

        let mut store = GuestRunApprovalStore::default();
        let first_token = store.issue(&first_plan, 4_000).unwrap();
        let second_token = store.issue(&second_plan, 4_000).unwrap();
        assert_eq!(store.pending_count(), 2);

        assert!(store.revoke(&first_plan.run_id));
        assert!(!store.revoke(&first_plan.run_id));
        assert_eq!(store.pending_count(), 1);

        assert!(store.consume(&second_plan, &second_token, 4_001).is_ok());
        assert!(store.consume(&first_plan, &first_token, 4_001).is_err());
        assert_eq!(store.pending_count(), 0);
    }


    #[test]
    fn authorized_guest_run_payload_binds_exact_request_proof_and_runtime_plan() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let run = build_guest_run_execution_plan(request).unwrap();
        let proof = runtime_proof_fixture(run.operation);
        let runtime = build_verified_guest_runtime_execution_plan(&proof).unwrap();
        let request_bytes = serialize_guest_run_execution_request(&run).unwrap();
        let authorized = AuthorizedVerifiedGuestRun {
            run: run.clone(),
            proof: proof.clone(),
            runtime: runtime.clone(),
            request_bytes: request_bytes.clone(),
        };
        assert!(validate_authorized_guest_run_payload(&authorized).is_ok());

        let mut changed = AuthorizedVerifiedGuestRun {
            run: run.clone(), proof: proof.clone(), runtime: runtime.clone(),
            request_bytes: request_bytes.clone(),
        };
        changed.request_bytes.push(b' ');
        assert!(validate_authorized_guest_run_payload(&changed).is_err());

        changed = AuthorizedVerifiedGuestRun {
            run: run.clone(), proof: proof.clone(), runtime: runtime.clone(),
            request_bytes: request_bytes.clone(),
        };
        changed.run.max_output_bytes -= 1;
        assert!(validate_authorized_guest_run_payload(&changed).is_err());

        changed = AuthorizedVerifiedGuestRun {
            run: run.clone(), proof: proof.clone(), runtime: runtime.clone(),
            request_bytes: request_bytes.clone(),
        };
        changed.proof.root_identity = (9, 9);
        assert!(validate_authorized_guest_run_payload(&changed).is_err());

        changed = AuthorizedVerifiedGuestRun {
            run: run.clone(), proof: proof.clone(), runtime: runtime.clone(),
            request_bytes: request_bytes.clone(),
        };
        changed.runtime.launch.arguments.push("extra".into());
        assert!(validate_authorized_guest_run_payload(&changed).is_err());

        changed = AuthorizedVerifiedGuestRun {
            run: run.clone(), proof: proof.clone(), runtime: runtime.clone(),
            request_bytes,
        };
        changed.runtime.operation = GuestRunOperation::PythonUnittest;
        assert!(validate_authorized_guest_run_payload(&changed).is_err());
    }

    #[test]
    fn final_runtime_authorization_rejects_operation_mismatch_without_consuming_approval() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let run = build_guest_run_execution_plan(request).unwrap();
        let mut approvals = GuestRunApprovalStore::default();
        let token = approvals.issue(&run, 5_000).unwrap();
        let proof = runtime_proof_fixture(GuestRunOperation::PythonUnittest);
        let runtime_plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();
        let unrelated_path = make_test_directory("runtime-authorization-operation");
        let file_path = unrelated_path.join("not-a-directory-handle");
        fs::write(&file_path, b"not a directory").unwrap();
        let handle = File::open(&file_path).unwrap();

        let result = authorize_verified_guest_runtime_execution(
            &mut approvals,
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
            &token,
            &proof,
            &runtime_plan,
            &proof.root_path,
            &handle,
            5_001,
        );
        assert!(result.is_err());
        assert_eq!(approvals.pending_count(), 1);
        assert!(approvals.consume(&run, &token, 5_002).is_ok());

        drop(handle);
        fs::remove_dir_all(unrelated_path).unwrap();
    }

    #[test]
    fn final_runtime_authorization_rejects_detached_root_without_consuming_approval() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let run = build_guest_run_execution_plan(request).unwrap();
        let mut approvals = GuestRunApprovalStore::default();
        let token = approvals.issue(&run, 6_000).unwrap();
        let proof = runtime_proof_fixture(run.operation);
        let runtime_plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();
        let unrelated_path = make_test_directory("runtime-authorization-root");
        let file_path = unrelated_path.join("not-a-directory-handle");
        fs::write(&file_path, b"not a directory").unwrap();
        let handle = File::open(&file_path).unwrap();
        let wrong_root = PathBuf::from(r"C:\\FRIDAY\\Toolchains\\Other");

        let result = authorize_verified_guest_runtime_execution(
            &mut approvals,
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
            &token,
            &proof,
            &runtime_plan,
            &wrong_root,
            &handle,
            6_001,
        );
        assert!(result.is_err());
        assert_eq!(approvals.pending_count(), 1);
        assert!(approvals.consume(&run, &token, 6_002).is_ok());

        drop(handle);
        fs::remove_dir_all(unrelated_path).unwrap();
    }

    #[test]
    fn final_runtime_authorization_rejects_mutated_plan_without_consuming_approval() {
        let request = parse_guest_run_request(&valid_guest_request_json()).unwrap();
        let run = build_guest_run_execution_plan(request).unwrap();
        let mut approvals = GuestRunApprovalStore::default();
        let token = approvals.issue(&run, 7_000).unwrap();
        let proof = runtime_proof_fixture(run.operation);
        let mut runtime_plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();
        runtime_plan.launch.arguments.push("unapproved".into());
        let unrelated_path = make_test_directory("runtime-authorization-plan");
        let file_path = unrelated_path.join("not-a-directory-handle");
        fs::write(&file_path, b"not a directory").unwrap();
        let handle = File::open(&file_path).unwrap();

        let result = authorize_verified_guest_runtime_execution(
            &mut approvals,
            parse_guest_run_request(&valid_guest_request_json()).unwrap(),
            &token,
            &proof,
            &runtime_plan,
            &proof.root_path,
            &handle,
            7_001,
        );
        assert!(result.is_err());
        assert_eq!(approvals.pending_count(), 1);
        assert!(approvals.consume(&run, &token, 7_002).is_ok());

        drop(handle);
        fs::remove_dir_all(unrelated_path).unwrap();
    }

    #[test]
    fn runtime_provisioning_plan_binds_authenticated_bundle_to_fixed_roots() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
            let plan = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();
            validate_guest_runtime_provisioning_plan(&manifest, &anchor, &plan).unwrap();
            assert_eq!(plan.operation, operation);
            assert_eq!(plan.final_root, PathBuf::from(fixed_guest_runtime_root(operation)));
            assert_eq!(plan.staging_root.parent(), plan.final_root.parent());
            assert_ne!(plan.staging_root, plan.final_root);
            assert_eq!(plan.manifest_sha256, anchor.manifest_sha256.to_ascii_lowercase());
            assert_eq!(plan.artifact_count, manifest.artifacts.len());
            assert_eq!(plan.declared_max_bytes,
                manifest.artifacts.iter().map(|artifact| artifact.max_bytes).sum::<u64>());
        }
    }

    #[test]
    fn runtime_provisioning_plan_rejects_redirected_paths_and_changed_limits() {
        let operation = GuestRunOperation::NodeNpmTest;
        let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let plan = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();

        let mut changed = plan.clone();
        changed.staging_root = PathBuf::from(r"C:\\FRIDAY\\Untrusted\\stage");
        assert!(validate_guest_runtime_provisioning_plan(&manifest, &anchor, &changed).is_err());

        let mut changed = plan.clone();
        changed.final_root = PathBuf::from(r"C:\\FRIDAY\\Toolchains\\Other");
        assert!(validate_guest_runtime_provisioning_plan(&manifest, &anchor, &changed).is_err());

        let mut changed = plan.clone();
        changed.declared_max_bytes -= 1;
        assert!(validate_guest_runtime_provisioning_plan(&manifest, &anchor, &changed).is_err());
    }

    #[test]
    fn runtime_provisioning_plan_rejects_manifest_or_anchor_tampering() {
        let operation = GuestRunOperation::PythonUnittest;
        let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let plan = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();

        let mut changed_manifest = manifest.clone();
        changed_manifest.artifacts[0].sha256 = "b".repeat(64);
        assert!(validate_guest_runtime_provisioning_plan(&changed_manifest, &anchor, &plan).is_err());

        let mut changed_anchor = anchor.clone();
        changed_anchor.toolchain_version.push_str("-other");
        assert!(validate_guest_runtime_provisioning_plan(&manifest, &changed_anchor, &plan).is_err());
    }

    fn runtime_proof_fixture(operation: GuestRunOperation) -> VerifiedGuestRuntimeBundle {
        VerifiedGuestRuntimeBundle {
            operation,
            root_path: PathBuf::from(fixed_guest_runtime_root(operation)),
            // Synthetic identity for pure launch-plan tests; these fixtures do
            // not open a filesystem root or authorize real process execution.
            root_identity: (1, 1),
            bundle_id: "friday-runtime-test".into(),
            toolchain_version: "1.2.3".into(),
            platform: "windows".into(),
            architecture: "x86_64".into(),
            manifest_sha256: "a".repeat(64),
            artifact_count: match operation {
                GuestRunOperation::NodeNpmTest => 2,
                _ => 1,
            },
            artifact_paths: required_guest_runtime_artifact_paths(operation),
        }
    }

    #[test]
    fn verified_runtime_execution_plan_binds_each_operation_to_its_fixed_profile() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let proof = runtime_proof_fixture(operation);
            let plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();
            validate_verified_guest_runtime_execution_plan(&proof, &plan).unwrap();
            assert_eq!(plan.operation, operation);
            assert_eq!(plan.root_path, proof.root_path);
            assert_eq!(plan.root_identity, proof.root_identity);
            assert_eq!(plan.launch, build_guest_runtime_launch_plan(operation));
            assert_eq!(plan.environment, build_guest_runtime_environment_plan(operation));
            assert_eq!(
                plan.environment_block,
                serialize_guest_runtime_environment_block(
                    operation,
                    &build_guest_runtime_environment_plan(operation),
                ).unwrap(),
            );
            assert_eq!(plan.bundle_id, proof.bundle_id);
            assert_eq!(plan.toolchain_version, proof.toolchain_version);
            assert_eq!(plan.manifest_sha256, proof.manifest_sha256);
        }
    }

    #[test]
    fn verified_runtime_execution_plan_rejects_substituted_root_identity_or_path() {
        let proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        let plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();

        let mut substituted = plan.clone();
        substituted.root_identity = (9, 9);
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &substituted).is_err());

        let mut substituted = plan.clone();
        substituted.root_path = PathBuf::from(r"C:\FRIDAY\Toolchains\Other");
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &substituted).is_err());

        let mut changed_proof = proof.clone();
        changed_proof.root_identity = (9, 9);
        assert!(validate_verified_guest_runtime_execution_plan(&changed_proof, &plan).is_err());
    }

    #[test]
    fn verified_runtime_execution_context_rejects_detached_plan_and_root() {
        let proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        let plan = build_verified_guest_runtime_execution_plan(&proof).unwrap();
        let unrelated_path = make_test_directory("runtime-context-unrelated");
        let unrelated_file_path = unrelated_path.join("not-a-directory-handle");
        fs::write(&unrelated_file_path, b"not a directory").unwrap();
        let unrelated_handle = File::open(&unrelated_file_path).unwrap();

        let mut substituted_plan = plan.clone();
        substituted_plan.root_identity = (9, 9);
        assert!(validate_verified_guest_runtime_execution_context(
            &proof, &substituted_plan, &proof.root_path, &unrelated_handle,
        ).is_err());

        let wrong_path = PathBuf::from(r"C:\FRIDAY\Toolchains\Other");
        assert!(validate_verified_guest_runtime_execution_context(
            &proof, &plan, &wrong_path, &unrelated_handle,
        ).is_err());

        drop(unrelated_handle);
        fs::remove_dir_all(unrelated_path).unwrap();
    }

    #[test]
    fn verified_runtime_execution_plan_rejects_wrong_root_operation_and_platform() {
        let mut proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.root_path = PathBuf::from(r"C:\FRIDAY\Toolchains\Node");
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.root_path = PathBuf::from(r"C:\FRIDAY\Toolchains\Rust\..\Node");
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.platform = "linux".into();
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.architecture = "arm64".into();
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.operation = GuestRunOperation::PythonUnittest;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());
    }

    #[test]
    fn verified_runtime_execution_plan_rejects_invalid_attestation_metadata() {
        let mut proof = runtime_proof_fixture(GuestRunOperation::PythonUnittest);
        proof.manifest_sha256 = "not-a-digest".into();
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::PythonUnittest);
        proof.bundle_id = "../untrusted".into();
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::PythonUnittest);
        proof.toolchain_version.clear();
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::PythonUnittest);
        proof.artifact_count = MAX_GUEST_RUNTIME_MANIFEST_ENTRIES + 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());
    }

    #[test]
    fn verified_runtime_execution_plan_rejects_inconsistent_or_unsafe_artifact_inventory() {
        let mut proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);

        proof.artifact_count += 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.artifact_paths.insert("../outside.exe".into());
        proof.artifact_count += 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.artifact_paths.insert("bin/other.exe".into());
        proof.artifact_count += 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.artifact_paths.insert("c:\\\\outside.exe".into());
        proof.artifact_count += 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());

        proof = runtime_proof_fixture(GuestRunOperation::RustCargoTest);
        proof.artifact_paths.insert("BIN\\\\CARGO.EXE".into());
        proof.artifact_count += 1;
        assert!(build_verified_guest_runtime_execution_plan(&proof).is_err());
    }

    #[test]
    fn verified_runtime_execution_plan_requires_exact_manifest_entrypoints() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let mut proof = runtime_proof_fixture(operation);
            assert!(build_verified_guest_runtime_execution_plan(&proof).is_ok());

            for required in required_guest_runtime_artifact_paths(operation) {
                let mut missing = proof.clone();
                missing.artifact_paths.remove(&required);
                assert!(build_verified_guest_runtime_execution_plan(&missing).is_err());
            }

            proof.artifact_paths.insert("untrusted\\replacement.exe".into());
            proof.artifact_count += 1;
            assert!(build_verified_guest_runtime_execution_plan(&proof).is_ok());
            assert!(build_verified_guest_runtime_execution_plan(&proof).is_ok());
        }
    }

    #[test]
    fn runtime_launch_artifact_mapping_rejects_path_and_script_substitution() {
        let operation = GuestRunOperation::NodeNpmTest;
        let root = PathBuf::from(fixed_guest_runtime_root(operation));
        let launch = build_guest_runtime_launch_plan(operation);
        let paths = required_guest_runtime_artifact_paths(operation);
        assert!(guest_runtime_launch_paths_are_manifested(operation, &root, &launch, &paths));

        let mut wrong_paths = paths.clone();
        wrong_paths.remove("node.exe");
        assert!(!guest_runtime_launch_paths_are_manifested(operation, &root, &launch, &wrong_paths));

        let mut wrong_launch = launch.clone();
        wrong_launch.program = PathBuf::from(r"C:\FRIDAY\Toolchains\Node\other.exe");
        assert!(!guest_runtime_launch_paths_are_manifested(operation, &root, &wrong_launch, &paths));

        wrong_launch = launch.clone();
        wrong_launch.arguments[0] = r"C:\FRIDAY\Toolchains\Node\node_modules\npm\bin\other.js".into();
        assert!(!guest_runtime_launch_paths_are_manifested(operation, &root, &wrong_launch, &paths));

        assert!(!guest_runtime_launch_paths_are_manifested(
            operation,
            &PathBuf::from(r"C:\FRIDAY\Toolchains\Rust"),
            &launch,
            &paths,
        ));
    }

    #[test]
    fn verified_runtime_execution_plan_rejects_every_derived_plan_substitution() {
        let proof = runtime_proof_fixture(GuestRunOperation::NodeNpmTest);
        let original = build_verified_guest_runtime_execution_plan(&proof).unwrap();

        let mut changed = original.clone();
        changed.launch.program = PathBuf::from(r"C:\Windows\System32\cmd.exe");
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());

        changed = original.clone();
        changed.launch.arguments.push("/c".into());
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());

        changed = original.clone();
        changed.launch.working_directory = PathBuf::from(r"C:\");
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());

        changed = original.clone();
        changed.environment.variables.insert("COMSPEC".into(), r"C:\Windows\System32\cmd.exe".into());
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());

        changed = original.clone();
        changed.environment_block.push(0x0041);
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());

        changed = original.clone();
        changed.bundle_id = "substituted".into();
        assert!(validate_verified_guest_runtime_execution_plan(&proof, &changed).is_err());
    }


    #[test]
    fn published_runtime_requires_exact_owner_marker_and_payload_closure() {
        for (operation, label) in [
            (GuestRunOperation::RustCargoTest, "rust"),
            (GuestRunOperation::NodeNpmTest, "node"),
            (GuestRunOperation::PythonUnittest, "python"),
        ] {
            let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
            let root_path = make_test_directory(&format!(
                "published-runtime-owner-marker-integrity-{label}"
            ));

            for artifact in &manifest.artifacts {
                let components = artifact.relative_path.split('\\').collect::<Vec<_>>();
                let file_name = components.last().unwrap();
                let mut file_path = root_path.clone();
                for component in &components[..components.len() - 1] {
                    file_path.push(component);
                }
                fs::create_dir_all(&file_path).unwrap();
                file_path.push(file_name);
                let bytes: &[u8] = match artifact.relative_path.as_str() {
                    "bin\\cargo.exe" => b"cargo-fixture",
                    "node.exe" => b"node-fixture",
                    "node_modules\\npm\\bin\\npm-cli.js" => b"npm-fixture",
                    "python.exe" => b"python-fixture",
                    _ => panic!("unexpected runtime fixture: {}", artifact.relative_path),
                };
                fs::write(file_path, bytes).unwrap();
            }

            let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
            let marker_path = root_path.join(".friday-runtime-owner.json");
            fs::write(&marker_path, &marker).unwrap();
            let root = crate::coding_sandbox_fs::open_verified_root_directory(&root_path).unwrap();

            // Only the authenticated published-runtime path may accept the
            // exact ownership marker paired with this trusted manifest.
            assert!(verify_authenticated_guest_runtime_bundle(
                &root_path, &root, operation, &manifest, &anchor,
            ).is_ok());

            // The standalone payload verifier must not silently ignore metadata.
            assert!(verify_guest_runtime_artifact_tree(&root, operation, &manifest).is_err());

            // Any content substitution, even valid-looking JSON, is rejected.
            fs::write(&marker_path, br#"{"tampered":true}"#).unwrap();
            assert!(verify_authenticated_guest_runtime_bundle(
                &root_path, &root, operation, &manifest, &anchor,
            ).is_err());

            // A missing marker is never treated as an ordinary payload bundle.
            fs::remove_file(&marker_path).unwrap();
            assert!(verify_authenticated_guest_runtime_bundle(
                &root_path, &root, operation, &manifest, &anchor,
            ).is_err());

            // The marker is bounded independently of the overall tree limit.
            fs::write(&marker_path, vec![b'x'; 16 * 1024 + 1]).unwrap();
            assert!(verify_authenticated_guest_runtime_bundle(
                &root_path, &root, operation, &manifest, &anchor,
            ).is_err());

            // Case-variant duplicate ownership metadata must not be ambiguous.
            #[cfg(unix)]
            {
                fs::write(&marker_path, &marker).unwrap();
                let duplicate_path = root_path.join(".FRIDAY-RUNTIME-OWNER.JSON");
                fs::write(&duplicate_path, &marker).unwrap();
                assert!(verify_authenticated_guest_runtime_bundle(
                    &root_path, &root, operation, &manifest, &anchor,
                ).is_err());
                fs::remove_file(&duplicate_path).unwrap();
            }

            // A link in place of the marker is not ownership evidence.
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let external_marker_path = root_path.with_extension("external-marker");
                fs::write(&external_marker_path, &marker).unwrap();
                fs::remove_file(&marker_path).unwrap();
                symlink(&external_marker_path, &marker_path).unwrap();
                assert!(verify_authenticated_guest_runtime_bundle(
                    &root_path, &root, operation, &manifest, &anchor,
                ).is_err());
                fs::remove_file(&marker_path).unwrap();
                fs::remove_file(&external_marker_path).unwrap();
            }

            // Exact marker content still cannot excuse an unlisted payload.
            fs::write(&marker_path, &marker).unwrap();
            fs::write(root_path.join("unexpected.dll"), b"unlisted").unwrap();
            assert!(verify_authenticated_guest_runtime_bundle(
                &root_path, &root, operation, &manifest, &anchor,
            ).is_err());

            drop(root);
            fs::remove_dir_all(root_path).unwrap();
        }
    }

    #[test]
    fn authenticated_runtime_publication_rehashes_the_final_published_tree() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let base = make_test_directory("authenticated-runtime-publication");
        let final_root = base.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&base).unwrap();
        let files = manifest.artifacts.iter().map(|artifact| {
            let normalized_path = artifact.relative_path.replace('\\', "/");
            let bytes = match normalized_path.as_str() {
                "bin/cargo.exe" => b"cargo-fixture".to_vec(),
                _ => panic!("unexpected runtime fixture: {normalized_path}"),
            };
            (PathBuf::from(normalized_path), bytes)
        }).collect::<Vec<_>>();
        crate::coding_sandbox_fs::create_bounded_runtime_destination(
            &parent, provisioning.staging_root.file_name().unwrap(),
            &files, &marker, 8, 1024, 4096, 8,
        ).unwrap();
        let staging_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let staging = verify_staged_guest_runtime_bundle_with_final_root(
            &provisioning.staging_root, &staging_handle, &manifest, &anchor,
            &provisioning, final_root.clone(),
        ).unwrap();

        let verified = publish_authenticated_guest_runtime_bundle_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &base, &parent,
            staging_handle, final_root.clone(),
        ).unwrap();
        assert_eq!(verified.root_path, final_root);
        assert_eq!(verified.bundle_id, anchor.bundle_id);
        assert_eq!(verified.manifest_sha256, anchor.manifest_sha256);
        assert_eq!(verified.artifact_count, manifest.artifacts.len());
        assert_eq!(
            fs::read(final_root.join("bin").join("cargo.exe")).unwrap(),
            b"cargo-fixture",
        );
        drop(parent);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn authenticated_runtime_publication_never_replaces_existing_destination() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let base = make_test_directory("authenticated-runtime-no-replace");
        let final_root = base.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        let marker = build_guest_runtime_staging_owner_marker(&manifest, &anchor).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&base).unwrap();
        let files = vec![(PathBuf::from("bin/cargo.exe"), b"cargo-fixture".to_vec())];
        crate::coding_sandbox_fs::create_bounded_runtime_destination(
            &parent, provisioning.staging_root.file_name().unwrap(),
            &files, &marker, 8, 1024, 4096, 8,
        ).unwrap();
        let staging_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let staging = verify_staged_guest_runtime_bundle_with_final_root(
            &provisioning.staging_root, &staging_handle, &manifest, &anchor,
            &provisioning, final_root.clone(),
        ).unwrap();
        fs::create_dir(&final_root).unwrap();
        fs::write(final_root.join("keep.txt"), b"preserve").unwrap();

        assert!(publish_authenticated_guest_runtime_bundle_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &base, &parent,
            staging_handle, final_root.clone(),
        ).is_err());
        assert_eq!(fs::read(final_root.join("keep.txt")).unwrap(), b"preserve");
        assert!(provisioning.staging_root.exists());
        drop(parent);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn runtime_publication_plan_binds_verified_staging_to_authenticated_provisioning() {
        for operation in [
            GuestRunOperation::RustCargoTest,
            GuestRunOperation::NodeNpmTest,
            GuestRunOperation::PythonUnittest,
        ] {
            let manifest = test_runtime_manifest(operation);
            let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
            let provisioning = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();
            let staging = VerifiedGuestRuntimeStaging {
                operation,
                staging_root: provisioning.staging_root.clone(),
                root_identity: (17, 29),
                bundle_id: anchor.bundle_id.clone(),
                manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
                artifact_count: manifest.artifacts.len(),
            };

            let publication = build_guest_runtime_publication_plan(
                &staging, &manifest, &anchor, &provisioning,
            ).unwrap();
            validate_guest_runtime_publication_plan(
                &staging, &manifest, &anchor, &provisioning, &publication,
            ).unwrap();

            assert_eq!(publication.operation, operation);
            assert_eq!(publication.bundle_id, anchor.bundle_id);
            assert_eq!(publication.manifest_sha256, anchor.manifest_sha256.to_ascii_lowercase());
            assert_eq!(publication.artifact_count, manifest.artifacts.len());
            assert_eq!(publication.staging_root, provisioning.staging_root);
            assert_eq!(publication.staging_root_identity, (17, 29));
            assert_eq!(publication.final_root, provisioning.final_root);
            assert_ne!(publication.staging_root, publication.final_root);
            assert_eq!(publication.staging_root.parent(), publication.final_root.parent());
        }
    }

    #[test]
    fn runtime_publication_plan_rejects_every_mutable_publication_field() {
        let operation = GuestRunOperation::NodeNpmTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let provisioning = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: (31, 47),
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };
        let publication = build_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning,
        ).unwrap();

        let mut changed = publication.clone();
        changed.operation = GuestRunOperation::PythonUnittest;
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.bundle_id.push_str("-substituted");
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.manifest_sha256 = "b".repeat(64);
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.artifact_count += 1;
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.staging_root.push("redirected");
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.staging_root_identity = (99, 101);
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());

        changed = publication.clone();
        changed.final_root.push("redirected");
        assert!(validate_guest_runtime_publication_plan(
            &staging, &manifest, &anchor, &provisioning, &changed,
        ).is_err());
    }

    #[test]
    fn runtime_publication_plan_rejects_staging_proof_substitution_and_invalid_identity() {
        let operation = GuestRunOperation::PythonUnittest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let provisioning = build_guest_runtime_provisioning_plan(&manifest, &anchor).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: (53, 59),
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };

        let mut changed = staging.clone();
        changed.operation = GuestRunOperation::RustCargoTest;
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());

        changed = staging.clone();
        changed.staging_root.push("other");
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());

        changed = staging.clone();
        changed.bundle_id.push_str("-other");
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());

        changed = staging.clone();
        changed.manifest_sha256 = "c".repeat(64);
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());

        changed = staging.clone();
        changed.artifact_count += 1;
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());

        changed = staging.clone();
        changed.root_identity = (0, 0);
        assert!(build_guest_runtime_publication_plan(
            &changed, &manifest, &anchor, &provisioning,
        ).is_err());
    }


    #[test]
    fn runtime_publication_preflight_requires_verified_siblings_and_absent_destination() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-publication-preflight");
        let final_root = root.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        fs::create_dir(&provisioning.staging_root).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let identity = crate::coding_sandbox_fs::verified_directory_identity(
            &provisioning.staging_root, &stage,
        ).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: identity,
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };
        let publication = build_guest_runtime_publication_plan_with_final_root(
            &staging, &manifest, &anchor, &provisioning, final_root.clone(),
        ).unwrap();
        let preflight = authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &root, &parent, &stage, final_root.clone(),
        ).unwrap();
        assert_eq!(preflight.operation, operation);
        assert_eq!(preflight.staging_root_identity, identity);
        assert_eq!(preflight.final_root, final_root);
        assert_eq!(preflight.parent_path, root);
        assert_eq!(preflight.parent_identity.0, identity.0);

        fs::create_dir(&final_root).unwrap();
        assert!(authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &root, &parent, &stage, final_root.clone(),
        ).is_err());
        drop(stage);
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_publication_preflight_rejects_substituted_handles_and_intent() {
        let operation = GuestRunOperation::NodeNpmTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-publication-preflight-reject");
        let final_root = root.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        fs::create_dir(&provisioning.staging_root).unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let identity = crate::coding_sandbox_fs::verified_directory_identity(
            &provisioning.staging_root, &stage,
        ).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: identity,
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };
        let publication = build_guest_runtime_publication_plan_with_final_root(
            &staging, &manifest, &anchor, &provisioning, final_root.clone(),
        ).unwrap();

        let unrelated = root.join("unrelated");
        fs::create_dir(&unrelated).unwrap();
        let unrelated_handle = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, unrelated.file_name().unwrap(),
        ).unwrap();
        assert!(authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &root, &parent, &unrelated_handle, final_root.clone(),
        ).is_err());

        let mut changed = publication.clone();
        changed.final_root = root.join("redirected");
        assert!(authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &changed,
            &root, &parent, &stage, final_root.clone(),
        ).is_err());

        let wrong_parent = root.join("unrelated");
        assert!(authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &wrong_parent, &unrelated_handle, &stage, final_root,
        ).is_err());

        drop(unrelated_handle);
        drop(stage);
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }


    #[test]
    fn runtime_publication_atomically_moves_verified_directory_without_replacement() {
        let operation = GuestRunOperation::RustCargoTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-publication-atomic-success");
        let final_root = root.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        fs::create_dir(&provisioning.staging_root).unwrap();
        fs::write(provisioning.staging_root.join("runtime.bin"), b"verified-runtime").unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let identity = crate::coding_sandbox_fs::verified_directory_identity(
            &provisioning.staging_root, &stage,
        ).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: identity,
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };
        let publication = build_guest_runtime_publication_plan_with_final_root(
            &staging, &manifest, &anchor, &provisioning, final_root.clone(),
        ).unwrap();
        let preflight = authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &root, &parent, &stage, final_root.clone(),
        ).unwrap();

        let published = publish_guest_runtime_bundle_atomically(&preflight, &parent, stage).unwrap();
        assert_eq!(published.operation, operation);
        assert_eq!(published.root_path, final_root);
        assert_eq!(published.root_identity, identity);
        assert_eq!(published.bundle_id, anchor.bundle_id);
        assert_eq!(published.artifact_count, manifest.artifacts.len());
        assert_eq!(fs::read(published.root_path.join("runtime.bin")).unwrap(), b"verified-runtime");
        assert!(!provisioning.staging_root.exists());

        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_source_path_normalization_rejects_ambiguous_and_unsafe_names() {
        assert_eq!(normalize_guest_runtime_source_path(Path::new("bin/cargo.exe")).unwrap(), "bin\\\\cargo.exe");
        assert_eq!(normalize_guest_runtime_source_path(Path::new("BIN/CARGO.EXE")).unwrap(), "bin\\\\cargo.exe");
        for path in [
            "../cargo.exe",
            "bin/../cargo.exe",
            "bin//cargo.exe",
            "/bin/cargo.exe",
            "C:/bin/cargo.exe",
            "bin\\\\NUL.exe",
            "bin/cargo.exe/",
            "bin/cargo\\\\other.exe",
            "bin/é.exe",
        ] {
            assert!(normalize_guest_runtime_source_path(Path::new(path)).is_err(), "accepted unsafe source path: {path}");
        }
    }

    #[test]
    fn runtime_source_path_normalization_enforces_path_and_component_limits() {
        let component = format!("{}.bin", "a".repeat(MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES - 4));
        assert!(normalize_guest_runtime_source_path(Path::new(&component)).is_ok());
        let oversized_component = format!("{}.bin", "a".repeat(MAX_GUEST_RUNTIME_ARTIFACT_COMPONENT_BYTES));
        assert!(normalize_guest_runtime_source_path(Path::new(&oversized_component)).is_err());
        let oversized_path = format!("{}\\\\{}", "a".repeat(510), "b".repeat(510));
        assert!(normalize_guest_runtime_source_path(Path::new(&oversized_path)).is_err());
    }

    #[test]
    fn runtime_publication_never_replaces_a_raced_in_destination() {
        let operation = GuestRunOperation::NodeNpmTest;
        let manifest = test_runtime_manifest(operation);
        let anchor = test_runtime_manifest_trust_anchor(operation, &manifest);
        let root = make_test_directory("runtime-publication-atomic-no-replace");
        let final_root = root.join("published-runtime");
        let provisioning = build_guest_runtime_provisioning_plan_with_final_root(
            &manifest, &anchor, final_root.clone(),
        ).unwrap();
        fs::create_dir(&provisioning.staging_root).unwrap();
        fs::write(provisioning.staging_root.join("staged.bin"), b"keep-stage").unwrap();
        let parent = crate::coding_sandbox_fs::open_verified_root_directory(&root).unwrap();
        let stage = crate::coding_sandbox_fs::open_child_directory_nofollow(
            &parent, provisioning.staging_root.file_name().unwrap(),
        ).unwrap();
        let identity = crate::coding_sandbox_fs::verified_directory_identity(
            &provisioning.staging_root, &stage,
        ).unwrap();
        let staging = VerifiedGuestRuntimeStaging {
            operation,
            staging_root: provisioning.staging_root.clone(),
            root_identity: identity,
            bundle_id: anchor.bundle_id.clone(),
            manifest_sha256: anchor.manifest_sha256.to_ascii_lowercase(),
            artifact_count: manifest.artifacts.len(),
        };
        let publication = build_guest_runtime_publication_plan_with_final_root(
            &staging, &manifest, &anchor, &provisioning, final_root.clone(),
        ).unwrap();
        let preflight = authorize_guest_runtime_publication_preflight_with_final_root(
            &staging, &manifest, &anchor, &provisioning, &publication,
            &root, &parent, &stage, final_root.clone(),
        ).unwrap();

        // Simulate a competing creator after preflight. The atomic primitive,
        // not the earlier absence check, must be the final no-overwrite gate.
        fs::create_dir(&final_root).unwrap();
        fs::write(final_root.join("do-not-touch.txt"), b"preserve").unwrap();
        assert!(publish_guest_runtime_bundle_atomically(&preflight, &parent, stage).is_err());
        assert_eq!(fs::read(final_root.join("do-not-touch.txt")).unwrap(), b"preserve");
        assert_eq!(fs::read(provisioning.staging_root.join("staged.bin")).unwrap(), b"keep-stage");

        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }


}
