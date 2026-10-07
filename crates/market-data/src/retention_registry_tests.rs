//! Registry parsing and receipt verification. The fail-safe direction is the
//! subject of most of these: every malformed input must come out *protected*
//! (for designation) or *unverified* (for receipts), never the other way.

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "retention-registry-{tag}-{}-{}-{n}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn protect(dir: &Path, day: &str, body: &str) {
    let p = dir.join(REGISTRY_DIR).join(PROTECTED_DIR);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join(format!("{day}.json")), body).unwrap();
}

fn valid_protection(day: &str) -> String {
    format!(
        r#"{{"schemaVersion":1,"date":"{day}","class":"designated","reason":"OI V2 development session",
            "protectedBy":"roman","protectedAt":"2026-09-25T12:00:00Z"}}"#
    )
}

fn receipt_json(day: &str, files: &[(&str, u64, &str)]) -> String {
    let files: Vec<String> = files
        .iter()
        .map(|(n, b, h)| format!(r#"{{"name":"{n}","bytes":{b},"sha256":"{h}"}}"#))
        .collect();
    format!(
        r#"{{"schemaVersion":1,"date":"{day}","destination":"H:/evidence (off-box)",
            "verifiedAt":"2026-09-25T01:00:00Z","verifiedBy":"roman","files":[{}]}}"#,
        files.join(",")
    )
}

fn write_receipt(dir: &Path, day: &str, body: &str) {
    let p = dir.join(REGISTRY_DIR).join(EXPORTS_DIR);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join(format!("{day}.json")), body).unwrap();
}

