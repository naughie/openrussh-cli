use anyhow::{Context as _, Result, ensure};
use russh_sftp::client::fs::ReadDir as RemoteReadDir;
use russh_sftp::client::{SftpSession, error::Error as SftpError};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::fs;

use std::fs::Metadata;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::str;

pub(super) async fn upload(
    session: &SftpSession,
    sources: &[PathBuf],
    destination: &str,
    recursive: bool,
) -> Result<()> {
    let directory = remote_metadata(session, destination)
        .await?
        .is_some_and(|metadata| metadata.is_dir());
    ensure!(
        sources.len() <= 1 || directory,
        "Multiple sources require an existing destination directory: {destination}"
    );

    for source in sources {
        let target = if directory {
            if local_basename(source) == Some(b"..".as_slice()) {
                let canonical = fs::canonicalize(source)
                    .await
                    .with_context(|| format!("Read local source {}", source.display()))?;
                remote_join(destination, local_name(&canonical)?)
            } else {
                remote_join(destination, local_name(source)?)
            }
        } else {
            destination.to_owned()
        };
        upload_source(session, source, target, recursive).await?;
    }
    Ok(())
}

struct UploadDir {
    entries: fs::ReadDir,
    destination: String,
    canonical: PathBuf,
}

async fn upload_source(
    session: &SftpSession,
    source: &Path,
    destination: String,
    recursive: bool,
) -> Result<()> {
    let metadata = fs::metadata(source)
        .await
        .with_context(|| format!("Read local source {}", source.display()))?;
    if metadata.is_file() {
        return upload_file(session, source, &destination, &metadata).await;
    }
    ensure!(
        metadata.is_dir(),
        "Unsupported local file type: {}",
        source.display()
    );
    ensure!(recursive, "{} is a directory (use -r)", source.display());

    let mut stack = vec![upload_dir(session, source, destination).await?];
    while let Some(dir) = stack.last_mut() {
        let Some(entry) = dir.entries.next_entry().await? else {
            stack.pop();
            continue;
        };
        let source = entry.path();
        let destination = remote_join(&dir.destination, local_name(&source)?);
        let metadata = fs::metadata(&source)
            .await
            .with_context(|| format!("Read local source {}", source.display()))?;

        if metadata.is_dir() {
            let canonical = fs::canonicalize(&source).await?;
            ensure!(
                stack.iter().all(|dir| dir.canonical != canonical),
                "Directory cycle at {}",
                source.display()
            );
            ensure_remote_dir(session, &destination).await?;
            stack.push(UploadDir {
                entries: fs::read_dir(&source).await?,
                destination,
                canonical,
            });
        } else {
            ensure!(
                metadata.is_file(),
                "Unsupported local file type: {}",
                source.display()
            );
            upload_file(session, &source, &destination, &metadata).await?;
        }
    }
    Ok(())
}

async fn upload_dir(
    session: &SftpSession,
    source: &Path,
    destination: String,
) -> Result<UploadDir> {
    ensure_remote_dir(session, &destination).await?;
    Ok(UploadDir {
        entries: fs::read_dir(source)
            .await
            .with_context(|| format!("Read local directory {}", source.display()))?,
        destination,
        canonical: fs::canonicalize(source).await?,
    })
}

async fn upload_file(
    session: &SftpSession,
    source: &Path,
    destination: &str,
    metadata: &Metadata,
) -> Result<()> {
    if let Some(metadata) = remote_metadata(session, destination).await? {
        ensure!(
            metadata.is_regular(),
            "Remote destination is not a regular file: {destination}"
        );
    }
    let mut source = fs::File::open(source)
        .await
        .with_context(|| format!("Open local file {}", source.display()))?;
    let attributes = FileAttributes {
        permissions: Some(metadata.permissions().mode() & 0o777),
        ..FileAttributes::empty()
    };
    let mut target = session
        .open_with_flags_and_attributes(
            destination,
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            attributes,
        )
        .await
        .with_context(|| format!("Create remote file {destination}"))?;
    let result = tokio::io::copy(&mut source, &mut target)
        .await
        .with_context(|| format!("Write remote file {destination}"));
    let close = target
        .close()
        .await
        .with_context(|| format!("Close remote file {destination}"));
    result?;
    close
}

pub(super) async fn download(
    session: &SftpSession,
    sources: &[String],
    destination: &Path,
    recursive: bool,
) -> Result<()> {
    let directory = local_metadata(destination)
        .await?
        .is_some_and(|metadata| metadata.is_dir());
    ensure!(
        sources.len() <= 1 || directory,
        "Multiple sources require an existing destination directory: {}",
        destination.display()
    );

    for source in sources {
        let target = if directory {
            if source.trim_end_matches('/').rsplit('/').next() == Some("..") {
                let canonical = session
                    .canonicalize(source.as_str())
                    .await
                    .with_context(|| format!("Read remote source {source}"))?;
                destination.join(remote_name(&canonical)?)
            } else {
                destination.join(remote_name(source)?)
            }
        } else {
            destination.to_owned()
        };
        download_source(session, source, target, recursive).await?;
    }
    Ok(())
}

struct DownloadDir {
    entries: RemoteReadDir,
    destination: PathBuf,
    canonical: String,
}

