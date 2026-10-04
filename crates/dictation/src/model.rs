use crate::resample;
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use async_compression::futures::bufread::GzipDecoder;
use futures::{
    AsyncRead, AsyncReadExt as _, StreamExt as _,
    io::{AllowStdIo, BufReader},
};
use http_client::{AsyncBody, HttpClient};
use parakeet_rs::{ParakeetTDT, Transcriber as _};
use sha2::{Digest, Sha256};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) struct ModelFile {
    pub(crate) name: &'static str,
    pub(crate) size: u64,
    pub(crate) sha256: &'static str,
}

const REPOSITORY: &str = "istupakov/parakeet-tdt-0.6b-v3-onnx";
const REVISION: &str = "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce";
const MODEL_DIRECTORY_NAME: &str = "parakeet-tdt-0.6b-v3-int8";
const MODEL_FILES: &[ModelFile] = &[
    ModelFile {
        name: "decoder_joint-model.int8.onnx",
        size: 18_202_004,
        sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
    },
    ModelFile {
        name: "encoder-model.int8.onnx",
        size: 652_183_999,
        sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
    },
    ModelFile {
        name: "vocab.txt",
        size: 93_939,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
];
const VERIFIED_MARKER_FILE: &str = "verified";

const ONNX_RUNTIME_VERSION: &str = "1.28.3";

pub(crate) enum ArchiveFormat {
    TarGz,
    Zip,
}

pub(crate) struct OnnxRuntime {
    os: &'static str,
    arch: &'static str,
    archive: ModelFile,
    archive_format: ArchiveFormat,
    library_path_in_archive: &'static str,
    library: ModelFile,
}

const ONNX_RUNTIMES: &[OnnxRuntime] = &[
    OnnxRuntime {
        os: "macos",
        arch: "aarch64",
        archive: ModelFile {
            name: "onnxruntime-osx-arm64-1.28.3.tgz",
            size: 33_057_743,
            sha256: "c436bd9f47dbce6f6311ccc21de97f829b3c862c09d24aa321958cb72f0a1d32",
        },
        archive_format: ArchiveFormat::TarGz,
        library_path_in_archive: "onnxruntime-osx-arm64-1.28.3/lib/libonnxruntime.1.28.3.dylib",
        library: ModelFile {
            name: "libonnxruntime.1.28.3.dylib",
            size: 39_350_024,
            sha256: "0c8707e83b3389849d03a3264308e905169b94b546f84df6bf662649bd89c3bb",
        },
    },
    OnnxRuntime {
        os: "linux",
        arch: "x86_64",
        archive: ModelFile {
            name: "onnxruntime-linux-x64-1.28.3.tgz",
            size: 9_130_098,
            sha256: "db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c",
        },
        archive_format: ArchiveFormat::TarGz,
        library_path_in_archive: "onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so.1.28.3",
        library: ModelFile {
            name: "libonnxruntime.so.1.28.3",
            size: 24_301_616,
            sha256: "5a1ce74e56e8c4b278d5a9e9d12b7192d9f70811084dde9616da4fa1710efc5a",
        },
    },
    OnnxRuntime {
        os: "linux",
        arch: "aarch64",
        archive: ModelFile {
            name: "onnxruntime-linux-aarch64-1.28.3.tgz",
            size: 8_123_797,
            sha256: "6c6b1ae96d7b0be9f555092857c6a4f2b0b5587a5298d732d9e52e8253038ce7",
        },
        archive_format: ArchiveFormat::TarGz,
        library_path_in_archive: "onnxruntime-linux-aarch64-1.28.3/lib/libonnxruntime.so.1.28.3",
        library: ModelFile {
            name: "libonnxruntime.so.1.28.3",
            size: 20_657_248,
            sha256: "c17d9c3b522c69594ef2b077d7f27a92ff0635d9df0c68c8997b7cad7bf71c61",
        },
    },
    OnnxRuntime {
        os: "windows",
        arch: "x86_64",
        archive: ModelFile {
            name: "onnxruntime-win-x64-1.28.3.zip",
            size: 78_606_946,
            sha256: "1d6fab48e85f948436af7c8c971d2c145cf224e2c444755dec894f8b0de11a83",
        },
        archive_format: ArchiveFormat::Zip,
        library_path_in_archive: "onnxruntime-win-x64-1.28.3/lib/onnxruntime.dll",
        library: ModelFile {
            name: "onnxruntime.dll",
            size: 15_828_832,
            sha256: "4d2774a5f64e4a230b16b74b167171e92f65b0683c19680c1d04fb284b92a6e2",
        },
    },
    OnnxRuntime {
        os: "windows",
        arch: "aarch64",
        archive: ModelFile {
            name: "onnxruntime-win-arm64-1.28.3.zip",
            size: 79_730_664,
            sha256: "76d5e5a23fb7fdbc2ca0c3214bad1b58b2a41016c7052e74e60ed8004bea71b1",
        },
        archive_format: ArchiveFormat::Zip,
        library_path_in_archive: "onnxruntime-win-arm64-1.28.3/lib/onnxruntime.dll",
        library: ModelFile {
            name: "onnxruntime.dll",
            size: 15_918_944,
            sha256: "20bcd3d4718376ef449d9520ec074d3c73938655e74f42c410de0e5aab6a8ed5",
        },
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    Missing,
    Downloading(DownloadProgress),
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

impl DownloadProgress {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

pub fn model_directory(data_directory: &Path) -> PathBuf {
    data_directory.join("models").join(MODEL_DIRECTORY_NAME)
}

pub fn model_download_size() -> u64 {
    total_size(MODEL_FILES) + current_onnx_runtime().map_or(0, |runtime| runtime.archive.size)
}

/// Checks file sizes only; the full checksum runs when a session loads the model.
pub fn model_status(data_directory: &Path) -> ModelStatus {
    let directory = model_directory(data_directory);
    if let Some(downloaded_bytes) = ActiveDownload::progress(&directory) {
        return ModelStatus::Downloading(DownloadProgress {
            downloaded_bytes,
            total_bytes: model_download_size(),
        });
    }
    let is_ready = current_onnx_runtime()
        .is_some_and(|runtime| is_installed(&directory, MODEL_FILES, runtime, &installed_marker()));
    if is_ready {
        ModelStatus::Ready
    } else {
        ModelStatus::Missing
    }
}

/// Dropping the returned future cancels the download and removes partial files.
pub async fn download_model(http_client: Arc<dyn HttpClient>, data_directory: &Path) -> Result<()> {
    let runtime = current_onnx_runtime().context(unsupported_platform_message())?;
    install(
        http_client.as_ref(),
        &model_directory(data_directory),
        MODEL_FILES,
        |file| {
            format!(
                "https://huggingface.co/{REPOSITORY}/resolve/{REVISION}/{}",
                file.name
            )
        },
        runtime,
        &format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{ONNX_RUNTIME_VERSION}/{}",
            runtime.archive.name
        ),
        &installed_marker(),
    )
    .await
}

fn current_onnx_runtime() -> Option<&'static OnnxRuntime> {
    ONNX_RUNTIMES.iter().find(|runtime| {
        runtime.os == std::env::consts::OS && runtime.arch == std::env::consts::ARCH
    })
}

fn unsupported_platform_message() -> String {
    format!(
        "Dictation is not available on {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn installed_marker() -> String {
    format!("{REVISION}\nonnxruntime {ONNX_RUNTIME_VERSION}")
}

fn total_size(files: &[ModelFile]) -> u64 {
    files.iter().map(|file| file.size).sum()
}

fn is_installed(
    directory: &Path,
    model_files: &[ModelFile],
    runtime: &OnnxRuntime,
    marker: &str,
) -> bool {
    model_files.iter().chain([&runtime.library]).all(|file| {
        std::fs::metadata(directory.join(file.name))
            .is_ok_and(|metadata| metadata.len() == file.size)
    }) && std::fs::read_to_string(directory.join(VERIFIED_MARKER_FILE))
        .is_ok_and(|contents| contents == marker)
}

async fn install(
    http_client: &dyn HttpClient,
    directory: &Path,
    model_files: &[ModelFile],
    model_url: impl Fn(&ModelFile) -> String,
    runtime: &OnnxRuntime,
    runtime_url: &str,
    marker: &str,
) -> Result<()> {
    let download = ActiveDownload::register(directory)?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("Could not create {}", directory.display()))?;
    remove_if_present(&directory.join(VERIFIED_MARKER_FILE))?;
    for file in model_files {
        download_file(
            http_client,
            directory,
            file,
            &model_url(file),
            &download.downloaded_bytes,
        )
        .await
        .and_then(|staging| staging.commit(&directory.join(file.name)))
        .with_context(|| format!("Could not download {}", file.name))?;
    }
    let archive = download_file(
        http_client,
        directory,
        &runtime.archive,
        runtime_url,
        &download.downloaded_bytes,
    )
    .await
    .context("Could not download ONNX Runtime")?;
    extract_library(&archive.path, runtime, directory)
        .await
        .context("Could not unpack ONNX Runtime")?;
    drop(archive);
    std::fs::write(directory.join(VERIFIED_MARKER_FILE), marker)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("Could not remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

async fn download_file(
    http_client: &dyn HttpClient,
    directory: &Path,
    file: &ModelFile,
    url: &str,
    downloaded_bytes: &AtomicU64,
) -> Result<StagingFile> {
    let mut response = http_client.get(url, AsyncBody::empty(), true).await?;
    ensure!(
        response.status().is_success(),
        "The server responded with {}",
        response.status()
    );
    receive_verified(
        response.body_mut(),
        file,
        directory.join(format!("{}.part", file.name)),
        |chunk_length| {
            downloaded_bytes.fetch_add(chunk_length, Ordering::Relaxed);
        },
    )
    .await
}

async fn receive_verified(
    mut reader: impl AsyncRead + Unpin,
    file: &ModelFile,
    staging_path: PathBuf,
    on_chunk: impl Fn(u64),
) -> Result<StagingFile> {
    let mut staging = StagingFile::create(staging_path)?;
    let mut hasher = Sha256::new();
    let mut received_bytes = 0u64;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        received_bytes += chunk.len() as u64;
        ensure!(
            received_bytes <= file.size,
            "The file is larger than expected"
        );
        hasher.update(chunk);
        staging.file.write_all(chunk)?;
        on_chunk(chunk.len() as u64);
    }
    ensure!(
        received_bytes == file.size && format!("{:x}", hasher.finalize()) == file.sha256,
        "Checksum verification failed"
    );
    Ok(staging)
}

async fn extract_library(
    archive_path: &Path,
    runtime: &OnnxRuntime,
    directory: &Path,
) -> Result<()> {
    let archive = BufReader::new(AllowStdIo::new(std::fs::File::open(archive_path)?));
    let staging_path = directory.join(format!("{}.part", runtime.library.name));
    let missing_library = || anyhow!("The archive has no {}", runtime.library_path_in_archive);
    let library = match runtime.archive_format {
        ArchiveFormat::TarGz => {
            let mut entries = async_tar::Archive::new(GzipDecoder::new(archive)).entries()?;
            loop {
                let entry = entries.next().await.ok_or_else(missing_library)??;
                if entry.path()?.to_str() == Some(runtime.library_path_in_archive) {
                    break receive_verified(entry, &runtime.library, staging_path, |_| {}).await?;
                }
            }
        }
        ArchiveFormat::Zip => {
            let mut zip = async_zip::base::read::stream::ZipFileReader::new(archive);
            loop {
                let mut item = zip.next_with_entry().await?.ok_or_else(missing_library)?;
                let is_library =
                    item.reader().entry().filename().as_str()? == runtime.library_path_in_archive;
                if is_library {
                    break receive_verified(
                        item.reader_mut(),
                        &runtime.library,
                        staging_path,
                        |_| {},
                    )
                    .await?;
                }
                zip = item.skip().await?;
            }
        }
    };
    library.commit(&directory.join(runtime.library.name))
}

static ACTIVE_DOWNLOADS: Mutex<Vec<(PathBuf, Arc<AtomicU64>)>> = Mutex::new(Vec::new());

struct ActiveDownload {
    directory: PathBuf,
    downloaded_bytes: Arc<AtomicU64>,
}

impl ActiveDownload {
    fn register(directory: &Path) -> Result<Self> {
        let mut active = ACTIVE_DOWNLOADS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if active
            .iter()
            .any(|(active_directory, _)| active_directory == directory)
        {
            bail!("The speech model is already downloading");
        }
        let downloaded_bytes = Arc::new(AtomicU64::new(0));
        active.push((directory.to_path_buf(), downloaded_bytes.clone()));
        Ok(Self {
            directory: directory.to_path_buf(),
            downloaded_bytes,
        })
    }

    fn progress(directory: &Path) -> Option<u64> {
        ACTIVE_DOWNLOADS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(active_directory, _)| active_directory == directory)
            .map(|(_, downloaded_bytes)| downloaded_bytes.load(Ordering::Relaxed))
    }
}

impl Drop for ActiveDownload {
    fn drop(&mut self) {
        ACTIVE_DOWNLOADS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(directory, _)| *directory != self.directory);
    }
}

struct StagingFile {
    path: PathBuf,
    file: std::fs::File,
    committed: bool,
}

impl StagingFile {
    fn create(path: PathBuf) -> Result<Self> {
        let file = std::fs::File::create(&path)
            .with_context(|| format!("Could not create {}", path.display()))?;
        Ok(Self {
            path,
            file,
            committed: false,
        })
    }

    fn commit(mut self, destination: &Path) -> Result<()> {
        self.file.sync_all()?;
        std::fs::rename(&self.path, destination)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("Could not remove {}: {error}", self.path.display());
        }
    }
}