#[test]
fn no_registry_means_every_day_is_ordinary() {
    let dir = temp_dir("none");
    let index = ProtectionIndex::load(&dir);
    assert_eq!(index.of(date("2026-09-21")), Protection::Ordinary);
    assert!(!index.globally_protected());
    assert!(index.errors().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_valid_designation_protects_exactly_its_day() {
    let dir = temp_dir("valid");
    protect(&dir, "2026-09-21", &valid_protection("2026-09-21"));
    let index = ProtectionIndex::load(&dir);
    assert!(matches!(
        index.of(date("2026-09-21")),
        Protection::Protected {
            class: ProtectionClass::Designated,
            ..
        }
    ));
    assert_eq!(index.of(date("2026-09-22")), Protection::Ordinary);
    assert!(index.errors().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every flavour of broken designation still protects its day.
#[test]
fn a_malformed_designation_fails_safe_to_protected() {
    let dir = temp_dir("malformed");
    protect(&dir, "2026-09-21", "{not json");
    protect(&dir, "2026-09-22", ""); // empty file
    protect(&dir, "2026-09-23", &valid_protection("2026-09-24")); // wrong date inside
    protect(
        &dir,
        "2026-09-24",
        &valid_protection("2026-09-24").replace("\"schemaVersion\":1", "\"schemaVersion\":2"),
    );
    protect(
        &dir,
        "2026-09-25",
        &valid_protection("2026-09-25").replace("designated", "maybe"),
    );
    let index = ProtectionIndex::load(&dir);
    for day in [
        "2026-09-21",
        "2026-09-22",
        "2026-09-23",
        "2026-09-24",
        "2026-09-25",
    ] {
        let p = index.of(date(day));
        assert!(matches!(p, Protection::Malformed { .. }), "{day}: {p:?}");
        assert!(p.is_protected(), "{day} must be enforced as protected");
    }
    assert_eq!(index.errors().len(), 5, "and each one reported");
    assert_eq!(
        index.of(date("2026-09-10")),
        Protection::Ordinary,
        "other days unaffected"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A registry entry that cannot be attributed to a day protects every day.
#[test]
fn an_unattributable_registry_entry_protects_everything() {
    let dir = temp_dir("stray");
    protect(&dir, "2026-09-21", &valid_protection("2026-09-21"));
    std::fs::write(
        dir.join(REGISTRY_DIR)
            .join(PROTECTED_DIR)
            .join("2026-9-22.json"),
        "{}",
    )
    .unwrap();
    let index = ProtectionIndex::load(&dir);
    assert!(index.globally_protected());
    assert!(index.of(date("2026-01-01")).is_protected());
    assert!(!index.errors().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_registry_path_that_is_not_a_directory_protects_everything() {
    let dir = temp_dir("notdir");
    std::fs::create_dir_all(dir.join(REGISTRY_DIR)).unwrap();
    std::fs::write(dir.join(REGISTRY_DIR).join(PROTECTED_DIR), "oops").unwrap();
    assert!(ProtectionIndex::load(&dir).globally_protected());

    let dir2 = temp_dir("notdir2");
    std::fs::write(dir2.join(REGISTRY_DIR), "oops").unwrap();
    assert!(ProtectionIndex::load(&dir2).globally_protected());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn a_matching_receipt_verifies_and_every_mismatch_does_not() {
    let dir = temp_dir("verify");
    let data = dir.join("episodes-2026-09-21.ndjson");
    std::fs::write(&data, b"hello\n").unwrap();
    let sha = sha256_file(&data).unwrap();
    assert_eq!(
        sha,
        "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    );
    let cache = HashCache::default();
    let name = "episodes-2026-09-21.ndjson";

    let verdict = |body: &str| {
        write_receipt(&dir, "2026-09-21", body);
        let state = load_receipt(&dir, date("2026-09-21"));
        verify_file(&state, &data, &cache, HashMode::Inline)
    };

    assert_eq!(
        verdict(&receipt_json("2026-09-21", &[(name, 6, &sha)])),
        FileVerdict::Verified
    );
    assert_eq!(
        verdict(&receipt_json(
            "2026-09-21",
            &[(name, 6, &sha.to_uppercase())]
        )),
        FileVerdict::Verified,
        "hex case is not a mismatch"
    );
    assert_eq!(
        verdict(&receipt_json("2026-09-21", &[(name, 7, &sha)])),
        FileVerdict::SizeMismatch {
            receipt: 7,
            disk: 6
        }
    );
    let wrong = "0".repeat(64);
    assert_eq!(
        verdict(&receipt_json("2026-09-21", &[(name, 6, &wrong)])),
        FileVerdict::HashMismatch
    );
    assert_eq!(
        verdict(&receipt_json("2026-09-21", &[("other.ndjson", 6, &sha)])),
        FileVerdict::NotInReceipt
    );
    for bad in [
        "{".to_string(),
        receipt_json("2026-09-22", &[(name, 6, &sha)]), // wrong date
        receipt_json("2026-09-21", &[]),                // no files
        receipt_json("2026-09-21", &[(name, 6, "abc")]), // short hash
        receipt_json("2026-09-21", &[("../x", 6, &sha)]), // a path
        receipt_json("2026-09-21", &[(name, 6, &sha), (name, 6, &sha)]), // duplicate
        receipt_json("2026-09-21", &[(name, 6, &sha)]).replace("H:/evidence (off-box)", " "),
    ] {
        assert!(
            matches!(verdict(&bad), FileVerdict::MalformedReceipt(_)),
            "{bad} must not verify"
        );
    }
    std::fs::remove_file(
        dir.join(REGISTRY_DIR)
            .join(EXPORTS_DIR)
            .join("2026-09-21.json"),
    )
    .unwrap();
    assert_eq!(
        verify_file(
            &load_receipt(&dir, date("2026-09-21")),
            &data,
            &cache,
            HashMode::Inline
        ),
        FileVerdict::NoReceipt
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The cache must never vouch for bytes it did not hash.
#[test]
fn a_changed_file_is_rehashed_not_served_from_cache() {
    let dir = temp_dir("cache");
    let data = dir.join("a.ndjson");
    std::fs::write(&data, b"one").unwrap();
    let cache = HashCache::default();
    let first = cache.hash(&data).unwrap();
    assert_eq!(cache.cached(&data), Some(first.clone()));
    std::fs::write(&data, b"two!").unwrap(); // length changes, so the key does
    assert_eq!(
        cache.cached(&data),
        None,
        "a changed file has no cached hash"
    );
    assert_ne!(cache.hash(&data).unwrap(), first);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn background_mode_never_hashes_inline_and_fills_the_cache_later() {
    let dir = temp_dir("background");
    let data = dir.join("2026-09-21-run-1.jsonl");
    std::fs::write(&data, b"hello\n").unwrap();
    let sha = sha256_file(&data).unwrap();
    write_receipt(
        &dir,
        "2026-09-21",
        &receipt_json("2026-09-21", &[("2026-09-21-run-1.jsonl", 6, &sha)]),
    );
    let state = load_receipt(&dir, date("2026-09-21"));
    let hasher = BackgroundHasher::new(Arc::new(HashCache::default()));

    assert_eq!(
        verify_file(&state, &data, hasher.cache(), HashMode::Background(&hasher)),
        FileVerdict::Pending,
        "the first look must not block on a hash"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match verify_file(&state, &data, hasher.cache(), HashMode::Background(&hasher)) {
            FileVerdict::Verified => break,
            FileVerdict::Pending if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            other => panic!("background verification did not complete: {other:?}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
