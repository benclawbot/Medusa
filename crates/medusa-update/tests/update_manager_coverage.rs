use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
};

use flate2::{Compression, write::GzEncoder};
#[cfg(unix)]
use medusa_update::Restart;
use medusa_update::{
    AtomicInstaller, GithubReleaseClient, Platform, ReleaseClient, UpdateCheck, copy_with_progress,
    verify_sha256,
};
use semver::Version;

fn server(
    build: impl FnOnce(&str) -> Vec<(String, Vec<u8>)> + Send + 'static,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = format!("http://{}", listener.local_addr().expect("address"));
    let responses = build(&address);
    let worker = thread::spawn(move || {
        for (content_type, body) in responses {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read request");
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).expect("headers");
            stream.write_all(&body).expect("body");
        }
    });
    (address, worker)
}

#[test]
fn discovers_manifest_backed_release_and_streams_platform_asset() {
    let manifest = br#"{"schema":"medusa-release-manifest-v1","assets":[{"path":"medusa-cli-linux.tar.gz","bytes":7,"sha256":"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"}]}"#.to_vec();
    let (base, worker) = server(move |base| {
        vec![
        (
            "application/json".into(),
            format!(
                r#"{{"tag_name":"v1.2.0","draft":false,"prerelease":false,"assets":[{{"name":"medusa-release-manifest.json","browser_download_url":"{base}/manifest","size":{},"digest":"sha256:ignored"}},{{"name":"medusa-cli-linux.tar.gz","browser_download_url":"{base}/linux","size":7}}]}}"#,
                manifest.len()
            )
            .into_bytes(),
        ),
        ("application/json".into(), manifest),
        ("application/octet-stream".into(), b"payload".to_vec()),
    ]
    });
    let client = GithubReleaseClient::new("acme/medusa", &base).expect("client");
    let release = client
        .latest()
        .expect("release request")
        .expect("published release");
    assert!(matches!(
        UpdateCheck::compare("1.1.9", release.version.clone()),
        UpdateCheck::Available { .. }
    ));
    let artifact = release
        .artifact_for(&Platform {
            os: "linux".into(),
            architecture: "x86_64".into(),
        })
        .expect("linux artifact");
    let directory = tempfile::tempdir().expect("tempdir");
    let destination = directory.path().join("release.tgz");
    let mut progress = Vec::new();
    client
        .download(artifact, &destination, |written, total| {
            progress.push((written, total))
        })
        .expect("download");
    verify_sha256(&destination, &artifact.sha256).expect("verified digest");
    assert_eq!(progress.last(), Some(&(7, Some(7))));
    worker.join().expect("server");
}

#[test]
fn absent_latest_release_is_not_an_updater_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = format!("http://{}", listener.local_addr().expect("address"));
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).expect("read request");
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("response");
    });
    let client = GithubReleaseClient::new("acme/medusa", address).expect("client");
    assert!(client.latest().expect("request").is_none());
    worker.join().expect("server");
}

#[test]
fn progress_copy_and_semantic_edge_cases_are_explicit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let destination = directory.path().join("payload");
    let mut source = &b"streamed bytes"[..];
    let mut seen = Vec::new();
    assert_eq!(
        copy_with_progress(&mut source, &destination, Some(14), |written, total| seen
            .push((written, total)))
        .expect("copy"),
        14
    );
    assert_eq!(fs::read(destination).expect("bytes"), b"streamed bytes");
    assert!(matches!(
        UpdateCheck::compare("development", Version::parse("1.0.0").expect("version")),
        UpdateCheck::CurrentBuildUnparseable { .. }
    ));
    assert_eq!(seen.last(), Some(&(14, Some(14))));
}

#[test]
fn tar_archives_extract_only_medusa_binary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let archive = directory.path().join("release.tar.gz");
    let output = directory.path().join("output");
    let compressed = fs::File::create(&archive).expect("archive");
    let encoder = GzEncoder::new(compressed, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(10);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, Path::new("bin/medusa"), &b"new-binary"[..])
        .expect("entry");
    let encoder = tar.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip");
    let extracted = AtomicInstaller::new(directory.path().join("target"))
        .extract_archive(&archive, &output)
        .expect("extract");
    assert_eq!(
        extracted.file_name().and_then(|name| name.to_str()),
        Some("medusa")
    );
    assert_eq!(fs::read(extracted).expect("binary"), b"new-binary");
}

#[cfg(unix)]
#[test]
fn successful_replacement_keeps_rollback_binary() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tempdir");
    let target = directory.path().join("medusa");
    let candidate = directory.path().join("candidate");
    fs::write(&target, b"old-binary").expect("target");
    fs::write(&candidate, b"#!/bin/sh\nexit 0\n").expect("candidate");
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).expect("permissions");
    let backup = AtomicInstaller::new(target.clone())
        .replace(&candidate, &Restart::default())
        .expect("replace")
        .expect("unix backup");
    assert_eq!(fs::read(backup).expect("backup"), b"old-binary");
    assert!(
        fs::read_to_string(target)
            .expect("new target")
            .contains("exit 0")
    );
}