pub(crate) struct Recognizer(ParakeetTDT);

impl Recognizer {
    pub(crate) fn load(model_directory: &Path) -> Result<Self> {
        let runtime = current_onnx_runtime().context(unsupported_platform_message())?;
        verify_checksums(
            model_directory,
            MODEL_FILES.iter().chain([&runtime.library]),
        )?;
        let library_path = model_directory.join(runtime.library.name);
        ort::init_from(&library_path)
            .with_context(|| format!("Could not load {}", library_path.display()))?
            .commit();
        ParakeetTDT::from_pretrained(model_directory, None)
            .map(Self)
            .map_err(|error| anyhow!("Could not load the speech model: {error}"))
    }

    pub(crate) fn transcribe(&mut self, samples: Vec<f32>, sample_rate: u32) -> Result<String> {
        ensure!(
            resample::is_supported_sample_rate(sample_rate),
            "Unsupported microphone sample rate"
        );
        ensure!(
            samples.len() <= crate::max_recording_samples(sample_rate),
            "The recording is longer than the limit"
        );
        let shorter_than_a_fifth_of_a_second = samples.len() < sample_rate as usize / 5;
        if shorter_than_a_fifth_of_a_second || samples.iter().all(|sample| sample.abs() < 0.0001) {
            return Ok(String::new());
        }
        let samples = resample::to_model_sample_rate(samples, sample_rate)?;
        self.0
            .transcribe_samples(samples, resample::MODEL_SAMPLE_RATE, 1, None)
            .map(|result| result.text)
            .map_err(|error| anyhow!("Could not transcribe the recording: {error}"))
    }
}

