use super::*;

#[test]
fn clean_lines_stay_byte_identical_and_dirty_lines_hold_markers() {
    let rig = Rig::new();
    let src = rig.source("s1.jsonl");
    let clean = line(1, "just a normal prompt, with the word token in it");
    let dirty = line(2, &format!("use {} please", fake_github()));
    let escaped = format!(
        "{{\"z\":1,\"a\":{{\"b\":\"line one\\nAWS_SECRET_ACCESS_KEY={}\\nline \\u00e9\"}}}}\n",
        "q".repeat(8) + "Zk3mN9xT"
    );
    fs::write(&src, format!("{clean}{dirty}{escaped}")).unwrap();

    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Appended { .. }
    ));
    let decoded = String::from_utf8(rig.decoded("s1.jsonl")).unwrap();
    let mut lines = decoded.split_inclusive('\n');
    assert_eq!(lines.next().unwrap(), clean);
    let second = lines.next().unwrap();
    assert!(!second.contains(&fake_github()));
    assert!(second.contains("[secret redacted: github_token]"));
    let third = lines.next().unwrap();
    // Key order, the number and the escaped accent survive the rewrite.
    assert!(third.starts_with("{\"z\":1,\"a\":{\"b\":\"line one\\nAWS_SECRET_ACCESS_KEY="));
    assert!(third.contains("[secret redacted: env_secret]"));
    assert!(third.contains('\u{e9}') || third.contains("\\u00e9"));
    assert!(serde_json::from_str::<serde_json::Value>(third).is_ok());
}

#[test]
fn private_spans_are_removed_and_text_lines_are_scrubbed_as_text() {
    let rig = Rig::new();
    let src = rig.source("s2.jsonl");
    let secret = fake_stripe();
    fs::write(
        &src,
        format!(
            "{}not json at all {secret} <PRIVATE>hush</PRIVATE> tail\n",
            line(1, "open <private>the vault code 1234</private> and go")
        ),
    )
    .unwrap();
    rig.mirror().sync_file(&src);
    let decoded = String::from_utf8(rig.decoded("s2.jsonl")).unwrap();
    assert!(!decoded.contains("vault code"));
    assert!(!decoded.contains("hush"));
    assert!(!decoded.contains(&secret));
    assert!(decoded.contains("[private]"));
    assert!(decoded.contains("[secret redacted: stripe_key]"));
}

#[test]
fn three_appends_decode_to_the_one_shot_result() {
    let rig = Rig::new();
    let one_shot = Rig::new();
    let parts: Vec<String> = (0..3)
        .map(|n| line(n, &format!("message {n} with {}", fake_github())))
        .collect();
    let src = rig.source("s3.jsonl");
    for part in &parts {
        append(&src, part.as_bytes());
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Appended { .. }
        ));
    }
    let whole = one_shot.source("s3.jsonl");
    fs::write(&whole, parts.concat()).unwrap();
    one_shot.mirror().sync_file(&whole);

    let row = rig
        .storage
        .mirror_row("-tmp-proj/s3.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(row.frames, 3);
    assert_eq!(rig.decoded("s3.jsonl"), one_shot.decoded("s3.jsonl"));
    assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::UpToDate);
}

