//! Encryption helpers for portable SOTArena solution archives.
//!
//! Archives use the interoperable age v1 format with X25519 recipients. The
//! plaintext tar stream is compressed directly into the age writer, so no
//! plaintext archive is written to disk.

use age::secrecy::ExposeSecret;
use anyhow::{anyhow, bail, ensure, Context, Result};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use zeroize::Zeroizing;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// A newly generated age X25519 identity and its public recipient.
///
/// The private identity is held in zeroizing memory. This type deliberately
/// does not implement `Debug` or `Display`.
pub struct KeyPair {
    recipient: String,
    identity: Zeroizing<String>,
}

impl KeyPair {
    /// Returns the portable public `age1…` recipient.
    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    /// Returns the private `AGE-SECRET-KEY-1…` identity.
    ///
    /// Callers must keep this value outside the repository and avoid logging
    /// or otherwise copying it unnecessarily.
    pub fn identity(&self) -> &str {
        self.identity.as_str()
    }
}

/// Generates a fresh age X25519 keypair.
pub fn generate_keypair() -> KeyPair {
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public().to_string();
    let secret = identity.to_string();
    KeyPair {
        recipient,
        identity: Zeroizing::new(secret.expose_secret().to_owned()),
    }
}

#[derive(Clone)]
struct ArchiveEntry {
    source: PathBuf,
    relative: PathBuf,
    is_directory: bool,
}

fn path_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_absent(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("{label} already exists: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
    }
}

fn canonical_new_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("output path must name a file or directory")?;
    let parent = fs::canonicalize(path_parent(path))
        .with_context(|| format!("cannot resolve output parent for {}", path.display()))?;
    Ok(parent.join(name))
}

fn safe_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty(),
        "archive contains an empty path"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "unsafe archive path {}",
        path.display()
    );
    Ok(())
}

fn collect_entries(source_root: &Path) -> Result<Vec<ArchiveEntry>> {
    fn visit(root: &Path, directory: &Path, entries: &mut Vec<ArchiveEntry>) -> Result<()> {
        let mut children = fs::read_dir(directory)
            .with_context(|| format!("cannot read {}", directory.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        children.sort_by_key(|entry| entry.file_name());

        for child in children {
            let source = child.path();
            let metadata = fs::symlink_metadata(&source)
                .with_context(|| format!("cannot inspect {}", source.display()))?;
            let file_type = metadata.file_type();
            let relative = source
                .strip_prefix(root)
                .expect("walked entry remains below root")
                .to_owned();
            safe_relative_path(&relative)?;

            ensure!(
                !file_type.is_symlink(),
                "symlink rejected: {}",
                source.display()
            );
            if file_type.is_dir() {
                entries.push(ArchiveEntry {
                    source: source.clone(),
                    relative,
                    is_directory: true,
                });
                visit(root, &source, entries)?;
            } else if file_type.is_file() {
                #[cfg(unix)]
                ensure!(
                    metadata.nlink() == 1,
                    "hardlinked file rejected: {}",
                    source.display()
                );
                entries.push(ArchiveEntry {
                    source,
                    relative,
                    is_directory: false,
                });
            } else {
                bail!("special file rejected: {}", source.display());
            }
        }
        Ok(())
    }

    let metadata = fs::symlink_metadata(source_root)
        .with_context(|| format!("cannot inspect {}", source_root.display()))?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "encryption input must be a real directory"
    );
    let mut entries = Vec::new();
    visit(source_root, source_root, &mut entries)?;
    Ok(entries)
}

/// Encrypts a directory to a new binary `.tar.zst.age` archive.
///
/// Every recipient must be an ASCII age X25519 `age1…` recipient. The output
/// is created atomically and is never overwritten. Symlinks, hardlinks, and
/// special files in the source tree are rejected.
pub fn encrypt_directory(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    recipients: &[String],
) -> Result<()> {
    let input = input.as_ref();
    let output = output.as_ref();
    ensure!(!recipients.is_empty(), "at least one recipient is required");
    ensure_absent(output, "encryption output")?;

    let input_metadata = fs::symlink_metadata(input)
        .with_context(|| format!("cannot inspect encryption input {}", input.display()))?;
    ensure!(
        input_metadata.file_type().is_dir() && !input_metadata.file_type().is_symlink(),
        "encryption input must be a real directory"
    );
    let canonical_input = fs::canonicalize(input)?;
    let canonical_output = canonical_new_path(output)?;
    ensure!(
        !canonical_output.starts_with(&canonical_input),
        "encryption output must not be inside the source directory"
    );

    let parsed = recipients
        .iter()
        .map(|recipient| {
            recipient
                .parse::<age::x25519::Recipient>()
                .map_err(|error| anyhow!("invalid age recipient: {error}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let entries = collect_entries(&canonical_input)?;
    let encryptor = age::Encryptor::with_recipients(
        parsed
            .iter()
            .map(|recipient| recipient as &dyn age::Recipient),
    )
    .context("cannot initialize age encryption")?;

    let parent = path_parent(output);
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("cannot create temporary output in {}", parent.display()))?;
    {
        let age_writer = encryptor
            .wrap_output(BufWriter::new(temporary.as_file_mut()))
            .context("cannot write age header")?;
        let zstd_writer = zstd::stream::write::Encoder::new(age_writer, 9)
            .context("cannot initialize zstd compression")?;
        let mut archive = tar::Builder::new(zstd_writer);
        archive.mode(tar::HeaderMode::Deterministic);
        for entry in entries {
            if entry.is_directory {
                archive
                    .append_dir(&entry.relative, &entry.source)
                    .with_context(|| format!("cannot archive {}", entry.source.display()))?;
            } else {
                archive
                    .append_path_with_name(&entry.source, &entry.relative)
                    .with_context(|| format!("cannot archive {}", entry.source.display()))?;
            }
        }
        archive.finish().context("cannot finish tar stream")?;
        let zstd_writer = archive.into_inner().context("cannot close tar stream")?;
        let age_writer = zstd_writer
            .finish()
            .context("cannot finish zstd compression")?;
        let mut file_writer = age_writer
            .finish()
            .context("cannot finish age encryption")?;
        file_writer
            .flush()
            .context("cannot flush encrypted output")?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("cannot sync encrypted output")?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| error.error)
        .with_context(|| format!("cannot publish encrypted output {}", output.display()))?;
    Ok(())
}

fn destination_is_empty_or_absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
                "decryption output already exists and is not a directory"
            );
            ensure!(
                fs::read_dir(path)?.next().is_none(),
                "decryption output directory is not empty"
            );
            Ok(true)
        }
    }
}

