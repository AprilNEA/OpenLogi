use super::*;

fn announce_of(formats: Vec<proto::ClipboardFormat>) -> proto::ClipboardAnnounce {
    proto::ClipboardAnnounce {
        sequence: 9,
        formats,
        ..Default::default()
    }
}

fn format(mime: &str, size_bytes: u64, sha256: Option<Vec<u8>>) -> proto::ClipboardFormat {
    proto::ClipboardFormat {
        mime: mime.to_owned(),
        size_bytes,
        sha256,
        ..Default::default()
    }
}

fn offered(relative_path: &str, size_bytes: u64) -> OfferedFile {
    OfferedFile {
        relative_path: relative_path.to_owned(),
        size_bytes,
        modified_at_ms: 1_700_000_000_000,
        source: PathBuf::from(relative_path),
    }
}

fn entry(relative_path: &str, size_bytes: u64) -> proto::FileEntry {
    proto::FileEntry {
        relative_path: relative_path.to_owned(),
        size_bytes,
        ..Default::default()
    }
}

fn list_of(entries: Vec<proto::FileEntry>) -> proto::FileList {
    proto::FileList {
        files: entries,
        ..Default::default()
    }
}

#[test]
fn an_offer_announces_its_text_by_size_and_digest() {
    let offer = TextOffer::new(3, "héllo".to_owned()).unwrap();
    let announce = offer.announce();

    assert_eq!(announce.sequence, 3);
    assert_eq!(announce.formats.len(), 1);
    assert_eq!(announce.formats[0].mime, TEXT_MIME);
    assert_eq!(announce.formats[0].size_bytes, 6);
    assert_eq!(
        Announced::from_announce(&announce),
        Announced::Text(AnnouncedText {
            sequence: 3,
            size: 6,
            digest: Some(ContentDigest::of("héllo")),
        })
    );
}

#[test]
fn empty_or_oversized_text_is_never_offered() {
    assert_eq!(
        TextOffer::new(1, String::new()).unwrap_err(),
        OfferError::Empty
    );
    assert_eq!(
        TextOffer::new(1, "a".repeat(MAX_TEXT_BYTES + 1)).unwrap_err(),
        OfferError::TooLarge
    );
    assert_eq!(
        TextOffer::new(1, "a".repeat(MAX_TEXT_BYTES))
            .unwrap()
            .size_bytes(),
        MAX_TEXT_BYTES
    );
}

#[test]
fn announces_without_fetchable_text_are_classified() {
    let over = wire_size(MAX_TEXT_BYTES) + 1;
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format(TEXT_MIME, over, None)])),
        Announced::TooLarge
    );
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format("image/png", 10, None)])),
        Announced::Nothing
    );
    assert_eq!(
        Announced::from_announce(&announce_of(Vec::new())),
        Announced::Nothing
    );
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format(TEXT_MIME, 0, None)])),
        Announced::Nothing
    );
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format(TEXT_MIME, 4, Some(vec![0; 31]))])),
        Announced::Nothing
    );
}

#[test]
fn the_text_representation_matches_media_type_spelling_variants() {
    for mime in [
        TEXT_MIME,
        "text/plain; charset=utf-8",
        "Text/Plain;Charset=UTF-8",
    ] {
        assert!(is_text_mime(mime), "{mime} names UTF-8 text");
    }
    for mime in ["text/plain", "text/plain;charset=utf-16", "text/html"] {
        assert!(!is_text_mime(mime), "{mime} is not the UTF-8 text mime");
    }
    assert_eq!(
        Announced::from_announce(&announce_of(vec![
            format("image/png", 10, None),
            format("text/plain; charset=UTF-8", 4, None),
        ])),
        Announced::Text(AnnouncedText {
            sequence: 9,
            size: 4,
            digest: None,
        })
    );
}