#[test]
fn a_partial_last_line_waits_for_the_next_pass() {
    let rig = Rig::new();
    let src = rig.source("s4.jsonl");
    let first = line(1, "complete");
    let second = line(2, "arrives in two writes");
    let (head, tail) = second.split_at(second.len() / 2);
    fs::write(&src, format!("{first}{head}")).unwrap();
    rig.mirror().sync_file(&src);
    assert_eq!(rig.decoded("s4.jsonl"), first.as_bytes());
    let row = rig
        .storage
        .mirror_row("-tmp-proj/s4.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(row.source_bytes, first.len() as u64);

    append(&src, tail.as_bytes());
    rig.mirror().sync_file(&src);
    assert_eq!(
        rig.decoded("s4.jsonl"),
        format!("{first}{second}").as_bytes()
    );
}

#[test]
fn a_truncated_or_rewritten_source_is_mirrored_again_from_scratch() {
    let rig = Rig::new();
    let src = rig.source("s5.jsonl");
    fs::write(&src, format!("{}{}", line(1, "alpha"), line(2, "beta"))).unwrap();
    rig.mirror().sync_file(&src);

    // Shorter than what was mirrored.
    fs::write(&src, line(9, "short")).unwrap();
    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Rewritten { .. }
    ));
    assert_eq!(rig.decoded("s5.jsonl"), line(9, "short").as_bytes());

    // Same prefix length but different bytes: the tail hash catches it.
    let longer = format!(
        "{}{}",
        line(8, "changed"),
        line(7, "and longer than before")
    );
    fs::write(&src, &longer).unwrap();
    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Rewritten { .. }
    ));
    assert_eq!(rig.decoded("s5.jsonl"), longer.as_bytes());
    let row = rig
        .storage
        .mirror_row("-tmp-proj/s5.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(row.frames, 1);
}

#[test]
fn a_crash_between_append_and_state_update_leaves_no_duplicate_lines() {
    let rig = Rig::new();
    let src = rig.source("s6.jsonl");
    let one = line(1, "first");
    let two = line(2, "second");
    fs::write(&src, &one).unwrap();
    rig.mirror().sync_file(&src);
    let saved = rig
        .storage
        .mirror_row("-tmp-proj/s6.jsonl")
        .unwrap()
        .unwrap();

    append(&src, two.as_bytes());
    rig.mirror().sync_file(&src);
    // Roll the state back to before the second append: the frame is on
    // disk, the row does not know.
    rig.storage.mirror_upsert(&saved).unwrap();
    assert!(fs::metadata(rig.zst("s6.jsonl")).unwrap().len() > saved.zst_bytes);

    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Appended { .. }
    ));
    assert_eq!(rig.decoded("s6.jsonl"), format!("{one}{two}").as_bytes());
    let row = rig
        .storage
        .mirror_row("-tmp-proj/s6.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(row.frames, 2);
    assert_eq!(
        row.zst_bytes,
        fs::metadata(rig.zst("s6.jsonl")).unwrap().len()
    );
}

#[test]
fn a_torn_frame_after_the_recorded_end_is_trimmed() {
    let rig = Rig::new();
    let src = rig.source("s6b.jsonl");
    fs::write(&src, line(1, "first")).unwrap();
    rig.mirror().sync_file(&src);
    let before = fs::metadata(rig.zst("s6b.jsonl")).unwrap().len();
    let stray = compress_frame(b"{\"stray\":1}\n").unwrap();
    append(&rig.zst("s6b.jsonl"), &stray[..stray.len() - 3]);

    append(&src, line(2, "second").as_bytes());
    rig.mirror().sync_file(&src);
    let row = rig
        .storage
        .mirror_row("-tmp-proj/s6b.jsonl")
        .unwrap()
        .unwrap();
    assert!(row.zst_bytes > before);
    let decoded = String::from_utf8(rig.decoded("s6b.jsonl")).unwrap();
    assert_eq!(
        decoded,
        format!("{}{}", line(1, "first"), line(2, "second"))
    );
}