fn extract_archive<R: Read>(reader: R, destination: &Path) -> Result<()> {
    let decoder = zstd::stream::read::Decoder::new(reader)
        .context("encrypted payload is not a valid zstd stream")?;
    let mut archive = tar::Archive::new(decoder);
    let mut seen = BTreeSet::new();
    for entry in archive.entries().context("cannot read tar archive")? {
        let mut entry = entry.context("cannot read tar entry")?;
        let path = entry.path().context("invalid tar path")?.into_owned();
        safe_relative_path(&path)?;
        ensure!(
            seen.insert(path.clone()),
            "duplicate archive path {}",
            path.display()
        );
        let entry_type = entry.header().entry_type();
        ensure!(
            entry_type.is_file() || entry_type.is_dir(),
            "archive link or special file rejected: {}",
            path.display()
        );
        ensure!(
            entry.unpack_in(destination)?,
            "unsafe archive path {}",
            path.display()
        );
    }

    // Reading through EOF verifies the final zstd frame and age stream tag.
    let mut decoder = archive.into_inner();
    io::copy(&mut decoder, &mut io::sink()).context("truncated or corrupted encrypted payload")?;
    Ok(())
}

/// Decrypts an age archive into a new or empty directory.
///
/// The identity must be an ASCII `AGE-SECRET-KEY-1…` X25519 identity. Plaintext
/// is extracted into a temporary sibling directory and published by rename only
/// after the entire authenticated stream has been verified.
pub fn decrypt_directory(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    identity: &str,
) -> Result<()> {
    let input = input.as_ref();
    let output = output.as_ref();
    let input_metadata = fs::symlink_metadata(input)
        .with_context(|| format!("cannot inspect encrypted input {}", input.display()))?;
    ensure!(
        input_metadata.file_type().is_file() && !input_metadata.file_type().is_symlink(),
        "encrypted input must be a real file"
    );
    let output_existed = destination_is_empty_or_absent(output)?;
    let output_parent = path_parent(output);
    fs::canonicalize(output_parent)
        .with_context(|| format!("cannot resolve output parent {}", output_parent.display()))?;

    let parsed_identity = identity
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|error| anyhow!("invalid age identity: {error}"))?;
    let encrypted = BufReader::new(
        File::open(input).with_context(|| format!("cannot open {}", input.display()))?,
    );
    let decryptor = age::Decryptor::new(encrypted).context("invalid age archive")?;
    let plaintext = decryptor
        .decrypt(std::iter::once(&parsed_identity as &dyn age::Identity))
        .context("identity cannot decrypt archive")?;

    let temporary = TempBuilder::new()
        .prefix(".sotarena-decrypt-")
        .tempdir_in(output_parent)
        .with_context(|| {
            format!(
                "cannot create temporary directory in {}",
                output_parent.display()
            )
        })?;
    extract_archive(plaintext, temporary.path()).context("cannot extract encrypted archive")?;

    // Re-check immediately before publication to avoid replacing newly-created
    // content. Replacing an existing empty directory is content-preserving.
    match (output_existed, destination_is_empty_or_absent(output)) {
        (false, Ok(false)) => {}
        (true, Ok(true)) => fs::remove_dir(output)
            .with_context(|| format!("cannot replace empty output {}", output.display()))?,
        (_, Ok(_)) => bail!("decryption output changed during extraction"),
        (_, Err(error)) => return Err(error),
    }
    fs::rename(temporary.path(), output)
        .with_context(|| format!("cannot publish decrypted directory {}", output.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn recipient(key: &KeyPair) -> Vec<String> {
        vec![key.recipient().to_owned()]
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn generated_keys_are_unique_and_parseable() {
        let first = generate_keypair();
        let second = generate_keypair();
        assert_ne!(first.recipient(), second.recipient());
        assert_ne!(first.identity(), second.identity());
        assert!(first.recipient().starts_with("age1"));
        assert!(first.identity().starts_with("AGE-SECRET-KEY-1"));
        first.recipient().parse::<age::x25519::Recipient>().unwrap();
        first.identity().parse::<age::x25519::Identity>().unwrap();
    }

    #[test]
    fn multiple_recipients_round_trip_binary_executable_and_empty_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        write_file(&source.join("binary.bin"), &[0, 1, 2, 0xff, 0, 7]);
        write_file(&source.join("demo.sh"), b"#!/bin/sh\nexit 0\n");
        #[cfg(unix)]
        fs::set_permissions(source.join("demo.sh"), fs::Permissions::from_mode(0o755)).unwrap();

        let first = generate_keypair();
        let second = generate_keypair();
        let recipients = vec![first.recipient().to_owned(), second.recipient().to_owned()];
        let archive = workspace.path().join("solutions.tar.zst.age");
        encrypt_directory(&source, &archive, &recipients).unwrap();

        for (index, key) in [first, second].iter().enumerate() {
            let output = workspace.path().join(format!("output-{index}"));
            decrypt_directory(&archive, &output, key.identity()).unwrap();
            assert_eq!(
                fs::read(output.join("binary.bin")).unwrap(),
                [0, 1, 2, 0xff, 0, 7]
            );
            assert!(output.join("empty").is_dir());
            #[cfg(unix)]
            assert_eq!(
                fs::metadata(output.join("demo.sh"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
        }
    }

    fn assert_failed_decryption_leaves_no_plaintext(transform: impl FnOnce(&mut Vec<u8>)) {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir(&source).unwrap();
        write_file(&source.join("secret.txt"), b"secret");
        let key = generate_keypair();
        let archive = workspace.path().join("archive.age");
        encrypt_directory(&source, &archive, &recipient(&key)).unwrap();
        let mut bytes = fs::read(&archive).unwrap();
        transform(&mut bytes);
        write_file(&archive, &bytes);
        let output = workspace.path().join("output");
        assert!(decrypt_directory(&archive, &output, key.identity()).is_err());
        assert!(!output.exists());
        assert!(fs::read_dir(workspace.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".sotarena-decrypt-")));
    }

    #[test]
    fn tampering_and_truncation_fail_without_plaintext() {
        assert_failed_decryption_leaves_no_plaintext(|bytes| {
            let index = bytes.len() / 2;
            bytes[index] ^= 0x80;
        });
        assert_failed_decryption_leaves_no_plaintext(|bytes| {
            bytes.truncate(bytes.len() / 2);
        });
    }

    #[test]
    fn wrong_key_fails_without_plaintext() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir(&source).unwrap();
        write_file(&source.join("secret.txt"), b"secret");
        let right = generate_keypair();
        let wrong = generate_keypair();
        let archive = workspace.path().join("archive.age");
        encrypt_directory(&source, &archive, &recipient(&right)).unwrap();
        let output = workspace.path().join("output");
        assert!(decrypt_directory(&archive, &output, wrong.identity()).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn output_overwrites_and_source_contained_archives_are_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir(&source).unwrap();
        write_file(&source.join("file"), b"data");
        let key = generate_keypair();

        let contained = source.join("archive.age");
        assert!(encrypt_directory(&source, &contained, &recipient(&key)).is_err());
        assert!(!contained.exists());

        let archive = workspace.path().join("archive.age");
        write_file(&archive, b"existing");
        assert!(encrypt_directory(&source, &archive, &recipient(&key)).is_err());
        assert_eq!(fs::read(&archive).unwrap(), b"existing");
    }

    #[test]
    fn nonempty_extraction_is_rejected_and_empty_extraction_is_supported() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source");
        fs::create_dir(&source).unwrap();
        write_file(&source.join("file"), b"data");
        let key = generate_keypair();
        let archive = workspace.path().join("archive.age");
        encrypt_directory(&source, &archive, &recipient(&key)).unwrap();

        let nonempty = workspace.path().join("nonempty");
        fs::create_dir(&nonempty).unwrap();
        write_file(&nonempty.join("keep"), b"keep");
        assert!(decrypt_directory(&archive, &nonempty, key.identity()).is_err());
        assert_eq!(fs::read(nonempty.join("keep")).unwrap(), b"keep");

        let empty = workspace.path().join("empty");
        fs::create_dir(&empty).unwrap();
        decrypt_directory(&archive, &empty, key.identity()).unwrap();
        assert_eq!(fs::read(empty.join("file")).unwrap(), b"data");
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_hardlinks_and_special_files_are_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let key = generate_keypair();

        let symlinks = workspace.path().join("symlinks");
        fs::create_dir(&symlinks).unwrap();
        write_file(&symlinks.join("target"), b"data");
        symlink("target", symlinks.join("link")).unwrap();
        assert!(encrypt_directory(
            &symlinks,
            workspace.path().join("symlinks.age"),
            &recipient(&key)
        )
        .is_err());

        let hardlinks = workspace.path().join("hardlinks");
        fs::create_dir(&hardlinks).unwrap();
        write_file(&hardlinks.join("first"), b"data");
        fs::hard_link(hardlinks.join("first"), hardlinks.join("second")).unwrap();
        assert!(encrypt_directory(
            &hardlinks,
            workspace.path().join("hardlinks.age"),
            &recipient(&key)
        )
        .is_err());

        let special = workspace.path().join("special");
        fs::create_dir(&special).unwrap();
        let fifo = special.join("fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(encrypt_directory(
            &special,
            workspace.path().join("special.age"),
            &recipient(&key)
        )
        .is_err());
    }

    fn encrypt_tar_bytes(path: &Path, tar_bytes: &[u8], recipient: &str) {
        let parsed = recipient.parse::<age::x25519::Recipient>().unwrap();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&parsed as &dyn age::Recipient))
                .unwrap();
        let compressed = zstd::stream::encode_all(tar_bytes, 1).unwrap();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        let mut writer = encryptor.wrap_output(file).unwrap();
        writer.write_all(&compressed).unwrap();
        writer.finish().unwrap();
    }

    fn tar_with_path(path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, path, &b"evil"[..])
                .unwrap();
            archive.finish().unwrap();
        }
        bytes
    }

    fn rewrite_first_tar_path(bytes: &mut [u8], path: &[u8]) {
        assert!(path.len() < 100);
        bytes[..100].fill(0);
        bytes[..path.len()].copy_from_slice(path);
        bytes[148..156].fill(b' ');
        let checksum: u32 = bytes[..512].iter().map(|byte| u32::from(*byte)).sum();
        let encoded = format!("{checksum:06o}\0 ");
        bytes[148..156].copy_from_slice(encoded.as_bytes());
    }

    fn rewrite_first_tar_type(bytes: &mut [u8], entry_type: u8) {
        bytes[156] = entry_type;
        bytes[148..156].fill(b' ');
        let checksum: u32 = bytes[..512].iter().map(|byte| u32::from(*byte)).sum();
        let encoded = format!("{checksum:06o}\0 ");
        bytes[148..156].copy_from_slice(encoded.as_bytes());
    }

    #[test]
    fn unsafe_archive_paths_are_rejected_without_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let key = generate_keypair();
        let mut tar = tar_with_path("safe");
        rewrite_first_tar_path(&mut tar, b"../escape");
        let archive = workspace.path().join("unsafe.age");
        encrypt_tar_bytes(&archive, &tar, key.recipient());
        let output = workspace.path().join("output");
        assert!(decrypt_directory(&archive, &output, key.identity()).is_err());
        assert!(!output.exists());
        assert!(!workspace.path().join("escape").exists());
    }

    #[test]
    fn archive_links_and_special_files_are_rejected() {
        for (index, entry_type) in [b'1', b'2', b'6'].into_iter().enumerate() {
            let workspace = tempfile::tempdir().unwrap();
            let key = generate_keypair();
            let mut tar = tar_with_path("unsafe-entry");
            rewrite_first_tar_type(&mut tar, entry_type);
            let archive = workspace.path().join(format!("unsafe-{index}.age"));
            encrypt_tar_bytes(&archive, &tar, key.recipient());
            let output = workspace.path().join("output");
            assert!(decrypt_directory(&archive, &output, key.identity()).is_err());
            assert!(!output.exists());
        }
    }
}