#[test]
fn a_full_chunk_fits_the_chunk_frame_cap() {
    let chunk = chunk_envelope(&vec![0xa5; CHUNK_DATA_LEN]).unwrap();
    assert_eq!(chunk.payload.len(), MAX_CHUNK_LEN);
    assert!(
        chunk_envelope(&vec![0xa5; CHUNK_DATA_LEN + 1]).is_err(),
        "one more byte must overflow the chunk cap"
    );
}

#[test]
fn debug_output_never_carries_the_text() {
    let offer = TextOffer::new(5, "hunter2".to_owned()).unwrap();
    let printed = format!("{offer:?} {:?}", offer.digest());
    assert!(!printed.contains("hunter2"));
    assert!(printed.contains("size_bytes: 7"));
    assert!(printed.contains("ContentDigest(..)"));
}

#[test]
fn files_are_announced_as_a_file_list() {
    let offer = FilesOffer::new(
        4,
        vec![offered("report.pdf", 1200), offered("photos/a.jpg", 30)],
    )
    .unwrap();
    assert_eq!(offer.file_count(), 2);
    assert_eq!(offer.total_bytes(), 1230);

    let announce = ClipboardOffer::Files(offer).announce();
    assert_eq!(announce.sequence, 4);
    assert_eq!(announce.formats.len(), 1);
    assert_eq!(announce.formats[0].mime, FILES_MIME);
    let Announced::Files(announced) = Announced::from_announce(&announce) else {
        panic!("a file list announce reads as files");
    };
    assert_eq!(announced.sequence(), 4);
}

#[test]
fn the_same_files_have_the_same_digest_wherever_they_sit() {
    let here = FilesOffer::new(1, vec![offered("a.txt", 3)]).unwrap();
    let mut elsewhere = offered("a.txt", 3);
    elsewhere.source = PathBuf::from("elsewhere/a.txt");
    let there = FilesOffer::new(7, vec![elsewhere]).unwrap();
    assert_eq!(here.digest(), there.digest());

    let changed = FilesOffer::new(1, vec![offered("a.txt", 4)]).unwrap();
    assert_ne!(here.digest(), changed.digest());
}

#[test]
fn files_outside_the_limits_are_never_offered() {
    assert_eq!(
        FilesOffer::new(1, Vec::new()).unwrap_err(),
        OfferError::NoFiles
    );
    let too_many = (0..=MAX_FILES)
        .map(|index| offered(&format!("{index}.txt"), 1))
        .collect();
    assert_eq!(
        FilesOffer::new(1, too_many).unwrap_err(),
        OfferError::FilesTooLarge
    );
    assert_eq!(
        FilesOffer::new(
            1,
            vec![offered("a.bin", MAX_FILES_BYTES), offered("b.bin", 1)]
        )
        .unwrap_err(),
        OfferError::FilesTooLarge
    );
    assert_eq!(
        FilesOffer::new(1, vec![offered("../escape.txt", 1)]).unwrap_err(),
        OfferError::UnsafePath
    );
}

#[test]
fn a_file_list_announce_prefers_files_and_bounds_the_list() {
    assert!(matches!(
        Announced::from_announce(&announce_of(vec![
            format(TEXT_MIME, 4, None),
            format(FILES_MIME, 40, None),
        ])),
        Announced::Files(_)
    ));
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format(
            FILES_MIME,
            wire_size(MAX_FILE_LIST_BYTES) + 1,
            None
        )])),
        Announced::TooLarge
    );
    assert_eq!(
        Announced::from_announce(&announce_of(vec![format(FILES_MIME, 0, None)])),
        Announced::Nothing
    );
}