#[test]
fn sealing_keeps_the_bytes_and_leaves_one_frame() {
    let rig = Rig::new();
    let src = rig.source("s7.jsonl");
    for n in 0..4 {
        append(&src, line(n, &format!("entry {n}")).as_bytes());
        rig.mirror().sync_file(&src);
    }
    let before = rig.decoded("s7.jsonl");
    let rel = "-tmp-proj/s7.jsonl";
    assert_eq!(rig.storage.mirror_row(rel).unwrap().unwrap().frames, 4);

    assert!(rig.mirror().seal_file(rel).unwrap());
    assert_eq!(rig.decoded("s7.jsonl"), before);
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert_eq!((row.frames, row.sealed), (1, true));
    assert_eq!(
        row.zst_bytes,
        fs::metadata(rig.zst("s7.jsonl")).unwrap().len()
    );
    assert!(!append_name(&rig.zst("s7.jsonl"), ".part").exists());
    assert!(!rig.mirror().seal_file(rel).unwrap());

    // A crash between the state update and the rename is healed too: the
    // finished .part is moved into place by the next sync.
    let mirror = rig.mirror();
    append(&src, line(4, "before the second seal").as_bytes());
    mirror.sync_file(&src);
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert_eq!(row.frames, 2);
    let part = mirror
        .write_sealed_part(&rig.zst("s7.jsonl"), &row)
        .unwrap();
    rig.storage
        .mirror_upsert(&MirrorRow {
            zst_bytes: fs::metadata(&part).unwrap().len(),
            frames: 1,
            sealed: true,
            ..row
        })
        .unwrap();
    let expected = rig.decoded("s7.jsonl");
    append(&src, line(5, "after the seal").as_bytes());
    mirror.sync_file(&src);
    let mut expected = expected;
    expected.extend_from_slice(line(5, "after the seal").as_bytes());
    assert_eq!(rig.decoded("s7.jsonl"), expected);
    assert!(!part.exists());
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert_eq!(row.frames, 2);
}

#[test]
fn the_output_is_plain_zstd_and_big_batches_use_a_long_window() {
    let rig = Rig::new();
    let src = rig.source("s8.jsonl");
    let big = "x".repeat(2 << 20);
    fs::write(&src, line(1, &big)).unwrap();
    rig.mirror().sync_file(&src);
    // The reference decoder (no window cap beyond zstd's own default of
    // 128 MiB) reads the file without any CSR-specific setup.
    let plain = zstd::stream::decode_all(File::open(rig.zst("s8.jsonl")).unwrap()).unwrap();
    assert_eq!(plain, line(1, &big).as_bytes());
    // And the zstd command-line tool, when it is installed.
    if let Ok(output) = std::process::Command::new("zstd")
        .arg("-dc")
        .arg(rig.zst("s8.jsonl"))
        .output()
    {
        assert!(output.status.success());
        assert_eq!(output.stdout, line(1, &big).as_bytes());
    }
}

#[test]
fn directories_are_0700_and_files_0600() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let src = rig.source("s9.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    fs::create_dir_all(mirror_root(&rig.data_dir)).unwrap();
    fs::set_permissions(
        mirror_root(&rig.data_dir),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    rig.mirror().sync_file(&src);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&mirror_root(&rig.data_dir)), 0o700);
    assert_eq!(mode(&transcripts_dir(&rig.data_dir)), 0o700);
    assert_eq!(mode(rig.zst("s9.jsonl").parent().unwrap()), 0o700);
    assert_eq!(mode(&rig.zst("s9.jsonl")), 0o600);
}