fn verify_checksums<'a>(
    directory: &Path,
    files: impl IntoIterator<Item = &'a ModelFile>,
) -> Result<()> {
    for file in files {
        let path = directory.join(file.name);
        let mut input = std::fs::File::open(&path)
            .with_context(|| format!("The speech model is missing {}", file.name))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut input, &mut hasher)?;
        ensure!(
            format!("{:x}", hasher.finalize()) == file.sha256,
            "The speech model is damaged. Download it again."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_compression::futures::bufread::GzipEncoder;
    use async_zip::{Compression, ZipEntryBuilder, base::write::ZipFileWriter};
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};

    const MODEL_URL: &str = "http://test.example/model/";
    const RUNTIME_URL: &str = "http://test.example/runtime";
    const LIBRARY_PATH_IN_ARCHIVE: &str = "runtime/lib/libfake.so";
    const LIBRARY_BYTES: &[u8] = b"native library";
    const MARKER: &str = "test-marker";

    const FAKE_MODEL_FILES: &[ModelFile] = &[
        ModelFile {
            name: "first.bin",
            size: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        },
        ModelFile {
            name: "second.bin",
            size: 5,
            sha256: "486ea46224d1bb4fb680f34f7c9ad96a8f24ec88be73ea8e5a6c65260e9cb8a7",
        },
    ];

    fn file_for(name: &'static str, bytes: &[u8]) -> ModelFile {
        ModelFile {
            name,
            size: bytes.len() as u64,
            sha256: Box::leak(format!("{:x}", Sha256::digest(bytes)).into_boxed_str()),
        }
    }

    fn tar_gz(path: &str, contents: &[u8]) -> Vec<u8> {
        block_on(async {
            let mut builder = async_tar::Builder::new(Vec::new());
            for (entry_path, entry_contents) in
                [("runtime/README", &b"readme"[..]), (path, contents)]
            {
                let mut header = async_tar::Header::new_gnu();
                header.set_size(entry_contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, entry_path, entry_contents)
                    .await
                    .unwrap();
            }
            let tar = builder.into_inner().await.unwrap();
            let mut compressed = Vec::new();
            GzipEncoder::new(&tar[..])
                .read_to_end(&mut compressed)
                .await
                .unwrap();
            compressed
        })
    }

    fn zip(path: &str, contents: &[u8]) -> Vec<u8> {
        block_on(async {
            let mut writer = ZipFileWriter::new(Vec::new());
            for (entry_path, entry_contents) in
                [("runtime/README", &b"readme"[..]), (path, contents)]
            {
                writer
                    .write_entry_whole(
                        ZipEntryBuilder::new(entry_path.to_owned().into(), Compression::Deflate),
                        entry_contents,
                    )
                    .await
                    .unwrap();
            }
            writer.close().await.unwrap()
        })
    }

    fn fake_runtime(archive_format: ArchiveFormat, archive: &[u8]) -> OnnxRuntime {
        OnnxRuntime {
            os: "test",
            arch: "test",
            archive: file_for("runtime.archive", archive),
            archive_format,
            library_path_in_archive: LIBRARY_PATH_IN_ARCHIVE,
            library: file_for("libfake.so", LIBRARY_BYTES),
        }
    }

    fn serve(responses: Vec<(String, Vec<u8>)>) -> Arc<dyn HttpClient> {
        let responses = Arc::new(responses);
        FakeHttpClient::create(move |request| {
            let responses = responses.clone();
            async move {
                let url = request.uri().to_string();
                let body = responses
                    .iter()
                    .find(|(response_url, _)| *response_url == url)
                    .map(|(_, body)| body.clone());
                Ok(match body {
                    Some(body) => Response::builder().status(200).body(body.into())?,
                    None => Response::builder().status(404).body(AsyncBody::empty())?,
                })
            }
        })
    }

    fn serve_package(second_file: &[u8], archive: &[u8]) -> Arc<dyn HttpClient> {
        serve(vec![
            (format!("{MODEL_URL}first.bin"), b"hello".to_vec()),
            (format!("{MODEL_URL}second.bin"), second_file.to_vec()),
            (RUNTIME_URL.to_owned(), archive.to_vec()),
        ])
    }

    fn run_install(
        http_client: &dyn HttpClient,
        directory: &Path,
        runtime: &OnnxRuntime,
    ) -> Result<()> {
        block_on(install(
            http_client,
            directory,
            FAKE_MODEL_FILES,
            |file| format!("{MODEL_URL}{}", file.name),
            runtime,
            RUNTIME_URL,
            MARKER,
        ))
    }

    fn file_names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn pinned_files_are_well_formed() {
        assert_eq!(REVISION.len(), 40);
        assert_eq!(total_size(MODEL_FILES), 670_479_942);
        let runtime_files = ONNX_RUNTIMES
            .iter()
            .flat_map(|runtime| [&runtime.archive, &runtime.library]);
        assert!(
            MODEL_FILES
                .iter()
                .chain(runtime_files)
                .all(|file| file.sha256.len() == 64 && !file.name.contains('/'))
        );
        assert!(ONNX_RUNTIMES.iter().all(|runtime| {
            runtime.archive.name.contains(ONNX_RUNTIME_VERSION)
                && runtime
                    .library_path_in_archive
                    .ends_with(runtime.library.name)
        }));
        assert_eq!(
            model_download_size(),
            670_479_942 + current_onnx_runtime().map_or(0, |runtime| runtime.archive.size)
        );
    }

    #[test]
    fn verified_install_from_either_archive_format_establishes_readiness() {
        for (archive_format, archive) in [
            (
                ArchiveFormat::TarGz,
                tar_gz(LIBRARY_PATH_IN_ARCHIVE, LIBRARY_BYTES),
            ),
            (
                ArchiveFormat::Zip,
                zip(LIBRARY_PATH_IN_ARCHIVE, LIBRARY_BYTES),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let runtime = fake_runtime(archive_format, &archive);
            let http_client = serve_package(b"world", &archive);
            assert!(!is_installed(
                directory.path(),
                FAKE_MODEL_FILES,
                &runtime,
                MARKER
            ));
            run_install(http_client.as_ref(), directory.path(), &runtime).unwrap();
            assert!(is_installed(
                directory.path(),
                FAKE_MODEL_FILES,
                &runtime,
                MARKER
            ));
            verify_checksums(
                directory.path(),
                FAKE_MODEL_FILES.iter().chain([&runtime.library]),
            )
            .unwrap();
            assert_eq!(
                file_names(directory.path()),
                ["first.bin", "libfake.so", "second.bin", "verified"]
            );
            assert_eq!(ActiveDownload::progress(directory.path()), None);
        }
    }

    #[test]
    fn corrupt_model_file_never_establishes_readiness_or_leaves_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_gz(LIBRARY_PATH_IN_ARCHIVE, LIBRARY_BYTES);
        let runtime = fake_runtime(ArchiveFormat::TarGz, &archive);
        let http_client = serve_package(b"WORLD", &archive);
        assert!(run_install(http_client.as_ref(), directory.path(), &runtime).is_err());
        assert!(!is_installed(
            directory.path(),
            FAKE_MODEL_FILES,
            &runtime,
            MARKER
        ));
        assert_eq!(file_names(directory.path()), ["first.bin"]);
    }

    #[test]
    fn archive_without_the_library_fails_and_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let archive = zip("runtime/lib/other.so", LIBRARY_BYTES);
        let runtime = fake_runtime(ArchiveFormat::Zip, &archive);
        let http_client = serve_package(b"world", &archive);
        assert!(run_install(http_client.as_ref(), directory.path(), &runtime).is_err());
        assert!(!is_installed(
            directory.path(),
            FAKE_MODEL_FILES,
            &runtime,
            MARKER
        ));
        assert_eq!(file_names(directory.path()), ["first.bin", "second.bin"]);
    }

    #[test]
    fn tampered_library_inside_a_valid_archive_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_gz(LIBRARY_PATH_IN_ARCHIVE, b"other library!");
        let runtime = fake_runtime(ArchiveFormat::TarGz, &archive);
        let http_client = serve_package(b"world", &archive);
        assert!(run_install(http_client.as_ref(), directory.path(), &runtime).is_err());
        assert_eq!(file_names(directory.path()), ["first.bin", "second.bin"]);
    }

    #[test]
    fn oversized_or_missing_file_fails_the_install() {
        let directory = tempfile::tempdir().unwrap();
        let archive = tar_gz(LIBRARY_PATH_IN_ARCHIVE, LIBRARY_BYTES);
        let runtime = fake_runtime(ArchiveFormat::TarGz, &archive);
        let http_client = serve_package(b"world!", &archive);
        assert!(run_install(http_client.as_ref(), directory.path(), &runtime).is_err());
        assert_eq!(file_names(directory.path()), ["first.bin"]);

        let http_client = serve(vec![(format!("{MODEL_URL}first.bin"), b"hello".to_vec())]);
        assert!(run_install(http_client.as_ref(), directory.path(), &runtime).is_err());
        assert!(!is_installed(
            directory.path(),
            FAKE_MODEL_FILES,
            &runtime,
            MARKER
        ));
    }

    #[test]
    fn concurrent_download_into_the_same_directory_is_refused_and_reports_progress() {
        let directory = tempfile::tempdir().unwrap();
        let download = ActiveDownload::register(directory.path()).unwrap();
        download.downloaded_bytes.store(42, Ordering::Relaxed);
        assert_eq!(ActiveDownload::progress(directory.path()), Some(42));
        assert!(ActiveDownload::register(directory.path()).is_err());
        drop(download);
        assert_eq!(ActiveDownload::progress(directory.path()), None);
    }

    #[test]
    fn missing_model_reports_missing_status() {
        let data_directory = tempfile::tempdir().unwrap();
        assert_eq!(model_status(data_directory.path()), ModelStatus::Missing);
    }

    #[test]
    fn partial_or_corrupt_model_cannot_load() {
        let directory = tempfile::tempdir().unwrap();
        let first_file = &MODEL_FILES[0];
        std::fs::write(directory.path().join(first_file.name), b"corrupt").unwrap();
        assert!(Recognizer::load(directory.path()).is_err());
    }

    #[test]
    fn progress_fraction_is_bounded() {
        let progress = |downloaded_bytes, total_bytes| DownloadProgress {
            downloaded_bytes,
            total_bytes,
        };
        assert_eq!(progress(0, 0).fraction(), 0.0);
        assert_eq!(progress(50, 100).fraction(), 0.5);
        assert_eq!(progress(150, 100).fraction(), 1.0);
    }
}
