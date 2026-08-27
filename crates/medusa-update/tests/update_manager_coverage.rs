use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

use flate2::{Compression, write::GzEncoder};
use medusa_update::{
    Architecture, ArtifactKind, AtomicInstaller, BuildSource, GithubReleaseClient, KeyStatus,
    MANIFEST_NAME, MANIFEST_SCHEMA, ManifestArtifact, ManifestSignature, OperatingSystem, Platform,
    ReleaseClient, ReleaseEvidence, ReleaseId, ReleaseManifest, RolloutPolicy, SIGNATURE_NAME,
    SIGNATURE_SCHEMA, TrustStore, TrustedKey, UpdateCheck, copy_with_progress, verify_sha256,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use semver::Version;
use sha2::{Digest, Sha256};

const TEST_KEY_ID: &str = "integration-test-key";
const TEST_SEED: [u8; 32] = [19; 32];

fn server(
    build: impl FnOnce(&str) -> Vec<(u16, String, Vec<u8>)> + Send + 'static,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = format!("http://{}", listener.local_addr().expect("address"));
    let responses = build(&address);
    let worker = thread::spawn(move || {
        for (status, content_type, body) in responses {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read request");
            let reason = if status == 200 { "OK" } else { "Not Found" };
            let header = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).expect("headers");
            stream.write_all(&body).expect("body");
        }
    });
    (address, worker)
}

fn signed_release(base: &str, payload: &[u8]) -> (TrustStore, Vec<u8>, Vec<u8>, Vec<u8>) {
    let platform = Platform::current().expect("supported test platform");
    let artifact_name = match platform.os {
        OperatingSystem::Linux => "medusa-cli-linux.tar.gz",
        OperatingSystem::Macos => "medusa-cli-macos.tar.gz",
        OperatingSystem::Windows => "medusa-cli-windows.zip",
    };
    let digest = hex::encode(Sha256::digest(payload));
    let manifest = ReleaseManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        version: Version::new(1, 2, 0),
        release_id: Some(ReleaseId::parse("1.2.0.1").expect("release id")),
        minimum_updater_version: Version::new(1, 0, 0),
        source: BuildSource {
            repository: "benclawbot/Medusa".to_owned(),
            revision: "a".repeat(40),
            rust_toolchain: "1.88.0".to_owned(),
            cargo_lock_sha256: "b".repeat(64),
            desktop_lock_sha256: "c".repeat(64),
        },
        rollout: RolloutPolicy {
            channel: "stable".to_owned(),
            sequence: 7,
            percentage: 100,
        },
        artifacts: vec![ManifestArtifact {
            name: artifact_name.to_owned(),
            kind: ArtifactKind::CliArchive,
            platform,
            target: "integration-test-target".to_owned(),
            bytes: payload.len() as u64,
            sha256: digest.clone(),
        }],
        evidence: vec![ReleaseEvidence {
            name: artifact_name.to_owned(),
            bytes: payload.len() as u64,
            sha256: digest,
        }],
    };
    let manifest = serde_json::to_vec(&manifest).expect("manifest");
    let key_pair = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).expect("test key");
    let signature = ManifestSignature {
        schema: SIGNATURE_SCHEMA.to_owned(),
        key_id: TEST_KEY_ID.to_owned(),
        algorithm: "Ed25519".to_owned(),
        manifest_sha256: hex::encode(Sha256::digest(&manifest)),
        signature: hex::encode(key_pair.sign(&manifest).as_ref()),
    };
    let signature = serde_json::to_vec(&signature).expect("signature");
    let trust_store = TrustStore::new(vec![TrustedKey {
        key_id: TEST_KEY_ID.to_owned(),
        public_key: key_pair
            .public_key()
            .as_ref()
            .try_into()
            .expect("public key"),
        status: KeyStatus::Active,
        first_sequence: 1,
        last_sequence: None,
    }])
    .expect("trust store");
    let release = serde_json::json!({
        "tag_name": "v1.2.0.1",
        "draft": false,
        "prerelease": false,
        "assets": [
            {
                "name": MANIFEST_NAME,
                "browser_download_url": format!("{base}/manifest"),
                "size": manifest.len()
            },
            {
                "name": SIGNATURE_NAME,
                "browser_download_url": format!("{base}/signature"),
                "size": signature.len()
            },
            {
                "name": artifact_name,
                "browser_download_url": format!("{base}/artifact"),
                "size": payload.len()
            }
        ]
    });
    (
        trust_store,
        serde_json::to_vec(&release).expect("release"),
        manifest,
        signature,
    )
}

#[test]
fn discovers_verified_release_and_streams_platform_asset() {
    let payload = b"payload".to_vec();
    let trust_store_slot = Arc::new(Mutex::new(None));
    let (base, worker) = server({
        let payload = payload.clone();
        let trust_store_slot = Arc::clone(&trust_store_slot);
        move |base| {
            let (trust_store, release, manifest, signature) = signed_release(base, &payload);
            *trust_store_slot.lock().expect("trust store lock") = Some(trust_store);
            vec![
                (200, "application/json".into(), release),
                (200, "application/json".into(), manifest),
                (200, "application/json".into(), signature),
                (200, "application/octet-stream".into(), payload),
            ]
        }
    });
    let trust_store = trust_store_slot
        .lock()
        .expect("trust store lock")
        .take()
        .expect("trust store");
    let client =
        GithubReleaseClient::with_trust_store("acme/medusa", &base, trust_store).expect("client");
    let release = client
        .latest()
        .expect("release request")
        .expect("published release");
    assert_eq!(release.release_id.to_string(), "1.2.0.1");
    assert!(matches!(
        UpdateCheck::compare("1.1.9", release.version.clone()),
        UpdateCheck::Available { .. }
    ));
    let platform = Platform::current().expect("platform");
    let artifact = release.artifact_for(&platform).expect("platform artifact");
    let directory = tempfile::tempdir().expect("tempdir");
    let destination = directory.path().join("release-archive");
    let mut progress = Vec::new();
    client
        .download(artifact, &destination, |written, total| {
            progress.push((written, total));
        })
        .expect("download");
    verify_sha256(&destination, &artifact.sha256).expect("verified digest");
    assert_eq!(progress.last(), Some(&(7, Some(7))));
    worker.join().expect("server");
}

#[test]
fn absent_latest_release_is_not_an_updater_failure() {
    let (address, worker) = server(|_| vec![(404, "application/json".into(), Vec::new())]);
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

#[test]
fn interrupted_swap_restores_previous_binary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let target = directory.path().join(if cfg!(windows) {
        "medusa.exe"
    } else {
        "medusa"
    });
    let backup = if cfg!(windows) {
        target.with_extension("previous.exe")
    } else {
        target.with_extension("previous")
    };
    fs::write(&backup, b"old-binary").expect("backup");
    let installer = AtomicInstaller::new(target.clone());
    assert!(installer.recover_interrupted().expect("recover"));
    assert_eq!(fs::read(target).expect("restored"), b"old-binary");
}

#[test]
fn platform_literal_conversions_remain_compatible() {
    assert_eq!(
        OperatingSystem::try_from("linux").expect("linux"),
        OperatingSystem::Linux
    );
    assert_eq!(
        Architecture::try_from("x86_64").expect("x86_64"),
        Architecture::X86_64
    );
}