#[test]
fn the_kill_switch_writes_nothing() {
    let rig = Rig::new();
    let src = rig.source("s10.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    let mut mirror = rig.mirror();
    mirror.disabled = true;
    assert_eq!(mirror.sync_file(&src), SyncOutcome::Disabled);
    assert_eq!(mirror.sync_pass(None), PassStats::default());
    assert!(!mirror_root(&rig.data_dir).exists());
    assert_eq!(rig.storage.mirror_totals().unwrap().files, 0);
}

#[test]
fn an_unwritable_mirror_counts_an_error_and_returns_normally() {
    let rig = Rig::new();
    let src = rig.source("s11.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    // A plain file where the mirror directory belongs.
    fs::write(mirror_root(&rig.data_dir), b"in the way").unwrap();
    assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::Failed);
    assert_eq!(
        rig.storage
            .get_meta_counter(MIRROR_ERRORS_KEY)
            .unwrap()
            .unwrap(),
        1
    );
    let last = rig.storage.get_meta(MIRROR_LAST_ERROR_KEY).unwrap();
    assert!(last.is_some_and(|text| !text.is_empty()));
}

#[test]
fn a_mirror_under_the_projects_dir_is_refused() {
    let rig = Rig::new();
    let src = rig.source("s12.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    // data_dir == projects_dir is what `Engine::from_parts` builds in tests.
    let mirror = Mirror::new(&rig.storage, &rig.projects, &rig.projects);
    assert_eq!(mirror.sync_file(&src), SyncOutcome::Failed);
    assert!(!rig.projects.join("mirror").exists());
}

#[test]
fn a_pass_backfills_subagents_flags_gone_sources_and_keeps_their_mirror() {
    let rig = Rig::new();
    let main = rig.source("sess.jsonl");
    fs::write(&main, line(1, &format!("key {}", fake_github()))).unwrap();
    let sub_dir = rig.projects.join("-tmp-proj/sess/subagents");
    fs::create_dir_all(&sub_dir).unwrap();
    fs::write(sub_dir.join("agent-a1.jsonl"), line(2, "sub")).unwrap();
    fs::write(rig.source("empty.jsonl"), b"").unwrap();

    let stats = rig.mirror().sync_pass(None);
    assert_eq!((stats.files, stats.synced, stats.errors), (3, 2, 0));
    assert!(rig
        .zst("sess/subagents/agent-a1.jsonl")
        .parent()
        .unwrap()
        .exists());
    let totals = rig.storage.mirror_totals().unwrap();
    assert_eq!((totals.files, totals.sources_gone), (2, 0));
    assert_eq!(rig.mirror().sync_pass(None).synced, 0);

    fs::remove_file(&main).unwrap();
    let stats = rig.mirror().sync_pass(None);
    assert_eq!(stats.sources_gone, 1);
    let row = rig
        .storage
        .mirror_row("-tmp-proj/sess.jsonl")
        .unwrap()
        .unwrap();
    assert!(row.source_gone_at.is_some());
    let decoded = String::from_utf8(rig.decoded("sess.jsonl")).unwrap();
    assert!(decoded.contains("[secret redacted: github_token]"));
    assert_eq!(rig.storage.mirror_totals().unwrap().sources_gone, 1);
    assert!(rig.storage.get_meta("mirror_last_sync").unwrap().is_some());
}

#[test]
fn a_pass_seals_quiet_multi_frame_transcripts_only() {
    let rig = Rig::new();
    let quiet = rig.source("quiet.jsonl");
    let busy = rig.source("busy.jsonl");
    for n in 0..3 {
        append(&quiet, line(n, "q").as_bytes());
        append(&busy, line(n, "b").as_bytes());
        rig.mirror().sync_file(&quiet);
        rig.mirror().sync_file(&busy);
    }
    // Nothing is old enough yet.
    assert_eq!(rig.mirror().sync_pass(None).sealed, 0);
    let mut mirror = rig.mirror();
    mirror.seal_after = Duration::ZERO;
    assert_eq!(mirror.sync_pass(None).sealed, 2);
    assert_eq!(rig.storage.mirror_totals().unwrap().sealed, 2);
}

#[test]
fn purge_dry_run_deletes_nothing_and_yes_deletes() {
    let rig = Rig::new();
    let src = rig.source("p.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    rig.mirror().sync_file(&src);

    let dry = rig.mirror().purge(false).unwrap();
    assert_eq!((dry.files, dry.deleted), (1, false));
    assert!(dry.zst_bytes > 0);
    assert!(rig.zst("p.jsonl").exists());
    assert_eq!(rig.storage.mirror_totals().unwrap().files, 1);

    // The legacy copy beside the new tree is left alone.
    let legacy = mirror_root(&rig.data_dir).join("projects/keep.jsonl");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, b"old").unwrap();
    assert!(rig.mirror().purge(true).unwrap().deleted);
    assert!(!transcripts_dir(&rig.data_dir).exists());
    assert!(legacy.exists());
    assert_eq!(rig.storage.mirror_totals().unwrap().files, 0);
}

#[test]
fn a_decode_error_ends_the_stream_instead_of_looping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.zst");
    let mut frame = compress_frame(&b"{\"a\":1}\n".repeat(1000)).unwrap();
    let cut = frame.len() / 2;
    frame.truncate(cut);
    fs::write(&path, &frame).unwrap();
    let reader = open_transcript(&path).unwrap();
    assert!(reader.lines().take(100_000).count() < 100_000);
}

// ─── U3: fix round after review ───

#[test]
fn a_stale_pass_snapshot_never_overwrites_a_sealed_mirror_of_a_gone_source() {
    let rig = Rig::new();
    let src = rig.source("gone.jsonl");
    let rel = "-tmp-proj/gone.jsonl";
    for n in 0..3 {
        append(&src, line(n, &format!("entry {n}")).as_bytes());
        rig.mirror().sync_file(&src);
    }
    let before = rig.decoded("gone.jsonl");
    assert_eq!(rig.storage.mirror_row(rel).unwrap().unwrap().frames, 3);

    // The pass reads its snapshot (three frames). Before it gets to the
    // gone-source step, another process seals the mirror and the source
    // disappears.
    let mirror = rig.mirror();
    {
        let src = src.clone();
        let rig = &rig;
        mirror.after_snapshot.replace(Some(Box::new(move || {
            assert!(rig.mirror().seal_file(rel).unwrap());
            fs::remove_file(&src).unwrap();
        })));
    }
    let mut mirror = mirror;
    mirror.seal_after = Duration::ZERO;
    let stats = mirror.sync_pass(None);

    assert_eq!(stats.sources_gone, 1);
    assert_eq!(stats.errors, 0);
    assert_eq!(rig.decoded("gone.jsonl"), before);
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert!(row.source_gone_at.is_some());
    assert_eq!((row.frames, row.sealed), (1, true));
    assert_eq!(
        row.zst_bytes,
        fs::metadata(rig.zst("gone.jsonl")).unwrap().len()
    );
}

#[test]
fn a_same_size_rewrite_of_the_source_is_caught_before_up_to_date() {
    let rig = Rig::new();
    let src = rig.source("same.jsonl");
    let first = format!("{}{}", line(1, "alpha alpha"), line(2, "beta beta"));
    fs::write(&src, &first).unwrap();
    rig.mirror().sync_file(&src);

    let second = first.replace("alpha", "gamma");
    assert_eq!(second.len(), first.len());
    fs::write(&src, &second).unwrap();
    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Rewritten { .. }
    ));
    assert_eq!(rig.decoded("same.jsonl"), second.as_bytes());

    // The daemon pass finds it too.
    let third = second.replace("beta", "delt");
    fs::write(&src, &third).unwrap();
    assert_eq!(rig.mirror().sync_pass(None).synced, 1);
    assert_eq!(rig.decoded("same.jsonl"), third.as_bytes());
}

