use std::{fs::OpenOptions, sync::mpsc, thread, time::Duration};

#[test]
fn bundle_identity_rejects_fifo_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Synthetic.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    let info = app.join("Contents/Info.plist");
    crate::file_input::tests::create_fifo(&info);
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        tx.send(super::bundle_identifier(&app)).unwrap();
    });
    let answer = rx.recv_timeout(Duration::from_secs(1));
    if matches!(answer, Err(mpsc::RecvTimeoutError::Timeout)) {
        drop(OpenOptions::new().write(true).open(&info).unwrap());
    }
    reader.join().unwrap();
    assert_eq!(
        answer.expect("bundle identity read blocked on a FIFO"),
        None
    );
}

#[test]
fn bundle_identity_reads_xml_and_binary_but_rejects_oversized_or_special_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let app = directory.path().join("Synthetic.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    let info = app.join("Contents/Info.plist");
    let xml = br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.synthetic</string></dict></plist>"#;
    std::fs::write(&info, xml).unwrap();
    assert_eq!(
        super::bundle_identifier(&app).as_deref(),
        Some("com.example.synthetic")
    );
    let value = plist::Value::from_reader(std::io::Cursor::new(xml)).unwrap();
    value.to_file_binary(&info).unwrap();
    assert_eq!(
        super::bundle_identifier(&app).as_deref(),
        Some("com.example.synthetic")
    );
    OpenOptions::new()
        .write(true)
        .open(&info)
        .unwrap()
        .set_len(1024 * 1024 + 1)
        .unwrap();
    assert_eq!(super::bundle_identifier(&app), None);
    std::fs::remove_file(&info).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &info).unwrap();
    assert_eq!(super::bundle_identifier(&app), None);
    std::fs::remove_file(&info).unwrap();
    std::fs::create_dir(&info).unwrap();
    assert_eq!(super::bundle_identifier(&app), None);
}
