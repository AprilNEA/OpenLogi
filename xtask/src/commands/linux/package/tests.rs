use super::PACKAGED_BINS;

/// `nfpm.yaml` decides what actually ships, so a binary listed there but
/// missing from [`PACKAGED_BINS`] is never built — the drift that shipped
/// releases whose overlay came from a stale build cache rather than source.
#[test]
fn packaged_bins_cover_every_binary_nfpm_installs() {
    let config = include_str!("../../../../../packaging/linux/nfpm.yaml");
    let mut installed: Vec<&str> = config
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- src: target/release/"))
        .collect();
    installed.sort_unstable();

    let mut built = PACKAGED_BINS;
    built.sort_unstable();

    assert_eq!(
        installed, built,
        "packaging/linux/nfpm.yaml and PACKAGED_BINS disagree on which binaries ship"
    );
}