#[test]
fn safe_relative_paths_stay_under_the_receivers_folder() {
    for path in [
        "a.txt",
        "photos/2024/IMG 0001.jpg",
        "notes.v2.md",
        "com10.txt",
        "lpt0",
        "consent.txt",
        "über/naïve – €.txt",
        // Four bytes with no char boundary at 3, where the `COM1` check looks.
        "a€.txt",
        "Cé1.txt",
    ] {
        assert!(is_safe_relative_path(path), "{path} is safe");
    }
    for path in [
        "",
        "/etc/passwd",
        "../up.txt",
        "a/../../up.txt",
        "a//b.txt",
        "./a.txt",
        "C:/Windows/win.ini",
        "C:evil.txt",
        "a\\b.txt",
        "trailing.",
        "trailing ",
        "file:stream",
        "what?.txt",
        "CON",
        "nul.txt",
        "Com1.log",
        "lpt9",
        "aux/inside.txt",
        "tab\tname",
    ] {
        assert!(!is_safe_relative_path(path), "{path:?} is unsafe");
    }
    assert!(!is_safe_relative_path(&"a".repeat(256)));
    assert!(!is_safe_relative_path(
        &"abcdefgh/".repeat(MAX_RELATIVE_PATH_BYTES / 9 + 1)
    ));
}

#[test]
fn a_file_list_is_accepted_only_when_every_file_can_be_created() {
    let listed = validate_file_list(list_of(vec![
        entry("a.txt", 3),
        entry("photos/b.jpg", 5),
        entry("photos/c.jpg", 7),
    ]))
    .unwrap();
    let files = listed.files();
    assert_eq!(files.len(), 3);
    assert_eq!(files[1].relative_path(), "photos/b.jpg");
    assert_eq!(files[2].size_bytes(), 7);
    assert!(listed.folders().is_empty());

    for (entries, why) in [
        (Vec::new(), "an empty list"),
        (vec![entry("../x", 1)], "an escaping path"),
        (
            vec![entry("a.txt", 1), entry("A.TXT", 1)],
            "the same file twice, ignoring case",
        ),
        (
            vec![entry("photos", 1), entry("photos/b.jpg", 1)],
            "a file where a folder must go",
        ),
        (
            vec![entry("a.bin", MAX_FILES_BYTES), entry("b.bin", 1)],
            "more bytes than the limit",
        ),
        (
            vec![entry("a.bin", u64::MAX), entry("b.bin", 2)],
            "sizes that overflow",
        ),
    ] {
        validate_file_list(list_of(entries)).expect_err(why);
    }
    let too_many = (0..=MAX_FILES)
        .map(|index| entry(&format!("{index}.txt"), 1))
        .collect();
    validate_file_list(list_of(too_many)).expect_err("more files than the limit");
}

#[test]
fn empty_folders_travel_with_the_files() {
    let offer = FilesOffer::with_folders(
        1,
        vec![offered("photos/a.jpg", 3)],
        vec!["photos/empty".to_owned(), "drafts".to_owned()],
    )
    .unwrap();
    let only_files = FilesOffer::new(1, vec![offered("photos/a.jpg", 3)]).unwrap();
    assert_ne!(
        offer.digest(),
        only_files.digest(),
        "a copy with an empty folder is not the same copy"
    );
    FilesOffer::with_folders(1, Vec::new(), vec!["empty".to_owned()])
        .expect("an empty folder alone is a copy");
    assert_eq!(
        FilesOffer::with_folders(
            1,
            vec![offered("photos/a.jpg", 3)],
            vec!["photos".to_owned()]
        )
        .unwrap_err(),
        OfferError::UnsafePath,
        "a folder listed empty holds a file"
    );

    let listed = validate_file_list(proto::FileList {
        files: vec![entry("photos/a.jpg", 3)],
        folders: vec!["photos/empty".to_owned()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(listed.folders()[0].relative_path(), "photos/empty");
    validate_file_list(proto::FileList {
        folders: vec!["../escape".to_owned()],
        ..Default::default()
    })
    .expect_err("an escaping folder");
}

#[test]
fn debug_output_never_carries_file_names() {
    let file = offered("secret-plans.docx", 10);
    let offer = FilesOffer::new(2, vec![file.clone()]).unwrap();
    let listed = validate_file_list(list_of(vec![entry("secret-plans.docx", 10)])).unwrap();
    let printed = format!("{file:?} {offer:?} {listed:?}");
    assert!(!printed.contains("secret"));
    assert!(printed.contains("size_bytes: 10"));
}