async fn download_source(
    session: &SftpSession,
    source: &str,
    destination: PathBuf,
    recursive: bool,
) -> Result<()> {
    let metadata = session
        .metadata(source)
        .await
        .with_context(|| format!("Read remote source {source}"))?;
    if metadata.is_regular() {
        return download_file(session, source, &destination, &metadata).await;
    }
    ensure!(metadata.is_dir(), "Unsupported remote file type: {source}");
    ensure!(recursive, "{source} is a directory (use -r)");

    let mut stack = vec![download_dir(session, source, destination).await?];
    while let Some(dir) = stack.last_mut() {
        let Some(entry) = dir.entries.next() else {
            stack.pop();
            continue;
        };
        let name = entry.file_name();
        validate_name(&name)?;
        let source = entry.path();
        let destination = dir.destination.join(&name);
        let metadata = entry.metadata();
        let metadata = if metadata.is_symlink() || metadata.permissions.is_none() {
            session
                .metadata(source.as_str())
                .await
                .with_context(|| format!("Read remote source {source}"))?
        } else {
            metadata
        };

        if metadata.is_dir() {
            let canonical = session.canonicalize(source.as_str()).await?;
            ensure!(
                stack.iter().all(|dir| dir.canonical != canonical),
                "Directory cycle at {source}"
            );
            ensure_local_dir(&destination).await?;
            stack.push(DownloadDir {
                entries: session.read_dir(source).await?,
                destination,
                canonical,
            });
        } else {
            ensure!(
                metadata.is_regular(),
                "Unsupported remote file type: {source}"
            );
            download_file(session, &source, &destination, &metadata).await?;
        }
    }
    Ok(())
}

async fn download_dir(
    session: &SftpSession,
    source: &str,
    destination: PathBuf,
) -> Result<DownloadDir> {
    ensure_local_dir(&destination).await?;
    Ok(DownloadDir {
        entries: session
            .read_dir(source)
            .await
            .with_context(|| format!("Read remote directory {source}"))?,
        destination,
        canonical: session.canonicalize(source).await?,
    })
}

async fn download_file(
    session: &SftpSession,
    source: &str,
    destination: &Path,
    metadata: &FileAttributes,
) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;

    if let Some(metadata) = local_metadata(destination).await? {
        ensure!(
            metadata.is_file(),
            "Local destination is not a regular file: {}",
            destination.display()
        );
    }
    let mut source_file = session
        .open(source)
        .await
        .with_context(|| format!("Open remote file {source}"))?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(metadata.permissions.unwrap_or(0o666) & 0o777)
        .open(destination)
        .await
        .with_context(|| format!("Create local file {}", destination.display()))?;
    let result = tokio::io::copy(&mut source_file, &mut target)
        .await
        .with_context(|| format!("Read remote file {source}"));
    let close = source_file
        .close()
        .await
        .with_context(|| format!("Close remote file {source}"));
    result?;
    target
        .flush()
        .await
        .with_context(|| format!("Flush local file {}", destination.display()))?;
    close
}

async fn remote_metadata(session: &SftpSession, path: &str) -> Result<Option<FileAttributes>> {
    match session.metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Read remote destination {path}")),
    }
}

async fn local_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Read local destination {}", path.display())),
    }
}

async fn ensure_remote_dir(session: &SftpSession, path: &str) -> Result<()> {
    if let Some(metadata) = remote_metadata(session, path).await? {
        ensure!(
            metadata.is_dir(),
            "Remote destination is not a directory: {path}"
        );
        return Ok(());
    }
    session
        .create_dir(path)
        .await
        .with_context(|| format!("Create remote directory {path}"))
}

async fn ensure_local_dir(path: &Path) -> Result<()> {
    if let Some(metadata) = local_metadata(path).await? {
        ensure!(
            metadata.is_dir(),
            "Local destination is not a directory: {}",
            path.display()
        );
        return Ok(());
    }
    fs::create_dir(path)
        .await
        .with_context(|| format!("Create local directory {}", path.display()))
}

fn local_name(path: &Path) -> Result<&str> {
    let name = local_basename(path);
    if name == Some(b".".as_slice()) || name.is_none() && path.has_root() {
        return Ok(".");
    }
    let name = name
        .and_then(|name| str::from_utf8(name).ok())
        .with_context(|| format!("Local source needs a UTF-8 file name: {}", path.display()))?;
    validate_name(name)?;
    Ok(name)
}

fn local_basename(path: &Path) -> Option<&[u8]> {
    path.as_os_str()
        .as_encoded_bytes()
        .rsplit(|&c| c == b'/')
        .find(|name| !name.is_empty())
}

fn remote_name(path: &str) -> Result<&str> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if name == "." || name.is_empty() && path.starts_with('/') {
        return Ok(".");
    }
    validate_name(name)?;
    Ok(name)
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']),
        "Invalid remote file name: {name:?}"
    );
    Ok(())
}

fn remote_join(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_are_single_posix_components() {
        for (path, name) in [
            ("file", "file"),
            ("dir/file", "file"),
            ("/dir/file/", "file"),
            ("dir/sp ace'✓", "sp ace'✓"),
        ] {
            assert_eq!(remote_name(path).unwrap(), name, "{path:?}");
        }
        for name in ["", ".", "..", "/absolute", "dir/file", "nul\0name"] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn root_sources_copy_contents_into_an_existing_directory() {
        for path in [".", "./", "/", "dir/.", "dir/.//", "dir/../."] {
            assert_eq!(local_name(Path::new(path)).unwrap(), ".");
            assert_eq!(remote_name(path).unwrap(), ".");
        }
    }
}