#[test]
fn a_missing_or_truncated_mirror_file_is_rebuilt_while_the_source_is_unchanged() {
    let rig = Rig::new();
    let src = rig.source("dmg.jsonl");
    let text = format!("{}{}", line(1, "one"), line(2, "two"));
    fs::write(&src, &text).unwrap();
    rig.mirror().sync_file(&src);

    let zst = rig.zst("dmg.jsonl");
    let full = fs::read(&zst).unwrap();
    fs::write(&zst, &full[..full.len() - 5]).unwrap();
    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Rewritten { .. }
    ));
    assert_eq!(rig.decoded("dmg.jsonl"), text.as_bytes());

    fs::remove_file(&zst).unwrap();
    assert_eq!(rig.mirror().sync_pass(None).synced, 1);
    assert_eq!(rig.decoded("dmg.jsonl"), text.as_bytes());
}

#[test]
fn a_partial_last_line_is_never_flushed_even_when_the_file_is_idle() {
    let rig = Rig::new();
    let src = rig.source("part.jsonl");
    let first = line(1, "complete");
    let github = fake_github();
    // The writer stopped inside a credential: too short to match yet.
    let whole = line(2, &format!("token {github} end"));
    let cut = whole.find("ghp_").unwrap() + 10;
    fs::write(&src, format!("{first}{}", &whole[..cut])).unwrap();

    let mut mirror = rig.mirror();
    mirror.seal_after = Duration::ZERO;
    mirror.sync_file(&src);
    assert_eq!(rig.decoded("part.jsonl"), first.as_bytes());
    let rel = "-tmp-proj/part.jsonl";
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert_eq!(row.source_bytes, first.len() as u64);

    // The writer resumes: the finished line is scrubbed as a whole.
    append(&src, &whole.as_bytes()[cut..]);
    mirror.sync_file(&src);
    let decoded = String::from_utf8(rig.decoded("part.jsonl")).unwrap();
    assert!(!decoded.contains("ghp_"));
    assert!(decoded.contains("[secret redacted: github_token]"));
    assert_eq!(decoded.lines().count(), 2);
}

