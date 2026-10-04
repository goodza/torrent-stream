use std::process::Command;

#[test]
fn generates_completion_scripts_without_starting_a_session() {
    let root = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = Command::new(env!("CARGO_BIN_EXE_torrent-stream"))
            .args(["--generate-completion", shell])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "{shell}: unexpected diagnostics");
        let script = String::from_utf8(output.stdout).unwrap();
        for flag in [
            "buffer-seconds",
            "buffer-mb",
            "magnet",
            "download-dir",
            "path",
        ] {
            assert!(script.contains(flag), "{shell}: missing {flag}");
        }
        assert!(script.contains("torrent-stream"));
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn rejects_invalid_or_mixed_completion_requests() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["--generate-completion"],
        vec!["--generate-completion", "invalid-shell"],
        vec!["--generate-completion", "zsh", "movie.torrent"],
        vec!["--generate-completion", "bash", "--no-mpv"],
        vec![
            "--generate-completion",
            "fish",
            "--magnet",
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_torrent-stream"))
            .args(&args)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