#[test]
fn json_unicode_escapes_cannot_hide_a_secret_or_a_private_span() {
    let rig = Rig::new();
    let src = rig.source("esc.jsonl");
    let body = "A".repeat(36);
    let secret_line =
        format!("{{\"type\":\"user\",\"message\":{{\"content\":\"key \\u0067hp_{body} end\"}}}}\n");
    let private_line = "{\"type\":\"user\",\"message\":{\"content\":\"a \\u003cprivate>hush hush\\u003c/private> b\"}}\n";
    let clean_escape =
        "{\"type\":\"user\",\"message\":{\"content\":\"caf\\u00e9 \\u001b[0m ok\"}}\n";
    fs::write(&src, format!("{secret_line}{private_line}{clean_escape}")).unwrap();
    rig.mirror().sync_file(&src);

    let decoded = String::from_utf8(rig.decoded("esc.jsonl")).unwrap();
    let mut lines = decoded.split_inclusive('\n');
    let first = lines.next().unwrap();
    assert!(!first.contains(&body), "{first}");
    assert!(first.contains("[secret redacted: github_token]"));
    let second = lines.next().unwrap();
    assert!(!second.contains("hush"), "{second}");
    // An escape that cannot spell a pattern leaves the line byte-identical.
    assert_eq!(lines.next().unwrap(), clean_escape);
}

#[test]
fn object_keys_are_scrubbed_in_the_mirror() {
    let rig = Rig::new();
    let src = rig.source("keys.jsonl");
    let other = format!("{}{}", "ghp_", "B".repeat(36));
    let text = format!(
        "{{\"input\":{{\"{}\":\"acct-1\",\"{other}\":\"acct-2\",\"plain\":1}}}}\n",
        fake_github()
    );
    fs::write(&src, &text).unwrap();
    rig.mirror().sync_file(&src);
    let decoded = String::from_utf8(rig.decoded("keys.jsonl")).unwrap();
    assert!(!decoded.contains("ghp_"), "{decoded}");
    let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
    let object = value["input"].as_object().unwrap();
    // Two secrets collapse to one marker; neither value is lost.
    assert_eq!(object.len(), 3, "{decoded}");
    assert!(object.values().any(|v| v == "acct-1"));
    assert!(object.values().any(|v| v == "acct-2"));
    assert_eq!(object["plain"], 1);
}

#[test]
fn a_source_replaced_while_waiting_for_the_locks_does_not_clobber_the_newer_mirror() {
    let rig = Rig::new();
    let src = rig.source("swap.jsonl");
    let rel = "-tmp-proj/swap.jsonl";
    append(&src, line(1, "old one").as_bytes());
    rig.mirror().sync_file(&src);
    append(&src, line(2, "old two").as_bytes());

    // This call opens the old inode. Before it takes any lock, the source is
    // replaced and another process mirrors the replacement.
    let replacement = format!(
        "{}{}{}",
        line(7, "new seven"),
        line(8, "new eight"),
        line(9, "new nine")
    );
    let mirror = rig.mirror();
    {
        let src = src.clone();
        let replacement = replacement.clone();
        let rig = &rig;
        mirror.after_source_open.replace(Some(Box::new(move || {
            let staged = src.with_extension("new");
            fs::write(&staged, &replacement).unwrap();
            fs::rename(&staged, &src).unwrap();
            assert!(matches!(
                rig.mirror().sync_file(&src),
                SyncOutcome::Rewritten { .. }
            ));
        })));
    }
    mirror.sync_file(&src);
    // The replacement is later deleted: the mirror is its only copy.
    fs::remove_file(&src).unwrap();

    assert_eq!(rig.decoded("swap.jsonl"), replacement.as_bytes());
    let row = rig.storage.mirror_row(rel).unwrap().unwrap();
    assert_eq!(row.source_bytes, replacement.len() as u64);
}

#[test]
fn a_renamed_key_never_lands_on_a_key_that_was_already_there() {
    let rig = Rig::new();
    let src = rig.source("collide.jsonl");
    let aws = format!("AKIA{}", "B".repeat(16));
    let marker = "[secret redacted: aws_access_key]";
    let text = format!(
        "{{\"input\":{{\"{aws}\":\"credential\",\"{marker}\":\"existing\",\"z\":1}},\"other\":{{\"{aws}\":\"alone\"}}}}\n"
    );
    fs::write(&src, &text).unwrap();
    rig.mirror().sync_file(&src);
    let decoded = String::from_utf8(rig.decoded("collide.jsonl")).unwrap();
    assert!(!decoded.contains("AKIA"), "{decoded}");
    let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
    let input = value["input"].as_object().unwrap();
    assert_eq!(input.len(), 3, "{decoded}");
    assert_eq!(input[marker], "existing");
    assert!(input.values().any(|v| v == "credential"));
    // A different object is its own namespace: no suffix needed there.
    assert_eq!(value["other"][marker], "alone");
}

#[test]
fn purge_waits_for_a_running_sync_and_a_sync_waits_for_purge() {
    let rig = Rig::new();
    let src = rig.source("lk.jsonl");
    fs::write(&src, line(1, "x")).unwrap();
    rig.mirror().sync_file(&src);

    let lock_path = mirror_root(&rig.data_dir).join(".mirror.lock");
    let open = || {
        private_file_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap()
    };
    // A sync in flight holds the shared lock: purge must not delete under it.
    let syncing = open();
    syncing.lock_shared().unwrap();
    assert!(rig.mirror().purge(true).is_err());
    assert!(rig.zst("lk.jsonl").exists());
    assert_eq!(rig.storage.mirror_totals().unwrap().files, 1);
    FileExt::unlock(&syncing).unwrap();

    // A purge in flight holds it exclusively: a sync backs off.
    let purging = open();
    purging.lock_exclusive().unwrap();
    append(&src, line(2, "y").as_bytes());
    assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::Busy);
    FileExt::unlock(&purging).unwrap();
    assert!(matches!(
        rig.mirror().sync_file(&src),
        SyncOutcome::Appended { .. }
    ));
}

#[test]
fn a_line_over_the_cap_stops_the_file_and_is_never_mirrored() {
    let rig = Rig::new();
    let src = rig.source("huge.jsonl");
    let first = line(1, "small and fine");
    let github = fake_github();
    let mut huge = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{github} ");
    huge.push_str(&"y".repeat(BATCH_MAX_BYTES));
    huge.push_str("\"}}\n");
    let after = line(3, "after the huge line");
    fs::write(&src, format!("{first}{huge}{after}")).unwrap();

    rig.mirror().sync_file(&src);
    let decoded = rig.decoded("huge.jsonl");
    assert!(
        decoded == first.as_bytes(),
        "mirror holds {} bytes",
        decoded.len()
    );
    let row = rig
        .storage
        .mirror_row("-tmp-proj/huge.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(row.source_bytes, first.len() as u64);
    assert_eq!(
        rig.storage
            .get_meta_counter(MIRROR_ERRORS_KEY)
            .unwrap()
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn import_mirrors_the_file_and_csr_transcript_reads_it_after_the_source_is_deleted() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-aaa.jsonl");
    let secret = fake_github();
    write_session(&src, &secret);

    assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
    let row = engine
        .storage()
        .mirror_row("-tmp-proj/sess-aaa.jsonl")
        .unwrap()
        .expect("import mirrored the transcript");
    assert_eq!(row.source_bytes, fs::metadata(&src).unwrap().len());

    // Already imported and mirrored: the second call is a cheap no-op.
    assert_eq!(engine.import_file(&src, "proj").await.unwrap(), 0);
    assert_eq!(engine.mirror().sync_file(&src), SyncOutcome::UpToDate);

    // Imported but never mirrored (as on the first run after upgrade): the
    // early "already imported" return must not skip the mirror.
    engine.storage().mirror_clear().unwrap();
    fs::remove_dir_all(transcripts_dir(&rig.data_dir)).unwrap();
    assert_eq!(engine.import_file(&src, "proj").await.unwrap(), 0);
    assert!(rig.zst("sess-aaa.jsonl").exists());

    // Claude Code deletes the JSONL.
    fs::remove_file(&src).unwrap();
    let request = |view| crate::transcript::TranscriptRequest {
        session: "sess-aaa".to_string(),
        view,
        ..Default::default()
    };
    let without =
        crate::transcript::run(&rig.projects, &request(crate::transcript::ViewKind::Stats));
    assert!(without.contains("not found"), "got: {without}");

    let stats = crate::transcript::run_with_mirror(
        &rig.projects,
        Some(&rig.data_dir),
        &request(crate::transcript::ViewKind::Stats),
    );
    assert!(stats.contains("transcript mirror"), "got: {stats}");
    assert!(stats.contains("<turns total=\"2\""), "got: {stats}");
    let prompts = crate::transcript::run_with_mirror(
        &rig.projects,
        Some(&rig.data_dir),
        &request(crate::transcript::ViewKind::Prompts),
    );
    assert!(
        prompts.contains("[secret redacted: github_token]"),
        "got: {prompts}"
    );
    assert!(!prompts.contains(&secret));
    assert!(prompts.contains("source: transcript mirror"));

    let pointer = crate::mcp::tools::get_full_conversation(
        &rig.projects,
        Some(&rig.data_dir),
        "sess-aaa",
        None,
    );
    assert!(pointer.contains("<mirror_path>"), "got: {pointer}");
    assert!(pointer.contains(&rig.zst("sess-aaa.jsonl").to_string_lossy().to_string()));
    assert!(pointer.contains("csr_transcript(session=\"sess-aaa\""));
    // Without a data dir the old not-found answer is unchanged.
    assert!(
        crate::mcp::tools::get_full_conversation(&rig.projects, None, "sess-aaa", None)
            .contains("not found")
    );
}

#[tokio::test]
async fn an_unwritable_mirror_does_not_fail_the_import() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    fs::write(mirror_root(&rig.data_dir), b"in the way").unwrap();
    let src = rig.source("sess-bbb.jsonl");
    write_session(&src, &fake_stripe());

    assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
    assert_eq!(
        engine
            .storage()
            .get_meta_counter(MIRROR_ERRORS_KEY)
            .unwrap()
            .unwrap(),
        1
    );
    assert!(engine
        .storage()
        .mirror_row("-tmp-proj/sess-bbb.jsonl")
        .unwrap()
        .is_none());
}
